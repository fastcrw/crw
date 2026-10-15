//! LLM provider dispatch (Anthropic, OpenAI Chat Completions, OpenAI
//! Responses, OpenAI-compatible, Azure).
//!
//! Two surfaces:
//!
//! * [`extract_via_llm`] — content-extraction fallback used when DOM-based
//!   extraction (readability + heuristics) yields a low-quality candidate.
//! * [`chat`] — generic single-turn chat call used by [`crate::summary`]
//!   and [`crate::answer`] for user-facing LLM features.
//!
//! All paths share one pooled [`reqwest::Client`] (per-call clients leak
//! TCP connections under load).

use crate::pricing;
use crw_core::config::LlmConfig;
use crw_core::error::{CrwError, CrwResult};
use crw_core::types::LlmUsage;
use rand::RngExt;
use std::sync::OnceLock;
use std::time::Duration;

/// Provider tags [`dispatch`] recognises — the single source of truth for the
/// `llm.providers` field of `GET /v1/capabilities`. Anything not listed here is
/// rejected up-front, so the advertised list and the accepted list cannot drift.
pub const SUPPORTED_PROVIDERS: &[&str] = &[
    "anthropic",
    "openai",
    "deepseek",
    "openai-compatible",
    "openai-responses",
    "azure",
];

/// Whether [`dispatch`] can route to this provider tag (case-insensitive).
pub fn is_supported_provider(provider: &str) -> bool {
    let p = provider.to_ascii_lowercase();
    SUPPORTED_PROVIDERS.contains(&p.as_str())
}

fn unknown_provider_error(provider: &str) -> CrwError {
    CrwError::InvalidRequest(format!(
        "unknown LLM provider: {provider}. Supported: {}",
        SUPPORTED_PROVIDERS.join(", ")
    ))
}

const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1/messages";
const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1/chat/completions";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

const EXTRACTION_SYSTEM_PROMPT: &str = "You extract the main article or content body from a web page's HTML. \
Return only the article text as plain markdown. Strip nav, header, footer, ads, comments, related links, \
sidebars, cookie banners, share buttons, author bios, social widgets. No preamble or commentary, no fenced \
code block — just the markdown content.";

/// Result of one LLM call: textual content + best-effort usage metadata.
#[derive(Debug, Clone)]
pub struct LlmCallResult {
    pub content: String,
    pub usage: Option<LlmUsage>,
    pub warning: Option<String>,
}

/// The crate's pooled LLM client. Shared with [`crate::responses`] so the
/// Responses transport does not open a second connection pool; callers that
/// need a different budget set a per-request `.timeout(..)`, which overrides
/// the builder default below.
pub(crate) fn shared_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client build (LLM shared)")
    })
}

/// hyper's wording for a pooled keep-alive connection the peer had already
/// closed. Matched on the message because hyper is not a direct dependency of
/// this crate; `crates/crw-extract/tests/transport_retry.rs` drives the real
/// failure end to end, so a hyper rewording fails a test rather than silently
/// disabling the retry below.
const DEAD_POOLED_CONNECTION: &str = "connection closed before message completed";

/// Send a provider POST, retrying once when the connection died before the
/// provider could have seen the request.
///
/// Every LLM call here rides a process-wide pooled client, and a provider (or
/// any middlebox between us) may close an idle keep-alive connection whenever
/// it likes. We only find out on the next send, so an otherwise good request
/// fails having never reached the provider. Retrying that costs one extra
/// round trip and turns a spurious user-visible extraction failure into a
/// success.
pub(crate) async fn send_provider_post(
    req: reqwest::RequestBuilder,
) -> reqwest::Result<reqwest::Response> {
    let retry = req.try_clone();
    let err = match req.send().await {
        Ok(resp) => return Ok(resp),
        Err(e) => e,
    };
    let out = match retry {
        Some(retry) if request_never_reached_provider(&err) => retry.send().await,
        _ => Err(err),
    };
    if let Err(ref e) = out {
        // The endpoint names the managed provider, so callers get a stripped
        // message (`crw_core::error::reqwest_message`). Operators need the URL,
        // and this is the one place every provider POST passes through.
        tracing::warn!("provider request failed: {e}");
    }
    out
}

/// True only when the request provably never got to the provider, so replaying
/// it cannot duplicate work that was already billed.
fn request_never_reached_provider(err: &reqwest::Error) -> bool {
    // A timeout is deliberately not retried: the provider may have received the
    // request and be working on it, and these calls cost money.
    if err.is_timeout() {
        return false;
    }
    if err.is_connect() {
        return true;
    }
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = source {
        if e.to_string().contains(DEAD_POOLED_CONNECTION) {
            return true;
        }
        source = e.source();
    }
    false
}

fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut idx = max_bytes;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}

/// Content-extraction fallback. Returns just the markdown content for
/// backward compatibility with the existing readability pipeline.
#[allow(clippy::too_many_arguments)]
pub async fn extract_via_llm(
    html: &str,
    api_key: &str,
    provider: &str,
    model: &str,
    base_url: Option<&str>,
    max_tokens: u32,
    max_html_bytes: usize,
    azure_api_version: Option<&str>,
) -> CrwResult<String> {
    if api_key.is_empty() {
        return Err(CrwError::InvalidRequest(
            "LLM fallback enabled but api_key is empty".into(),
        ));
    }
    let truncated = truncate_on_char_boundary(html, max_html_bytes);
    let user_msg =
        format!("Extract the main article/content body from this HTML as markdown:\n\n{truncated}");

    let result = dispatch(
        provider,
        api_key,
        model,
        base_url,
        max_tokens,
        azure_api_version,
        None, // extraction path: keep provider-default temperature
        None, // extraction path: no reasoning_effort
        EXTRACTION_SYSTEM_PROMPT,
        &user_msg,
    )
    .await?;
    Ok(result.content)
}

/// Generic single-turn chat call used by feature-level modules
/// ([`crate::summary`], [`crate::answer`]).
///
/// Uses `cfg.api_key/provider/model/base_url/max_tokens/azure_api_version`.
pub async fn chat(
    cfg: &LlmConfig,
    system_prompt: &str,
    user_msg: &str,
) -> CrwResult<LlmCallResult> {
    if cfg.api_key.is_empty() {
        return Err(CrwError::InvalidRequest(
            "LLM call requires non-empty api_key — set CRW_EXTRACTION__LLM__API_KEY \
             or pass llm_api_key in request"
                .into(),
        ));
    }
    dispatch(
        &cfg.provider,
        &cfg.api_key,
        &cfg.model,
        cfg.base_url.as_deref(),
        cfg.max_tokens,
        cfg.azure_api_version.as_deref(),
        cfg.temperature,
        cfg.reasoning_effort.as_deref(),
        system_prompt,
        user_msg,
    )
    .await
}

/// Generate ONE entity/keyword-focused rewrite of a search query to widen
/// retrieval recall on the answer path. The caller fetches BOTH the original
/// and this rewrite and unions the candidate pools, so recall can only
/// increase. Returns an empty `Vec` on any failure or when the rewrite is
/// trivial/identical — the caller then uses the original query alone, which
/// means this can never reduce recall or break a search.
pub async fn expand_query(cfg: &LlmConfig, query: &str, max_variants: usize) -> Vec<String> {
    let n = max_variants.max(1);
    let sys = format!(
        "You rewrite a user's search query into up to {n} alternative \
         web-search queries that maximize the chance of finding the answer. \
         Rules: (1) EXPAND any abbreviation, acronym, or initialism to its full \
         proper name. (2) Keep the key named entities; use precise keywords a \
         relevant page would contain; drop filler words — but ALWAYS keep any \
         place name, city, region, or country VERBATIM in EVERY rewrite. A \
         location is never a filler word: dropping \"belgrade\" from \"best \
         pizza in belgrade\" would surface the wrong city, so preserve it in \
         all variants. (3) Make the alternatives DIVERSE — e.g. one focused on \
         the full entity name, one on distinctive keywords. Output ONLY the \
         rewritten queries, ONE per line: no quotes, no numbering, no labels. \
         Output at most {n} line(s)."
    );
    let mut leg = cfg.clone();
    leg.max_tokens = leg.max_tokens.min(60 + 60 * n as u32);
    match chat(&leg, &sys, query).await {
        Ok(r) => {
            let mut out: Vec<String> = Vec::new();
            for line in r.content.trim().lines() {
                let v = line.trim().trim_matches('"').trim().to_string();
                if v.is_empty() || v.eq_ignore_ascii_case(query.trim()) {
                    continue;
                }
                if out.iter().any(|e| e.eq_ignore_ascii_case(&v)) {
                    continue;
                }
                out.push(v);
                if out.len() >= n {
                    break;
                }
            }
            out
        }
        Err(_) => Vec::new(),
    }
}

/// Evidence-scout for adaptive multi-round retrieval. Given the question and a
/// short excerpt of what round-1 retrieval surfaced (which did NOT answer it),
/// produce up to `max_queries` TARGETED follow-up web-search queries to find or
/// confirm the answer. Unlike `expand_query` (blind rephrasings), the scout is
/// failure-aware: it leans on entity names/aliases seen in the evidence and
/// goes harder — exact-phrase `"entity"`, full official names for any acronym,
/// the specific predicate/date asked, or a likely authoritative source. Returns
/// deduped queries (empty on LLM failure → caller simply skips the extra round).
pub async fn scout_followups(
    cfg: &LlmConfig,
    query: &str,
    evidence: &str,
    max_queries: usize,
) -> Vec<String> {
    let n = max_queries.max(1);
    let sys = format!(
        "A first web search did NOT answer the user's question. You are a search \
         strategist. Using the question and the EVIDENCE excerpt of what the first \
         search found, write up to {n} NEW, BETTER web-search queries likely to \
         surface or confirm the answer. Rules: (1) EXPAND every acronym/initialism \
         to its full proper name. (2) Prefer the exact entity name(s) seen in the \
         evidence, quoted, plus the specific thing asked (the predicate, the date, \
         the number). ALWAYS keep any place name, city, region, or country from \
         the question VERBATIM in every query — a location is never optional. \
         (3) If the question uses an ordinal or positional reference (\"16th \
         edition\", \"Nth annual\", \"3rd president\", \"the Nth winner\") and the \
         evidence EXPLICITLY ties that reference to a concrete year, name, or \
         entity, use that concrete value in a query. Resolve ONLY from what the \
         evidence supports, never guess. Do NOT resolve open-ended temporal \
         references (\"current\", \"latest\", \"most recent\"), and never alter a \
         place name. \
         (4) Try a different angle than the original phrasing — an exact-phrase \
         query, an authoritative source guess, or the canonical entity. (5) Do NOT \
         repeat the user's original wording. Output ONLY the queries, ONE per \
         line: no quotes around the whole line, no numbering, no labels. Output at \
         most {n} line(s)."
    );
    let user = format!("QUESTION: {query}\n\nEVIDENCE (did not answer it):\n{evidence}");
    let mut leg = cfg.clone();
    leg.max_tokens = leg.max_tokens.min(60 + 60 * n as u32);
    match chat(&leg, &sys, &user).await {
        Ok(r) => {
            let mut out: Vec<String> = Vec::new();
            for line in r.content.trim().lines() {
                let v = line.trim().trim_matches('"').trim().to_string();
                if v.is_empty() || v.eq_ignore_ascii_case(query.trim()) {
                    continue;
                }
                if out.iter().any(|e| e.eq_ignore_ascii_case(&v)) {
                    continue;
                }
                out.push(v);
                if out.len() >= n {
                    break;
                }
            }
            out
        }
        Err(_) => Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch(
    provider: &str,
    api_key: &str,
    model: &str,
    base_url: Option<&str>,
    max_tokens: u32,
    azure_api_version: Option<&str>,
    temperature: Option<f32>,
    reasoning_effort: Option<&str>,
    system_prompt: &str,
    user_msg: &str,
) -> CrwResult<LlmCallResult> {
    // D reserved lane: bound LLM-call concurrency and keep a slice for
    // interactive traffic. Read the class here (async side) and hold the permit
    // across the provider HTTP call.
    // Reject unknown tags against SUPPORTED_PROVIDERS first, so the list served
    // by `/v1/capabilities` is exactly the list this dispatcher accepts.
    if !is_supported_provider(provider) {
        return Err(unknown_provider_error(provider));
    }
    let _llm_permit = crate::llm_gate::acquire_llm().await;
    let client = shared_client();
    match provider.to_ascii_lowercase().as_str() {
        "anthropic" => {
            call_anthropic(
                client,
                api_key,
                model,
                base_url,
                max_tokens,
                temperature,
                reasoning_effort,
                system_prompt,
                user_msg,
            )
            .await
        }
        // DeepSeek and other OpenAI-compatible providers use the same wire
        // protocol; users select them via `base_url`. Thread the dispatcher's
        // provider tag through so usage records reflect the actual provider
        // (e.g. `deepseek`) instead of always reporting `openai`.
        provider_tag @ ("openai" | "deepseek" | "openai-compatible") => {
            call_openai(
                client,
                api_key,
                model,
                base_url,
                max_tokens,
                temperature,
                reasoning_effort,
                system_prompt,
                user_msg,
                provider_tag,
            )
            .await
        }
        "openai-responses" => {
            let cfg = LlmConfig {
                provider: "openai-responses".to_string(),
                api_key: api_key.to_string(),
                model: model.to_string(),
                base_url: base_url.map(str::to_string),
                max_tokens,
                temperature,
                reasoning_effort: reasoning_effort.map(str::to_string),
                ..LlmConfig::default()
            };
            crate::responses::call_text(&cfg, system_prompt, user_msg, REQUEST_TIMEOUT).await
        }
        "azure" => {
            let endpoint = base_url.ok_or_else(|| {
                CrwError::InvalidRequest(
                    "azure provider requires base_url (Azure OpenAI endpoint)".into(),
                )
            })?;
            let version = azure_api_version.ok_or_else(|| {
                CrwError::InvalidRequest(
                    "azure provider requires azure_api_version (e.g. 2024-05-01-preview)".into(),
                )
            })?;
            call_azure(
                client,
                api_key,
                endpoint,
                model,
                version,
                max_tokens,
                temperature,
                system_prompt,
                user_msg,
            )
            .await
        }
        // Unreachable: the `is_supported_provider` guard above already rejected
        // anything without an arm. Kept so a new SUPPORTED_PROVIDERS entry that
        // forgets its arm fails loudly instead of silently routing nowhere.
        other => Err(unknown_provider_error(other)),
    }
}

/// `output_config.effort` for an Anthropic request, shared by both Anthropic
/// builders. Models from generation 5 on (Sonnet 5.5, Opus 5.5, Haiku 5.5,
/// Fable 5.1) think by default and thinking counts toward `max_tokens`, so at
/// their default effort the small helper budgets (query rewrite 120, passage
/// selection 256, search legs 1024) can be spent before any text. They get the
/// configured `reasoning_effort` when it is an Anthropic level, else `low`,
/// which skips thinking on most simple requests. Older ids think only when
/// asked and some reject the field, so they get none and their requests are
/// unchanged.
pub(crate) fn anthropic_effort<'a>(
    model: &str,
    reasoning_effort: Option<&'a str>,
) -> Option<&'a str> {
    // The generation is the first number after `claude-`: `claude-sonnet-5-5`,
    // `us.anthropic.claude-opus-5-5-v1:0`, `claude-3-7-sonnet-20250219`.
    let (_, rest) = model.split_once("claude-")?;
    let generation: u32 = rest.split('-').take(2).find_map(|s| s.parse().ok())?;
    (generation >= 5).then_some(match reasoning_effort {
        Some(e @ ("low" | "medium" | "high" | "xhigh" | "max")) => e,
        _ => "low",
    })
}

#[allow(clippy::too_many_arguments)]
async fn call_anthropic(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    base_url: Option<&str>,
    max_tokens: u32,
    temperature: Option<f32>,
    reasoning_effort: Option<&str>,
    system_prompt: &str,
    user_msg: &str,
) -> CrwResult<LlmCallResult> {
    let url = base_url.unwrap_or(ANTHROPIC_DEFAULT_BASE_URL);
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": system_prompt,
        "messages": [{ "role": "user", "content": user_msg }],
    });
    if let Some(t) = temperature {
        body["temperature"] = serde_json::json!(t);
    }
    if let Some(effort) = anthropic_effort(model, reasoning_effort) {
        body["output_config"] = serde_json::json!({ "effort": effort });
    }
    let resp = send_provider_post(
        client
            .post(url)
            .header("x-api-key", api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body),
    )
    .await
    .map_err(|e| {
        CrwError::Internal(format!(
            "LLM request failed: {}",
            crw_core::error::reqwest_message(e)
        ))
    })?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| {
        CrwError::Internal(format!(
            "LLM response read failed: {}",
            crw_core::error::reqwest_message(e)
        ))
    })?;
    if !status.is_success() {
        // NOTE: body may contain the request echoed back by some gateways.
        // The HTTP status code is enough — do not leak the body.
        return Err(CrwError::Internal(format!(
            "LLM HTTP {status} from anthropic"
        )));
    }
    let payload: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| CrwError::Internal(format!("LLM response parse failed: {e}")))?;
    let content = payload
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| {
            arr.iter()
                .find_map(|b| b.get("text").and_then(|t| t.as_str()))
        })
        .map(|s| s.to_string())
        .ok_or_else(|| CrwError::Internal("anthropic response missing content".to_string()))?;

    let usage = parse_anthropic_usage(&payload, model);
    Ok(LlmCallResult {
        content,
        usage,
        warning: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn call_openai(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    base_url: Option<&str>,
    max_tokens: u32,
    temperature: Option<f32>,
    reasoning_effort: Option<&str>,
    system_prompt: &str,
    user_msg: &str,
    provider: &str,
) -> CrwResult<LlmCallResult> {
    // Accept either a full endpoint URL or a `…/v1` base; append the path if
    // missing so users don't have to remember the suffix.
    let url_owned: String;
    let url: &str = match base_url {
        None => OPENAI_DEFAULT_BASE_URL,
        Some(b) if b.contains("/chat/completions") => b,
        Some(b) => {
            let trimmed = b.trim_end_matches('/');
            url_owned = format!("{trimmed}/chat/completions");
            &url_owned
        }
    };
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_msg }
        ],
    });
    // Deterministic eval: temp=0 + fixed seed make answers reproducible so a
    // real +2-3pp lever is distinguishable from sampling noise. None (prod
    // default) sends neither, preserving the provider default.
    if let Some(t) = temperature {
        body["temperature"] = serde_json::json!(t);
        body["seed"] = serde_json::json!(42);
    }
    // Only forward a present, non-empty value. A configured-but-empty value
    // deserializes to `Some("")` and would be rejected (HTTP 400) by providers
    // that validate the field, so treat it as unset.
    if let Some(effort) = reasoning_effort.filter(|s| !s.is_empty()) {
        body["reasoning_effort"] = serde_json::json!(effort);
    }
    // Fixed self-limiting retry on transient server throttling (HTTP 429/503)
    // only. The shared client carries a 30s per-attempt timeout and the
    // caller's request deadline is not threaded in here, so the budget stays
    // small and fixed (a few short jittered sleeps). 429/503 are fast server
    // rejects, so the worst case stays well under the request deadline. All
    // other non-2xx responses (and transport/timeout errors) keep the original
    // single-POST contract: hard-error on the first response.
    const MAX_ATTEMPTS: u32 = 3;
    let mut attempt: u32 = 0;
    let (status, text) = loop {
        attempt += 1;
        let resp = send_provider_post(
            client
                .post(url)
                .bearer_auth(api_key)
                .header("content-type", "application/json")
                .json(&body),
        )
        .await
        .map_err(|e| {
            CrwError::Internal(format!(
                "LLM request failed: {}",
                crw_core::error::reqwest_message(e)
            ))
        })?;

        let status = resp.status();
        let is_retryable = status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status == reqwest::StatusCode::SERVICE_UNAVAILABLE;
        if is_retryable && attempt < MAX_ATTEMPTS {
            // Exponential backoff with jitter: ~0.5s, ~1s base + up to ~1s
            // jitter. Drop the response body unread — the status is enough.
            let base_ms = 500u64 * (1u64 << (attempt - 1));
            let jitter_ms = rand::rng().random_range(0..1000);
            tokio::time::sleep(Duration::from_millis(base_ms + jitter_ms)).await;
            continue;
        }

        let text = resp.text().await.map_err(|e| {
            CrwError::Internal(format!(
                "LLM response read failed: {}",
                crw_core::error::reqwest_message(e)
            ))
        })?;
        break (status, text);
    };
    if !status.is_success() {
        return Err(CrwError::Internal(format!("LLM HTTP {status} from openai")));
    }
    let payload: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| CrwError::Internal(format!("LLM response parse failed: {e}")))?;
    let content = payload
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| CrwError::Internal("openai response missing content".to_string()))?;

    let usage = parse_openai_usage(&payload, model, provider);
    Ok(LlmCallResult {
        content,
        usage,
        warning: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn call_azure(
    client: &reqwest::Client,
    api_key: &str,
    endpoint: &str,
    deployment: &str,
    api_version: &str,
    max_tokens: u32,
    temperature: Option<f32>,
    system_prompt: &str,
    user_msg: &str,
) -> CrwResult<LlmCallResult> {
    let endpoint_trimmed = endpoint.trim_end_matches('/');
    let url = format!(
        "{endpoint_trimmed}/openai/deployments/{deployment}/chat/completions?api-version={api_version}"
    );
    let mut body = serde_json::json!({
        "max_tokens": max_tokens,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_msg }
        ],
    });
    if let Some(t) = temperature {
        body["temperature"] = serde_json::json!(t);
        body["seed"] = serde_json::json!(42);
    }
    let resp = send_provider_post(
        client
            .post(&url)
            .header("api-key", api_key)
            .header("content-type", "application/json")
            .json(&body),
    )
    .await
    .map_err(|e| {
        CrwError::Internal(format!(
            "LLM request failed: {}",
            crw_core::error::reqwest_message(e)
        ))
    })?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| {
        CrwError::Internal(format!(
            "LLM response read failed: {}",
            crw_core::error::reqwest_message(e)
        ))
    })?;
    if !status.is_success() {
        return Err(CrwError::Internal(format!("LLM HTTP {status} from azure")));
    }
    let payload: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| CrwError::Internal(format!("LLM response parse failed: {e}")))?;
    let content = payload
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| CrwError::Internal("azure response missing content".to_string()))?;

    let usage = parse_openai_usage(&payload, deployment, "azure");
    Ok(LlmCallResult {
        content,
        usage,
        warning: None,
    })
}

fn parse_anthropic_usage(payload: &serde_json::Value, model: &str) -> Option<LlmUsage> {
    let usage = payload.get("usage")?;
    let input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64())? as u32;
    let output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64())? as u32;
    // Anthropic prompt-cache fields (only present when cache is in use).
    // `cache_read_input_tokens` is a cache HIT (discounted read).
    // `cache_creation_input_tokens` is a cache WRITE — billed at the full
    // input rate, so we count it as a "miss" for the hit/miss breakdown.
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let cache_creation = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let (cache_hit_input_tokens, cache_miss_input_tokens) = match (cache_read, cache_creation) {
        (None, None) => (None, None),
        (read, create) => {
            let hit = read.unwrap_or(0);
            let create = create.unwrap_or(0);
            // Anthropic reports `input_tokens` as non-cached input only —
            // the cache_read/cache_creation buckets are additive on top.
            // Treat plain `input_tokens` + cache_creation as the miss side.
            let miss = input_tokens.saturating_add(create);
            (Some(hit), Some(miss))
        }
    };
    let total = input_tokens + output_tokens;
    Some(LlmUsage {
        input_tokens,
        output_tokens,
        total_tokens: total,
        estimated_cost_usd: pricing::calculate_cost(model, input_tokens, output_tokens),
        model: model.to_string(),
        provider: "anthropic".to_string(),
        cache_hit_input_tokens,
        cache_miss_input_tokens,
        truncated: false,
        calls: 1,
        // R1 counters are scoped to /v1/search aggregation; single-call
        // sites always emit defaults. Aggregation happens in the caller
        // (crw-server::routes::search::search_inner).
        executed_summaries: 0,
        answer_executed: false,
    })
}

fn parse_openai_usage(
    payload: &serde_json::Value,
    model: &str,
    provider: &str,
) -> Option<LlmUsage> {
    let usage = payload.get("usage")?;
    let input_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64())? as u32;
    let output_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64())? as u32;
    let total = usage
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(input_tokens + output_tokens);

    // Cache breakdown — providers expose this two different ways:
    //   * OpenAI / Azure / OpenAI-compat: `usage.prompt_tokens_details.cached_tokens`
    //     (cache_hit; cache_miss = prompt_tokens - cached_tokens)
    //   * DeepSeek: explicit `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`
    //     at the top level of `usage`.
    let deepseek_hit = usage
        .get("prompt_cache_hit_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let deepseek_miss = usage
        .get("prompt_cache_miss_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let openai_cached = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);

    let (cache_hit_input_tokens, cache_miss_input_tokens) =
        match (deepseek_hit, deepseek_miss, openai_cached) {
            (Some(hit), Some(miss), _) => (Some(hit), Some(miss)),
            (Some(hit), None, _) => (Some(hit), Some(input_tokens.saturating_sub(hit))),
            (None, Some(miss), _) => (Some(input_tokens.saturating_sub(miss)), Some(miss)),
            (None, None, Some(cached)) => (Some(cached), Some(input_tokens.saturating_sub(cached))),
            (None, None, None) => (None, None),
        };

    Some(LlmUsage {
        input_tokens,
        output_tokens,
        total_tokens: total,
        estimated_cost_usd: pricing::calculate_cost(model, input_tokens, output_tokens),
        model: model.to_string(),
        provider: provider.to_string(),
        cache_hit_input_tokens,
        cache_miss_input_tokens,
        truncated: false,
        calls: 1,
        // R1 counters are scoped to /v1/search aggregation; single-call
        // sites always emit defaults. Aggregation happens in the caller
        // (crw-server::routes::search::search_inner).
        executed_summaries: 0,
        answer_executed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn openai_chat_response() -> serde_json::Value {
        serde_json::json!({
            "choices": [{ "message": { "content": "hello from mock" } }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
        })
    }

    fn anthropic_chat_response() -> serde_json::Value {
        serde_json::json!({
            "content": [{ "type": "text", "text": "hello from anthropic mock" }],
            "usage": { "input_tokens": 12, "output_tokens": 6 }
        })
    }

    fn base_cfg(provider: &str, base_url: String) -> LlmConfig {
        LlmConfig {
            provider: provider.into(),
            api_key: "test-key".into(),
            model: "test-model".into(),
            base_url: Some(base_url),
            ..Default::default()
        }
    }

    #[test]
    fn supported_providers_are_the_ones_dispatch_accepts() {
        // `/v1/capabilities` advertises SUPPORTED_PROVIDERS verbatim, so every
        // entry must be routable and nothing else may be.
        for provider in SUPPORTED_PROVIDERS {
            assert!(
                is_supported_provider(provider),
                "advertised provider `{provider}` is not accepted by dispatch"
            );
            assert!(
                is_supported_provider(&provider.to_uppercase()),
                "provider matching must be case-insensitive: `{provider}`"
            );
        }
        assert!(!is_supported_provider("gemini"));
        assert!(!is_supported_provider(""));
    }

    #[test]
    fn unknown_provider_error_lists_every_supported_provider() {
        let msg = unknown_provider_error("gemini").to_string();
        for provider in SUPPORTED_PROVIDERS {
            assert!(
                msg.contains(provider),
                "the unknown-provider error must list `{provider}`, got: {msg}"
            );
        }
    }

    #[tokio::test]
    async fn unknown_provider_is_rejected_before_any_http_call() {
        let err = dispatch(
            "gemini", "key", "model", None, 128, None, None, None, "sys", "user",
        )
        .await
        .expect_err("an unsupported provider must be rejected");
        assert!(err.to_string().contains("unknown LLM provider"));
    }

    #[tokio::test]
    async fn empty_api_key_errors_synchronously() {
        let result = extract_via_llm(
            "<html></html>",
            "",
            "anthropic",
            "claude-haiku-4-5",
            None,
            512,
            10_000,
            None,
        )
        .await;
        assert!(matches!(result, Err(CrwError::InvalidRequest(_))));
    }

    #[tokio::test]
    async fn unknown_provider_errors() {
        let result = extract_via_llm(
            "<html></html>",
            "key",
            "groq",
            "model",
            None,
            512,
            10_000,
            None,
        )
        .await;
        assert!(matches!(result, Err(CrwError::InvalidRequest(_))));
    }

    #[tokio::test]
    async fn truncation_respects_char_boundaries() {
        let html = format!("{}🚀tail", "a".repeat(99));
        // Provider unknown so we never make a request, but the truncation
        // logic runs first and would panic on a non-boundary slice.
        let _ = extract_via_llm(&html, "key", "unknown", "m", None, 512, 100, None).await;
    }

    #[tokio::test]
    async fn azure_provider_requires_base_url_and_api_version() {
        let no_base = extract_via_llm(
            "<html></html>",
            "key",
            "azure",
            "gpt-4o-mini",
            None,
            512,
            10_000,
            Some("2024-05-01-preview"),
        )
        .await;
        assert!(matches!(no_base, Err(CrwError::InvalidRequest(_))));
        let no_version = extract_via_llm(
            "<html></html>",
            "key",
            "azure",
            "gpt-4o-mini",
            Some("https://x.openai.azure.com"),
            512,
            10_000,
            None,
        )
        .await;
        assert!(matches!(no_version, Err(CrwError::InvalidRequest(_))));
    }

    #[test]
    fn parse_anthropic_usage_extracts_tokens() {
        let payload = serde_json::json!({
            "usage": { "input_tokens": 100, "output_tokens": 50 }
        });
        let usage = parse_anthropic_usage(&payload, "claude-haiku-4-5").unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.total_tokens, 150);
        assert!(usage.estimated_cost_usd.is_some());
    }

    #[test]
    fn parse_openai_usage_extracts_tokens() {
        let payload = serde_json::json!({
            "usage": { "prompt_tokens": 200, "completion_tokens": 100, "total_tokens": 300 }
        });
        let usage = parse_openai_usage(&payload, "gpt-4o-mini", "openai").unwrap();
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.output_tokens, 100);
        assert_eq!(usage.total_tokens, 300);
        assert_eq!(usage.calls, 1);
        assert!(!usage.truncated);
        assert!(usage.cache_hit_input_tokens.is_none());
        assert!(usage.cache_miss_input_tokens.is_none());
    }

    #[test]
    fn parse_anthropic_usage_extracts_cache_hit_tokens() {
        let payload = serde_json::json!({
            "usage": {
                "input_tokens": 80,
                "output_tokens": 40,
                "cache_read_input_tokens": 1024,
                "cache_creation_input_tokens": 256,
            }
        });
        let usage = parse_anthropic_usage(&payload, "claude-haiku-4-5").unwrap();
        assert_eq!(usage.input_tokens, 80);
        assert_eq!(usage.output_tokens, 40);
        assert_eq!(usage.cache_hit_input_tokens, Some(1024));
        // miss = plain input_tokens + cache_creation (both billed at full rate)
        assert_eq!(usage.cache_miss_input_tokens, Some(80 + 256));
        assert_eq!(usage.provider, "anthropic");
        assert_eq!(usage.calls, 1);
    }

    #[test]
    fn parse_openai_usage_deepseek_cache_breakdown() {
        // DeepSeek-style explicit hit/miss fields.
        let payload = serde_json::json!({
            "usage": {
                "prompt_tokens": 1500,
                "completion_tokens": 200,
                "total_tokens": 1700,
                "prompt_cache_hit_tokens": 1200,
                "prompt_cache_miss_tokens": 300,
            }
        });
        let usage = parse_openai_usage(&payload, "deepseek-chat", "deepseek").unwrap();
        assert_eq!(usage.input_tokens, 1500);
        assert_eq!(usage.cache_hit_input_tokens, Some(1200));
        assert_eq!(usage.cache_miss_input_tokens, Some(300));
        // Provider tag must be carried through — NOT hardcoded to "openai".
        assert_eq!(usage.provider, "deepseek");
    }

    #[test]
    fn parse_openai_usage_compat_cached_tokens() {
        // OpenAI / OpenAI-compatible style: nested prompt_tokens_details.cached_tokens.
        let payload = serde_json::json!({
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 50,
                "total_tokens": 1050,
                "prompt_tokens_details": { "cached_tokens": 400 },
            }
        });
        let usage = parse_openai_usage(&payload, "gpt-4o-mini", "openai").unwrap();
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.cache_hit_input_tokens, Some(400));
        assert_eq!(usage.cache_miss_input_tokens, Some(600));
        assert_eq!(usage.provider, "openai");
    }

    // ── SUPPORTED_PROVIDERS / dispatch table ──────────────────────────────

    #[test]
    fn supported_providers_exact_list() {
        // Pins the exact advertised set + order for `/v1/capabilities`. A drift
        // here (add/remove/reorder) must be a deliberate edit, not a surprise.
        assert_eq!(
            SUPPORTED_PROVIDERS,
            &[
                "anthropic",
                "openai",
                "deepseek",
                "openai-compatible",
                "openai-responses",
                "azure",
            ]
        );
    }

    #[test]
    fn is_supported_provider_rejects_whitespace_and_garbage() {
        assert!(!is_supported_provider(" openai"));
        assert!(!is_supported_provider("openai "));
        assert!(!is_supported_provider("open ai"));
        assert!(!is_supported_provider("openai\n"));
        assert!(!is_supported_provider(&"x".repeat(10_000)));
    }

    #[test]
    fn unknown_provider_error_echoes_offending_tag_verbatim() {
        let msg = unknown_provider_error("Gr0q!!").to_string();
        assert!(msg.contains("Gr0q!!"), "got: {msg}");
    }

    // ── truncate_on_char_boundary ──────────────────────────────────────────

    #[test]
    fn truncate_on_char_boundary_zero_cap_yields_empty() {
        assert_eq!(truncate_on_char_boundary("hello", 0), "");
    }

    #[test]
    fn truncate_on_char_boundary_cap_larger_than_input_is_a_noop() {
        assert_eq!(truncate_on_char_boundary("hi", 1_000), "hi");
    }

    #[test]
    fn truncate_on_char_boundary_exact_boundary_kept_whole() {
        // "hello" is 5 ASCII bytes; a cap exactly at the length must not clip.
        assert_eq!(truncate_on_char_boundary("hello", 5), "hello");
    }

    // ── parse_anthropic_usage edge cases ───────────────────────────────────

    #[test]
    fn parse_anthropic_usage_missing_usage_field_is_none() {
        let payload = serde_json::json!({ "content": [] });
        assert!(parse_anthropic_usage(&payload, "claude-haiku-4-5").is_none());
    }

    #[test]
    fn parse_anthropic_usage_wrong_field_type_is_none() {
        // `input_tokens` as a string fails the `.as_u64()` extraction, and the
        // whole function returns `None` via `?` rather than panicking.
        let payload = serde_json::json!({
            "usage": { "input_tokens": "not-a-number", "output_tokens": 5 }
        });
        assert!(parse_anthropic_usage(&payload, "claude-haiku-4-5").is_none());
    }

    #[test]
    fn parse_anthropic_usage_zero_tokens() {
        let payload = serde_json::json!({ "usage": { "input_tokens": 0, "output_tokens": 0 } });
        let usage = parse_anthropic_usage(&payload, "claude-haiku-4-5").unwrap();
        assert_eq!(usage.total_tokens, 0);
        assert_eq!(usage.estimated_cost_usd, Some(0.0));
    }

    #[test]
    fn parse_anthropic_usage_unknown_model_has_no_cost() {
        let payload = serde_json::json!({ "usage": { "input_tokens": 100, "output_tokens": 50 } });
        let usage = parse_anthropic_usage(&payload, "some-future-model-xyz").unwrap();
        assert_eq!(usage.estimated_cost_usd, None);
    }

    // ── parse_openai_usage edge cases ──────────────────────────────────────

    #[test]
    fn parse_openai_usage_missing_usage_field_is_none() {
        let payload = serde_json::json!({ "choices": [] });
        assert!(parse_openai_usage(&payload, "gpt-4o-mini", "openai").is_none());
    }

    #[test]
    fn parse_openai_usage_total_tokens_falls_back_to_sum_when_absent() {
        let payload =
            serde_json::json!({ "usage": { "prompt_tokens": 30, "completion_tokens": 20 } });
        let usage = parse_openai_usage(&payload, "gpt-4o-mini", "openai").unwrap();
        assert_eq!(usage.total_tokens, 50);
    }

    #[test]
    fn parse_openai_usage_deepseek_hit_only_derives_miss() {
        let payload = serde_json::json!({
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 10,
                "prompt_cache_hit_tokens": 700,
            }
        });
        let usage = parse_openai_usage(&payload, "deepseek-chat", "deepseek").unwrap();
        assert_eq!(usage.cache_hit_input_tokens, Some(700));
        assert_eq!(usage.cache_miss_input_tokens, Some(300));
    }

    #[test]
    fn parse_openai_usage_deepseek_miss_only_derives_hit() {
        let payload = serde_json::json!({
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 10,
                "prompt_cache_miss_tokens": 250,
            }
        });
        let usage = parse_openai_usage(&payload, "deepseek-chat", "deepseek").unwrap();
        assert_eq!(usage.cache_hit_input_tokens, Some(750));
        assert_eq!(usage.cache_miss_input_tokens, Some(250));
    }

    #[test]
    fn parse_openai_usage_unknown_model_has_no_cost() {
        let payload =
            serde_json::json!({ "usage": { "prompt_tokens": 100, "completion_tokens": 50 } });
        let usage = parse_openai_usage(&payload, "totally-unknown-model", "openai").unwrap();
        assert_eq!(usage.estimated_cost_usd, None);
    }

    #[test]
    fn parse_openai_usage_provider_tag_passthrough_for_azure() {
        let payload =
            serde_json::json!({ "usage": { "prompt_tokens": 10, "completion_tokens": 5 } });
        let usage = parse_openai_usage(&payload, "gpt-4o-mini", "azure").unwrap();
        assert_eq!(usage.provider, "azure");
    }

    // ── extract_via_llm: truncation edge cases (no network) ────────────────

    #[tokio::test]
    async fn extract_via_llm_zero_max_html_bytes_truncates_to_empty() {
        // Provider is unknown so no network call is ever made; truncation runs
        // first and must not panic on a zero-byte cap.
        let result = extract_via_llm("<html>hi</html>", "key", "unknown", "m", None, 512, 0, None)
            .await
            .unwrap_err();
        assert!(matches!(result, CrwError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn extract_via_llm_azure_case_insensitive_still_requires_base_url() {
        let result = extract_via_llm(
            "<html></html>",
            "key",
            "AZURE",
            "gpt-4o-mini",
            None,
            512,
            10_000,
            Some("2024-05-01-preview"),
        )
        .await;
        assert!(matches!(result, Err(CrwError::InvalidRequest(_))));
    }

    // ── OpenAI-compatible URL construction: the doubling bug class ─────────

    #[tokio::test]
    async fn openai_bare_base_url_gets_chat_completions_appended() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response()))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", server.uri());
        let res = chat(&cfg, "sys", "user").await.expect("chat succeeds");
        assert_eq!(res.content, "hello from mock");
    }

    #[tokio::test]
    async fn openai_full_endpoint_used_verbatim_never_doubled() {
        let server = MockServer::start().await;
        // Mounted ONLY at the single, non-doubled path. A doubling bug would
        // POST to `/chat/completions/chat/completions` and 404 here.
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response()))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", format!("{}/chat/completions", server.uri()));
        chat(&cfg, "sys", "user")
            .await
            .expect("verbatim endpoint must not be doubled");
    }

    #[tokio::test]
    async fn openai_v1_base_gets_single_chat_completions_suffix() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response()))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", format!("{}/v1", server.uri()));
        chat(&cfg, "sys", "user").await.expect("chat succeeds");
    }

    #[tokio::test]
    async fn openai_trailing_slash_base_not_double_slashed() {
        let server = MockServer::start().await;
        // Exact path match: a stray double slash (`/v1//chat/completions`) would
        // NOT match this mock and the call would fail.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response()))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", format!("{}/v1/", server.uri()));
        chat(&cfg, "sys", "user").await.expect("chat succeeds");
    }

    #[tokio::test]
    async fn deepseek_provider_tag_shares_openai_url_logic_and_reports_own_tag() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response()))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = base_cfg("deepseek", format!("{}/v1", server.uri()));
        let res = chat(&cfg, "sys", "user").await.expect("chat succeeds");
        assert_eq!(res.usage.unwrap().provider, "deepseek");
    }

    #[tokio::test]
    async fn openai_compatible_provider_tag_routes_through_call_openai() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response()))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = base_cfg("openai-compatible", server.uri());
        chat(&cfg, "sys", "user").await.expect("chat succeeds");
    }

    #[tokio::test]
    async fn dispatch_provider_matching_is_case_insensitive_for_openai_and_deepseek() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response()))
            .mount(&server)
            .await;

        let cfg_mixed = base_cfg("OpenAI", server.uri());
        chat(&cfg_mixed, "sys", "user")
            .await
            .expect("mixed-case provider tag must still route");

        let cfg_upper = base_cfg("DEEPSEEK", server.uri());
        chat(&cfg_upper, "sys", "user")
            .await
            .expect("upper-case provider tag must still route");
    }

    // ── OpenAI error variants ──────────────────────────────────────────────

    #[tokio::test]
    async fn openai_auth_failure_surfaces_status_and_provider() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": { "message": "Invalid API key" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", server.uri());
        let err = chat(&cfg, "sys", "user").await.unwrap_err().to_string();
        assert!(err.contains("401"), "got: {err}");
        assert!(err.contains("openai"), "got: {err}");
    }

    #[tokio::test]
    async fn openai_malformed_json_body_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw("{not valid json", "application/json"),
            )
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", server.uri());
        let err = chat(&cfg, "sys", "user").await.unwrap_err().to_string();
        assert!(err.contains("LLM response parse failed"), "got: {err}");
    }

    #[tokio::test]
    async fn openai_empty_choices_array_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "choices": [] })),
            )
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", server.uri());
        let err = chat(&cfg, "sys", "user").await.unwrap_err().to_string();
        assert!(
            err.contains("openai response missing content"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn openai_null_message_content_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": null } }]
            })))
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", server.uri());
        let err = chat(&cfg, "sys", "user").await.unwrap_err().to_string();
        assert!(
            err.contains("openai response missing content"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn openai_500_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", server.uri());
        let err = chat(&cfg, "sys", "user").await.unwrap_err().to_string();
        assert!(err.contains("500"), "got: {err}");
    }

    #[tokio::test]
    async fn openai_temperature_and_seed_forwarded_when_set() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response()))
            .mount(&server)
            .await;

        let mut cfg = base_cfg("openai", server.uri());
        cfg.temperature = Some(0.0);
        chat(&cfg, "sys", "user").await.expect("chat succeeds");

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body.get("temperature").and_then(|v| v.as_f64()), Some(0.0));
        assert_eq!(body.get("seed").and_then(|v| v.as_i64()), Some(42));
    }

    #[tokio::test]
    async fn openai_temperature_absent_when_unset() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response()))
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", server.uri());
        chat(&cfg, "sys", "user").await.expect("chat succeeds");

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert!(body.get("temperature").is_none());
        assert!(body.get("seed").is_none());
    }

    // ── Anthropic ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn anthropic_success_round_trip_via_full_endpoint() {
        // call_anthropic uses base_url VERBATIM (no path-appending logic), so
        // the base must already be the full `/v1/messages` endpoint.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_chat_response()))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = base_cfg("anthropic", format!("{}/v1/messages", server.uri()));
        let res = chat(&cfg, "sys", "user").await.expect("chat succeeds");
        assert_eq!(res.content, "hello from anthropic mock");
        let usage = res.usage.unwrap();
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.output_tokens, 6);
        assert_eq!(usage.provider, "anthropic");
    }

    #[tokio::test]
    async fn anthropic_chat_skips_thinking_blocks() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [
                    { "type": "thinking", "thinking": "", "signature": "c2ln" },
                    { "type": "redacted_thinking", "data": "ZGF0YQ==" },
                    { "type": "text", "text": "hello" }
                ],
                "usage": { "input_tokens": 12, "output_tokens": 6 }
            })))
            .mount(&server)
            .await;

        let cfg = base_cfg("anthropic", format!("{}/v1/messages", server.uri()));
        let res = chat(&cfg, "sys", "user").await.expect("chat succeeds");
        assert_eq!(res.content, "hello");
    }

    #[test]
    fn anthropic_effort_only_for_models_that_think_by_default() {
        let cases = [
            ("claude-sonnet-5-5", None, Some("low")),
            ("claude-opus-5-5", Some("high"), Some("high")),
            ("claude-haiku-5-5", Some("none"), Some("low")),
            ("claude-haiku-5-5", Some(""), Some("low")),
            ("us.anthropic.claude-fable-5-1-v1:0", None, Some("low")),
            ("claude-sonnet-4-6", Some("high"), None),
            ("claude-haiku-4-5-20251001", None, None),
            ("claude-opus-4-20250514", None, None),
            ("claude-3-7-sonnet-20250219", None, None),
            ("test-model", Some("low"), None),
        ];
        for (model, configured, want) in cases {
            assert_eq!(anthropic_effort(model, configured), want, "{model}");
        }
    }

    /// Thinking counts toward `max_tokens` on current models, so the small
    /// helper budgets need a low effort there; older ids get no field.
    #[tokio::test]
    async fn anthropic_chat_sends_effort_only_to_models_that_think_by_default() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{ "type": "text", "text": "ok" }],
                "usage": { "input_tokens": 1, "output_tokens": 1 }
            })))
            .mount(&server)
            .await;

        let mut cfg = base_cfg("anthropic", format!("{}/v1/messages", server.uri()));
        for model in ["claude-sonnet-5-5", "claude-sonnet-4-6"] {
            cfg.model = model.into();
            chat(&cfg, "sys", "user").await.expect("chat succeeds");
        }
        let bodies: Vec<serde_json::Value> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect();
        assert_eq!(
            bodies[0]["output_config"],
            serde_json::json!({ "effort": "low" })
        );
        assert!(bodies[1].get("output_config").is_none());
    }

    #[tokio::test]
    async fn anthropic_error_status_surfaces_provider_tag() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let cfg = base_cfg("anthropic", format!("{}/v1/messages", server.uri()));
        let err = chat(&cfg, "sys", "user").await.unwrap_err().to_string();
        assert!(err.contains("500"), "got: {err}");
        assert!(err.contains("anthropic"), "got: {err}");
    }

    #[tokio::test]
    async fn anthropic_empty_content_array_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "content": [] })),
            )
            .mount(&server)
            .await;

        let cfg = base_cfg("anthropic", format!("{}/v1/messages", server.uri()));
        let err = chat(&cfg, "sys", "user").await.unwrap_err().to_string();
        assert!(
            err.contains("anthropic response missing content"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn anthropic_content_with_only_non_text_blocks_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{ "type": "tool_use", "id": "t1", "name": "x", "input": {} }]
            })))
            .mount(&server)
            .await;

        let cfg = base_cfg("anthropic", format!("{}/v1/messages", server.uri()));
        let err = chat(&cfg, "sys", "user").await.unwrap_err().to_string();
        assert!(
            err.contains("anthropic response missing content"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn anthropic_malformed_json_body_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw("not json at all", "application/json"),
            )
            .mount(&server)
            .await;

        let cfg = base_cfg("anthropic", format!("{}/v1/messages", server.uri()));
        let err = chat(&cfg, "sys", "user").await.unwrap_err().to_string();
        assert!(err.contains("LLM response parse failed"), "got: {err}");
    }

    #[tokio::test]
    async fn anthropic_does_not_retry_on_429_single_post_contract() {
        // Unlike the OpenAI path, call_anthropic has no retry loop: a 429 must
        // hard-error on the first response, exactly one request.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(429))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = base_cfg("anthropic", format!("{}/v1/messages", server.uri()));
        let err = chat(&cfg, "sys", "user").await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn anthropic_temperature_forwarded_when_set() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_chat_response()))
            .mount(&server)
            .await;

        let mut cfg = base_cfg("anthropic", format!("{}/v1/messages", server.uri()));
        cfg.temperature = Some(0.5);
        chat(&cfg, "sys", "user").await.expect("chat succeeds");

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body.get("temperature").and_then(|v| v.as_f64()), Some(0.5));
    }

    #[tokio::test]
    async fn anthropic_temperature_absent_when_unset() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_chat_response()))
            .mount(&server)
            .await;

        let cfg = base_cfg("anthropic", format!("{}/v1/messages", server.uri()));
        chat(&cfg, "sys", "user").await.expect("chat succeeds");

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert!(body.get("temperature").is_none());
    }

    // ── Azure ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn azure_url_includes_deployment_and_api_version_query() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/deployments/my-deployment/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response()))
            .expect(1)
            .mount(&server)
            .await;

        let result = extract_via_llm(
            "<html>hi</html>",
            "key",
            "azure",
            "my-deployment",
            Some(&server.uri()),
            512,
            10_000,
            Some("2024-05-01-preview"),
        )
        .await;
        assert!(result.is_ok(), "got: {result:?}");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests[0].url.query(),
            Some("api-version=2024-05-01-preview")
        );
    }

    #[tokio::test]
    async fn azure_endpoint_trailing_slash_is_trimmed_not_doubled() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/deployments/dep/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response()))
            .expect(1)
            .mount(&server)
            .await;

        let result = extract_via_llm(
            "<html></html>",
            "key",
            "azure",
            "dep",
            Some(&format!("{}/", server.uri())),
            512,
            10_000,
            Some("2024-05-01-preview"),
        )
        .await;
        assert!(result.is_ok(), "got: {result:?}");
    }

    #[tokio::test]
    async fn azure_success_reports_provider_tag_in_usage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/deployments/dep/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response()))
            .mount(&server)
            .await;

        let cfg = LlmConfig {
            provider: "azure".into(),
            api_key: "key".into(),
            model: "dep".into(),
            base_url: Some(server.uri()),
            azure_api_version: Some("2024-05-01-preview".into()),
            ..Default::default()
        };
        let res = chat(&cfg, "sys", "user").await.expect("chat succeeds");
        assert_eq!(res.usage.unwrap().provider, "azure");
    }

    #[tokio::test]
    async fn azure_error_status_surfaces_provider_tag() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/deployments/dep/chat/completions"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let cfg = LlmConfig {
            provider: "azure".into(),
            api_key: "key".into(),
            model: "dep".into(),
            base_url: Some(server.uri()),
            azure_api_version: Some("2024-05-01-preview".into()),
            ..Default::default()
        };
        let err = chat(&cfg, "sys", "user").await.unwrap_err().to_string();
        assert!(err.contains("403"), "got: {err}");
        assert!(err.contains("azure"), "got: {err}");
    }

    #[tokio::test]
    async fn azure_empty_choices_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/deployments/dep/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "choices": [] })),
            )
            .mount(&server)
            .await;

        let cfg = LlmConfig {
            provider: "azure".into(),
            api_key: "key".into(),
            model: "dep".into(),
            base_url: Some(server.uri()),
            azure_api_version: Some("2024-05-01-preview".into()),
            ..Default::default()
        };
        let err = chat(&cfg, "sys", "user").await.unwrap_err().to_string();
        assert!(err.contains("azure response missing content"), "got: {err}");
    }

    #[tokio::test]
    async fn azure_temperature_and_seed_forwarded_when_set() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/deployments/dep/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response()))
            .mount(&server)
            .await;

        let cfg = LlmConfig {
            provider: "azure".into(),
            api_key: "key".into(),
            model: "dep".into(),
            base_url: Some(server.uri()),
            azure_api_version: Some("2024-05-01-preview".into()),
            temperature: Some(0.25),
            ..Default::default()
        };
        chat(&cfg, "sys", "user").await.expect("chat succeeds");

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body.get("temperature").and_then(|v| v.as_f64()), Some(0.25));
        assert_eq!(body.get("seed").and_then(|v| v.as_i64()), Some(42));
    }

    // ── expand_query ────────────────────────────────────────────────────

    #[tokio::test]
    async fn expand_query_parses_dedupes_and_drops_self_and_empty_lines() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content":
                    "\"best pizza belgrade\"\n\nbest pizza in belgrade\nBest Pizza Belgrade\ntop pizzerias serbia"
                } }]
            })))
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", server.uri());
        let out = expand_query(&cfg, "best pizza in belgrade", 3).await;
        // Line 1: quotes stripped -> "best pizza belgrade".
        // Line 2 (blank): skipped.
        // Line 3: identical to the query (case-insensitive) -> skipped.
        // Line 4: dup of line 1 (case-insensitive) -> skipped.
        // Line 5: kept.
        assert_eq!(out, vec!["best pizza belgrade", "top pizzerias serbia"]);
    }

    #[tokio::test]
    async fn expand_query_returns_empty_on_llm_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", server.uri());
        let out = expand_query(&cfg, "some query", 3).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn expand_query_caps_at_max_variants() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "a\nb\nc\nd\ne" } }]
            })))
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", server.uri());
        let out = expand_query(&cfg, "query", 2).await;
        assert_eq!(out.len(), 2);
        assert_eq!(out, vec!["a", "b"]);
    }

    // ── scout_followups ─────────────────────────────────────────────────

    #[tokio::test]
    async fn scout_followups_sends_question_and_evidence_and_parses_lines() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "\"exact phrase query\"\nauthoritative source guess" } }]
            })))
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", server.uri());
        let out =
            scout_followups(&cfg, "who won the 16th edition", "no clear winner found", 5).await;
        assert_eq!(
            out,
            vec!["exact phrase query", "authoritative source guess"]
        );

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let user_msg = body["messages"][1]["content"].as_str().unwrap();
        assert!(user_msg.contains("QUESTION: who won the 16th edition"));
        assert!(user_msg.contains("EVIDENCE (did not answer it):\nno clear winner found"));
    }

    #[tokio::test]
    async fn scout_followups_returns_empty_on_llm_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", server.uri());
        let out = scout_followups(&cfg, "q", "evidence", 3).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn scout_followups_zero_max_queries_floors_to_one() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "only one line\nsecond line ignored by caller cap" } }]
            })))
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", server.uri());
        // `max_queries.max(1)` means 0 behaves like 1: at most one result kept.
        let out = scout_followups(&cfg, "q", "evidence", 0).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], "only one line");
    }

    // ── dispatch: reserved gate does not block a request from completing ──

    #[tokio::test]
    async fn dispatch_multiple_sequential_calls_all_succeed() {
        // Exercises the llm_gate permit acquire/release across repeated calls on
        // the same provider — a leaked permit would deadlock later calls.
        let server = MockServer::start().await;
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response()))
            .mount(&server)
            .await;

        let cfg = base_cfg("openai", server.uri());
        for _ in 0..3 {
            chat(&cfg, "sys", "user").await.expect("chat succeeds");
            hits.fetch_add(1, Ordering::SeqCst);
        }
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }
}
