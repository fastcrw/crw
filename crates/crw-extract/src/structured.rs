use crate::basis;
use crate::pricing;
use crw_core::config::LlmConfig;
use crw_core::error::{CrwError, CrwResult};
use crw_core::evidence::{Basis, BasisWarning};
use crw_core::types::LlmUsage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use std::time::Duration;

/// Request timeout for LLM API calls.
pub(crate) const LLM_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Below this much remaining budget a schema-validation retry is not attempted:
/// a second full call would not finish, and burning the tokens to find that out
/// helps nobody. The whole operation stays inside the caller's original timeout.
const MIN_RETRY_BUDGET: Duration = Duration::from_secs(15);

/// Request timeout for a basis extraction. Basis rides the same call but grows
/// its **output** by 2-4k tokens, and output tokens are the serial-decode term:
/// at the slower providers' 15-30 tok/s that alone exceeds the 60s default.
/// Applied per-request, so the judge and summary paths keep the 60s bound.
const BASIS_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Default UTF-8-safe truncation ceiling on markdown sent to the LLM for
/// structured extraction. Matches the Next.js side's pre-flight cap so the
/// per-call reserve never goes wildly out of band. Pages larger than this
/// can still be processed with an explicit caller-supplied override.
pub const DEFAULT_MAX_INPUT_BYTES: usize = 50_000;

/// Result of a structured-extraction LLM call: the validated JSON value
/// plus per-call token usage and a `truncated` flag indicating whether the
/// markdown input was clipped at [`DEFAULT_MAX_INPUT_BYTES`] (or the
/// caller-supplied override) before being sent to the LLM.
///
/// The `basis*` / `llm_input_hash` fields are populated only by
/// [`extract_structured_with_basis`]; they stay empty on the plain path.
#[derive(Debug, Clone, Default)]
pub struct StructuredExtractResult {
    pub value: serde_json::Value,
    pub usage: Option<LlmUsage>,
    pub truncated: bool,
    /// Per-field evidence, one entry per top-level scalar schema property.
    pub basis: Vec<Basis>,
    /// Coded explanations for every basis downgrade. Never upstream text.
    pub basis_warnings: Vec<BasisWarning>,
    /// `"sha256:"`-prefixed hash of the canonical source text — the exact
    /// (truncated) markdown sent to the model. This is the document-map key a
    /// consumer verifies `EvidenceCitation.source_hash` against; it is recorded
    /// even when no citation survived, so the check is not circular.
    pub llm_input_hash: Option<String>,
}

/// UTF-8-safe truncation: clip at `max_bytes` but walk back to the nearest
/// char boundary so we never split a multibyte sequence. Returns
/// `(truncated_slice, was_truncated)`.
pub(crate) fn truncate_md(s: &str, max_bytes: usize) -> (&str, bool) {
    if s.len() <= max_bytes {
        return (s, false);
    }
    let mut idx = max_bytes;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    (&s[..idx], true)
}

/// Shared HTTP client for LLM API calls (avoids per-request connection overhead).
fn shared_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(LLM_REQUEST_TIMEOUT)
            .build()
            .unwrap_or_default()
    })
}

/// Validate a JSON value against a JSON schema.
pub(crate) fn validate_against_schema(
    value: &serde_json::Value,
    schema: &serde_json::Value,
) -> CrwResult<()> {
    let validator = jsonschema::validator_for(schema)
        .map_err(|e| CrwError::ExtractionError(format!("Invalid JSON schema: {e}")))?;
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect();
    if !errors.is_empty() {
        return Err(CrwError::ExtractionError(format!(
            "LLM output failed schema validation:\n{}",
            errors.join("\n")
        )));
    }
    Ok(())
}

/// Extract structured JSON from markdown content using an LLM.
///
/// Backward-compatible thin wrapper: callers that only need the validated
/// JSON value can keep calling this. New callers that also want the LLM
/// token-usage envelope + truncation flag should use
/// [`extract_structured_with_usage`].
pub async fn extract_structured(
    markdown: &str,
    schema: &serde_json::Value,
    llm: &LlmConfig,
) -> CrwResult<serde_json::Value> {
    Ok(
        extract_structured_with_usage(markdown, Some(schema), None, llm, None)
            .await?
            .value,
    )
}

/// Extract structured JSON and return token usage + truncation status.
///
/// `max_input_bytes` overrides the per-call markdown byte ceiling. `None`
/// falls back to [`DEFAULT_MAX_INPUT_BYTES`] (50 KB). Truncation is done
/// on a UTF-8 char boundary; if it occurred, the returned
/// [`StructuredExtractResult::truncated`] is `true` and the
/// `LlmUsage.truncated` field (when usage is present) is also set so
/// downstream billing surfaces can flag pages that were clipped.
pub async fn extract_structured_with_usage(
    markdown: &str,
    schema: Option<&serde_json::Value>,
    user_prompt: Option<&str>,
    llm: &LlmConfig,
    max_input_bytes: Option<usize>,
) -> CrwResult<StructuredExtractResult> {
    extract_inner(markdown, schema, user_prompt, llm, max_input_bytes, None).await
}

/// Extract structured JSON **with per-field evidence** (`basis`).
///
/// Same single LLM call as [`extract_structured_with_usage`]: the model is asked
/// to attribute each top-level scalar field it extracts (source url, verbatim
/// excerpt, confidence) inside the same tool call. The attribution is then
/// verified server-side and deterministically — see [`crate::basis`]. A claim
/// that does not hold up is downgraded, never dressed up: the result never
/// carries a fake attribution.
///
/// Requires a `schema` (evidence is defined per schema leaf) and refuses
/// upfront a schema whose evidence could not fit the model's output cap.
///
/// `source_url` is the document the server fetched. It is what the citations
/// carry and what the model's claimed url is checked against; the model's own
/// string never reaches the wire.
pub async fn extract_structured_with_basis(
    markdown: &str,
    schema: &serde_json::Value,
    user_prompt: Option<&str>,
    llm: &LlmConfig,
    max_input_bytes: Option<usize>,
    source_url: &str,
) -> CrwResult<StructuredExtractResult> {
    if let Some(reason) = basis::reject_reason(schema, llm.max_tokens) {
        return Err(CrwError::InvalidRequest(reason));
    }
    extract_inner(
        markdown,
        Some(schema),
        user_prompt,
        llm,
        max_input_bytes,
        Some(source_url),
    )
    .await
}

/// The one extraction path. `basis_for` carries the document url in basis mode
/// and is `None` otherwise — and every basis behaviour (the tool-schema
/// injection, the prompt section, the longer timeout, the hash, the alignment)
/// is gated on it. With `None` the request bytes are byte-for-byte what they
/// were before basis existed, which is what keeps every existing caller and
/// self-hoster on exactly the path they are on today.
async fn extract_inner(
    markdown: &str,
    schema: Option<&serde_json::Value>,
    user_prompt: Option<&str>,
    llm: &LlmConfig,
    max_input_bytes: Option<usize>,
    basis_for: Option<&str>,
) -> CrwResult<StructuredExtractResult> {
    if llm.api_key.is_empty() {
        return Err(CrwError::ExtractionError(
            "LLM API key is empty. Set [extraction.llm.api_key] or CRW_EXTRACTION__LLM__API_KEY."
                .into(),
        ));
    }

    let max_bytes = max_input_bytes.unwrap_or(DEFAULT_MAX_INPUT_BYTES);
    let (clipped, truncated) = truncate_md(markdown, max_bytes);

    // The canonical source text is `clipped` — the exact bytes the model sees,
    // after cleaning AND truncation. Hash it BEFORE the call, so the hash can
    // never be anything the model had a hand in. (Deliberately not the same
    // hash as `ScrapeData.source_hash`, which covers the full markdown; the
    // citation's `sourceTextKind` is what disambiguates the two.)
    let llm_input_hash =
        basis_for.map(|_| format!("sha256:{}", hex::encode(Sha256::digest(clipped.as_bytes()))));

    // When the caller gave only a prompt (no schema), let the LLM shape the
    // object itself; the tool still needs an input schema, so use a permissive
    // one that accepts any properties.
    let permissive_schema = serde_json::json!({ "type": "object", "additionalProperties": true });
    let base_schema = schema.unwrap_or(&permissive_schema);
    let leaves = basis_for
        .map(|_| basis::scalar_leaves(base_schema))
        .unwrap_or_default();
    // In basis mode the caller's schema stays the document ROOT, with one
    // `basis` property added. Nesting it under a wrapper would break every
    // `"$ref": "#/$defs/..."` a generated schema carries.
    let owned_tool_schema = basis_for.map(|_| basis::tool_schema(base_schema, &leaves));
    let tool_schema = owned_tool_schema.as_ref().unwrap_or(base_schema);

    // The caller-supplied prompt (trusted API input, not scraped content) steers
    // extraction. Fall back to the generic schema-driven instruction when absent.
    let user_prompt = user_prompt.map(str::trim).filter(|p| !p.is_empty());
    // Grounding clause. With the tool forced the model can no longer decline, and
    // `classify_block` only catches WALLS, not thin pages: it returns `None`
    // unconditionally once the markdown trims to >= `http_retry_threshold_bytes`
    // (default 100 — one sentence), so a sparse-but-unblocked stub does reach
    // here. Nothing downstream can catch an invented value either: `align_basis`
    // downgrades the EVIDENCE to unsupported but leaves the value in place, and
    // basis is opt-in anyway. So the instruction itself has to carry the licence
    // to say nothing.
    const GROUNDING: &str = "\nOnly use information present in the Content below. \
         If the content does not contain a value for a property, use null (or an \
         empty array for a list) rather than guessing. Never invent a value, and \
         never infer one from the URL alone.";
    let instruction = match user_prompt {
        Some(p) => format!(
            "Extract structured data from the following content. \
             Follow this instruction: {p}\n\
             Call the extract_data tool with the extracted data.{GROUNDING}"
        ),
        None => format!(
            "Extract structured data from the following content according to the JSON schema. \
             Call the extract_data tool with the extracted data.{GROUNDING}"
        ),
    };
    let evidence = basis_for.map(basis::prompt_section).unwrap_or_default();
    let prompt = format!("{instruction}{evidence}\n\n## Content\n{clipped}");

    let timeout = if basis_for.is_some() {
        BASIS_REQUEST_TIMEOUT
    } else {
        LLM_REQUEST_TIMEOUT
    };

    // One attempt = dispatch, lift the basis out, validate. Wrapped in a loop so a
    // model that returns a schema-violating object gets exactly one more chance,
    // with the concrete validation errors handed back to it. The provider cannot
    // guarantee compliance on the models we run (Azure Foundry does not enforce
    // `strict` on non-OpenAI deployments), so the engine has to.
    let deadline = std::time::Instant::now() + timeout;
    let mut retry_note: Option<String> = None;

    let (value, usage, model_basis) = loop {
        // `timeout` is the budget for the WHOLE operation, not per attempt. The
        // outer envelope is the SaaS poller's 260s, and /v1/extract is an async
        // job so nothing else bounds this — a naive second full-length call could
        // double the wall time and turn a fast, clear schema error into a slow
        // 504 that also cost twice as much.
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());

        let attempt_prompt = match &retry_note {
            None => std::borrow::Cow::Borrowed(&prompt),
            Some(note) => std::borrow::Cow::Owned(format!("{prompt}{note}")),
        };

        // Force the tool ONLY when a caller schema exists. Forcing removes the
        // model's ability to decline, and on a prompt-only extraction there is no
        // schema and therefore no validation (see below) — so a forced call on a
        // sparse page would return invented fields with nothing to catch them.
        // With no schema we keep today's behaviour: the model may answer in prose
        // and the text fallback fails loudly instead of fabricating.
        let force_tool = schema.is_some();

        let dispatched = match llm.provider.as_str() {
            "anthropic" => {
                call_anthropic(
                    &attempt_prompt,
                    tool_schema,
                    llm,
                    "extract_data",
                    "Extract structured data from the content",
                    remaining,
                    force_tool,
                )
                .await
            }
            "openai" | "deepseek" | "openai-compatible" => {
                call_openai(
                    &attempt_prompt,
                    tool_schema,
                    llm,
                    "extract_data",
                    "Extract structured data from the content",
                    remaining,
                    force_tool,
                )
                .await
            }
            "openai-responses" => {
                call_responses(
                    &attempt_prompt,
                    tool_schema,
                    llm,
                    "extract_data",
                    "Extract structured data from the content",
                    remaining,
                    force_tool,
                )
                .await
            }
            other => Err(CrwError::ExtractionError(format!(
                "Unsupported LLM provider: {other}. Use 'anthropic', 'openai', 'deepseek', 'openai-compatible', or 'openai-responses'."
            ))),
        };
        // A malformed tool-call payload is retryable for the same reason a schema
        // violation is: it is the model's output, not our request, and it is not
        // deterministic. Observed live under concurrency — a tool call truncated
        // at ~1.9k characters, far below the 4k token cap, so a second attempt
        // has a real chance. Only THIS error class retries; a provider HTTP
        // error, a bad config or an unsupported provider is not the model being
        // flaky and must surface immediately.
        let (mut value, mut usage) = match dispatched {
            Ok(v) => v,
            Err(e) => {
                let retryable = matches!(&e, CrwError::ExtractionError(m)
                    if m.contains("function call arguments") || m.contains("returned invalid JSON"));
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if !retryable || retry_note.is_some() || left < MIN_RETRY_BUDGET {
                    return Err(e);
                }
                crw_core::metrics::metrics().structured_retries_total.inc();
                tracing::warn!(error = %e, "model returned an unparseable tool call; retrying once");
                retry_note = Some(format!(
                    "\n\n## Correction\nYour previous tool call could not be parsed: {e}\n\
                     Call the extract_data tool again and return ONE complete, valid JSON object."
                ));
                continue;
            }
        };

        if truncated && let Some(u) = usage.as_mut() {
            u.truncated = true;
        }

        // Lift the basis out of the response BEFORE validation: what remains is
        // the caller's own object, which is what their schema describes. A model
        // that ignored the basis instruction entirely still produces a valid
        // extract — every leaf just lands `unsupported`. Degrade honestly, never
        // hard-fail. Re-done per attempt: the retry has its own fresh response,
        // and validating the already-stripped object again could never succeed.
        let model_basis = basis_for
            .and_then(|_| value.as_object_mut())
            .and_then(|o| o.remove("basis"));

        // Only validate against a caller-supplied schema; a prompt-only
        // extraction has no contract to check the permissive result against.
        let Some(schema) = schema else {
            break (value, usage, model_basis);
        };
        let Err(err) = validate_against_schema(&value, schema) else {
            break (value, usage, model_basis);
        };

        // Second failure, or not enough budget left to be worth spending: give
        // the caller the validation error, which is the honest answer.
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if retry_note.is_some() || left < MIN_RETRY_BUDGET {
            return Err(err);
        }

        crw_core::metrics::metrics().structured_retries_total.inc();
        tracing::warn!(
            error = %err,
            "structured extraction failed schema validation; retrying once with the errors fed back"
        );
        retry_note = Some(format!(
            "\n\n## Correction\nYour previous tool call did not satisfy the schema:\n{err}\n\
             Call the extract_data tool again and return the COMPLETE object, with \
             every required property present."
        ));
        // NOTE: usage from this failed attempt is deliberately DROPPED, not
        // merged. `LlmUsage::merge` would be the obvious move and its doc comment
        // says "tokens add up", but the SaaS bills the customer straight off these
        // token counts with a 3x markup, so merging would charge them for a retry
        // that exists because OUR call was unreliable. We absorb it. The retry is
        // still visible: `calls` below, and the counter above.
    };

    // `value` is now schema-validated and authoritative. The model's claims are
    // checked against it and against the bytes we actually sent; they never
    // rewrite it.
    let (basis, basis_warnings) = match (basis_for, &llm_input_hash) {
        (Some(url), Some(hash)) => basis::align_basis(
            base_schema,
            &value,
            model_basis.as_ref(),
            url,
            hash,
            clipped,
        ),
        _ => (vec![], vec![]),
    };

    // Deliberate exception to `LlmUsage::merge`'s "tokens add up, calls counts the
    // legs" contract: on a retried extraction `calls` is 2 while the tokens are
    // one leg's. Do NOT "fix" this into a merge — that reintroduces the customer
    // double-charge described above.
    let usage = match (usage, retry_note.is_some()) {
        (Some(mut u), true) => {
            u.calls = u.calls.saturating_add(1);
            Some(u)
        }
        (u, _) => u,
    };

    Ok(StructuredExtractResult {
        value,
        usage,
        truncated,
        basis,
        basis_warnings,
        llm_input_hash,
    })
}

// ── Anthropic ──

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    /// Force the single tool, same reason as [`OpenAiRequest::tool_choice`].
    /// Anthropic spells it `{"type":"tool","name":...}`. Note the Messages API
    /// rejects forcing when MANUAL extended thinking is enabled; nothing wires
    /// `thinking` into `LlmConfig` today, so this is unconditional. If that ever
    /// changes, this has to become conditional with it.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize, Default)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[allow(dead_code)]
        id: String,
        #[allow(dead_code)]
        name: String,
        input: serde_json::Value,
    },
}

/// Call Anthropic with a tool-use forcing the given `schema`. `prompt` is the
/// full user message; `tool_name`/`tool_desc` name the forced tool. Shared by
/// structured extraction and the change-tracking judge.
///
/// `timeout` is per-request (it overrides the shared client's default), because
/// a basis extraction decodes several thousand more output tokens than a judge
/// call and must not drag the judge's bound up with it.
pub(crate) async fn call_anthropic(
    prompt: &str,
    schema: &serde_json::Value,
    llm: &LlmConfig,
    tool_name: &str,
    tool_desc: &str,
    timeout: Duration,
    force_tool: bool,
) -> CrwResult<(serde_json::Value, Option<LlmUsage>)> {
    // D reserved lane (covers structured JSON + the change-tracking judge, which
    // both route through here). Held across the provider HTTP call.
    let _llm_permit = crate::llm_gate::acquire_llm().await;
    let url = anthropic_messages_url(llm.base_url.as_deref(), "https://api.anthropic.com");

    let client = shared_client();
    // Same drop-the-force-once guard as the OpenAI transport: a proxy in front of
    // the Messages API that does not accept `tool_choice` must not become a hard
    // extraction failure where it works today.
    let mut forcing = force_tool;
    let (status, text) = loop {
        let body = AnthropicRequest {
            model: llm.model.clone(),
            max_tokens: llm.max_tokens,
            messages: vec![Message {
                role: "user".into(),
                content: prompt.to_string(),
            }],
            tools: Some(vec![AnthropicTool {
                name: tool_name.into(),
                description: tool_desc.into(),
                input_schema: schema.clone(),
            }]),
            tool_choice: forcing.then(|| serde_json::json!({ "type": "tool", "name": tool_name })),
        };

        let resp = crate::llm::send_provider_post(
            client
                .post(&url)
                .timeout(timeout)
                .header("x-api-key", &llm.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body),
        )
        .await
        .map_err(|e| {
            CrwError::ExtractionError(format!(
                "Anthropic API request failed: {}",
                crw_core::error::reqwest_message(e)
            ))
        })?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            CrwError::ExtractionError(format!(
                "Failed to read Anthropic response ({status}): {}",
                crw_core::error::reqwest_message(e)
            ))
        })?;

        if forcing && status.is_client_error() {
            tracing::warn!(
                %status,
                "provider rejected a forced tool_choice; retrying without it"
            );
            forcing = false;
            continue;
        }
        break (status, text);
    };

    if !status.is_success() {
        // NOTE: body may contain the request echoed back by some gateways.
        // The HTTP status code is enough — do not leak the body. This error
        // text reaches the API caller verbatim in the response envelope, and
        // on the managed path the echoed request carries the SERVER key.
        return Err(CrwError::ExtractionError(format!(
            "Anthropic API error ({status})"
        )));
    }

    let parsed: AnthropicResponse = serde_json::from_str(&text).map_err(|e| {
        CrwError::ExtractionError(format!("Failed to parse Anthropic response: {e}"))
    })?;

    let usage = parsed.usage.as_ref().map(|u| {
        let (cache_hit, cache_miss) =
            match (u.cache_read_input_tokens, u.cache_creation_input_tokens) {
                (None, None) => (None, None),
                (read, create) => {
                    let hit = read.unwrap_or(0);
                    let create = create.unwrap_or(0);
                    let miss = u.input_tokens.saturating_add(create);
                    (Some(hit), Some(miss))
                }
            };
        LlmUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            total_tokens: u.input_tokens + u.output_tokens,
            estimated_cost_usd: pricing::calculate_cost(
                &llm.model,
                u.input_tokens,
                u.output_tokens,
            ),
            model: llm.model.clone(),
            provider: "anthropic".to_string(),
            cache_hit_input_tokens: cache_hit,
            cache_miss_input_tokens: cache_miss,
            truncated: false,
            calls: 1,
            // R1 counters are aggregated in the /v1/search caller;
            // single-call sites always emit defaults.
            executed_summaries: 0,
            answer_executed: false,
        }
    });

    // Try tool_use blocks first (structured output).
    for block in &parsed.content {
        if let AnthropicContentBlock::ToolUse { input, .. } = block {
            return Ok((input.clone(), usage));
        }
    }

    // Fallback: extract from text blocks.
    let raw_text: String = parsed
        .content
        .into_iter()
        .filter_map(|c| match c {
            AnthropicContentBlock::Text { text } => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    let value = parse_json_response(&raw_text)?;
    Ok((value, usage))
}

// ── OpenAI ──

#[derive(Serialize)]
struct OpenAiRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAiToolDef>>,
    /// Only the FIRST tool call is consumed below, so never invite more than
    /// one. Forcing a tool raises the odds of multi-call emission on gateways
    /// that support it, and silently dropping calls 2..n would look exactly like
    /// the partial extraction this change exists to fix. `responses.rs` has
    /// always set this.
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    /// Force the single tool. Without it the endpoint defaults to
    /// `tool_choice: "auto"` and the model may answer in prose, or emit a tool
    /// call that ignores half the schema — measured at 7/34 complete responses
    /// against 34/34 with the tool forced. `responses.rs` has always forced it;
    /// this is the chat-completions transport catching up.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    /// Reasoning budget, forwarded when configured.
    ///
    /// This builder is separate from the one in `llm.rs`, which has always sent
    /// this field, and the omission here is not cosmetic: on a reasoning model
    /// the default budget is spent on every call. Measured on `deepseek-flash`
    /// over four pages, this path spends 8086 completion tokens with reasoning
    /// left at its default and 201 with it set to "none". Structured extraction
    /// is the highest-volume LLM path (`formats:["json"]`, `/v1/extract`,
    /// `/v2/parse`) and the change-tracking judge shares this builder, so the
    /// difference lands on nearly every managed call.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
}

#[derive(Serialize)]
struct OpenAiToolDef {
    r#type: String,
    function: OpenAiFunctionDef,
}

#[derive(Serialize)]
struct OpenAiFunctionDef {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize, Default)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: Option<u32>,
    #[serde(default)]
    prompt_cache_hit_tokens: Option<u32>,
    #[serde(default)]
    prompt_cache_miss_tokens: Option<u32>,
    #[serde(default)]
    prompt_tokens_details: Option<OpenAiPromptDetails>,
}

#[derive(Deserialize, Default)]
struct OpenAiPromptDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Deserialize)]
struct OpenAiToolCall {
    function: OpenAiFunctionCall,
}

#[derive(Deserialize)]
struct OpenAiFunctionCall {
    #[allow(dead_code)]
    name: String,
    arguments: String,
}

/// Resolve the chat-completions endpoint for an OpenAI-compatible provider.
///
/// For a user-supplied `base_url` this matches the summary path in
/// `llm::call_openai`, so a single value works for both: the `base_url` carries
/// the API-version segment (the OpenAI `/v1` convention) and we append only
/// `/chat/completions`; a base already pointing at `…/chat/completions` is used
/// verbatim. The `None` branch intentionally diverges from the summary path:
/// here we honour the per-provider `default_base` (e.g. DeepSeek), whereas the
/// summary path hardcodes the OpenAI default — both append `/v1/chat/completions`
/// to a bare host.
///
/// This avoids the doubling bug where a `base_url` of `…/v1` became
/// `…/v1/v1/chat/completions` (→ 405) on the structured path while the summary
/// path correctly hit `…/v1/chat/completions`.
fn openai_chat_url(base_url: Option<&str>, default_base: &str) -> String {
    match base_url {
        Some(b) if b.contains("/chat/completions") => b.to_string(),
        Some(b) => format!("{}/chat/completions", b.trim_end_matches('/')),
        None => format!("{}/v1/chat/completions", default_base.trim_end_matches('/')),
    }
}

/// Resolve the messages endpoint for an Anthropic-compatible provider.
///
/// The same bug class as `openai_chat_url`: the Anthropic **summary** path
/// (`llm::call_anthropic`) consumes `base_url` verbatim (its default is the full
/// `…/v1/messages`), so a bare host that works here would 404 there. To let a
/// single full `…/v1/messages` endpoint satisfy both paths, this is idempotent:
/// a base already ending in `/v1/messages` is used verbatim; one ending in `/v1`
/// gets `/messages`; a bare host (or `None`) gets the full `/v1/messages` suffix.
/// We treat a base as already-complete only when `/v1/messages` is its true
/// suffix (`ends_with`, after trimming a trailing slash) rather than merely
/// present (`contains`), so a path that happens to embed `/v1/messages`
/// mid-string is not mistaken for a finished endpoint.
fn anthropic_messages_url(base_url: Option<&str>, default_base: &str) -> String {
    let b = match base_url {
        Some(b) => b,
        None => return format!("{}/v1/messages", default_base.trim_end_matches('/')),
    };
    let trimmed = b.trim_end_matches('/');
    if trimmed.ends_with("/v1/messages") {
        trimmed.to_string()
    } else if trimmed.ends_with("/v1") {
        format!("{trimmed}/messages")
    } else {
        format!("{trimmed}/v1/messages")
    }
}

/// Call an OpenAI-compatible provider with a function-call forcing the given
/// `schema`. `prompt` is the full user message; `tool_name`/`tool_desc` name
/// the forced function. Shared by structured extraction and the judge.
///
/// `timeout` is per-request — see [`call_anthropic`].
pub(crate) async fn call_openai(
    prompt: &str,
    schema: &serde_json::Value,
    llm: &LlmConfig,
    tool_name: &str,
    tool_desc: &str,
    timeout: Duration,
    force_tool: bool,
) -> CrwResult<(serde_json::Value, Option<LlmUsage>)> {
    // D reserved lane (structured JSON + judge). Held across the HTTP call.
    let _llm_permit = crate::llm_gate::acquire_llm().await;
    let default_base = match llm.provider.as_str() {
        "deepseek" => "https://api.deepseek.com",
        _ => "https://api.openai.com",
    };
    let url = openai_chat_url(llm.base_url.as_deref(), default_base);

    let client = shared_client();
    // Force the tool, but do not let that force become a new way to fail. An
    // arbitrary `base_url` is explicitly supported config, and a gateway that
    // does not implement the `tool_choice` object form would reject the whole
    // request — and the "model answered in prose" fallback below is only
    // reachable on a 200. So on a 4xx from a forced request, drop the force once
    // and retry: a setup that works today keeps working.
    let mut forcing = force_tool;
    let (status, text) = loop {
        let body = OpenAiRequest {
            model: llm.model.clone(),
            max_tokens: llm.max_tokens,
            messages: vec![Message {
                role: "user".into(),
                content: prompt.to_string(),
            }],
            tools: Some(vec![OpenAiToolDef {
                r#type: "function".into(),
                function: OpenAiFunctionDef {
                    name: tool_name.into(),
                    description: tool_desc.into(),
                    parameters: schema.clone(),
                },
            }]),
            parallel_tool_calls: forcing.then_some(false),
            tool_choice: forcing.then(
                || serde_json::json!({ "type": "function", "function": { "name": tool_name } }),
            ),
            // Same guard as `llm.rs`: a configured-but-empty value serializes as
            // `""` and providers that validate the field answer 400, so an empty
            // string counts as unset.
            reasoning_effort: llm
                .reasoning_effort
                .as_deref()
                .filter(|v| !v.is_empty())
                .map(str::to_string),
        };

        let resp = crate::llm::send_provider_post(
            client
                .post(&url)
                .timeout(timeout)
                .header("Authorization", format!("Bearer {}", llm.api_key))
                .header("content-type", "application/json")
                .json(&body),
        )
        .await
        .map_err(|e| {
            CrwError::ExtractionError(format!(
                "OpenAI API request failed: {}",
                crw_core::error::reqwest_message(e)
            ))
        })?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            CrwError::ExtractionError(format!(
                "Failed to read OpenAI response ({status}): {}",
                crw_core::error::reqwest_message(e)
            ))
        })?;

        if forcing && status.is_client_error() {
            tracing::warn!(
                %status,
                "provider rejected a forced tool_choice; retrying without it"
            );
            forcing = false;
            continue;
        }
        break (status, text);
    };

    if !status.is_success() {
        // Same rule as the Anthropic branch above: never echo the provider
        // body back to the API caller.
        return Err(CrwError::ExtractionError(format!(
            "OpenAI API error ({status})"
        )));
    }

    let parsed: OpenAiResponse = serde_json::from_str(&text)
        .map_err(|e| CrwError::ExtractionError(format!("Failed to parse OpenAI response: {e}")))?;

    let usage = parsed.usage.as_ref().map(|u| {
        let total = u
            .total_tokens
            .unwrap_or_else(|| u.prompt_tokens + u.completion_tokens);
        let openai_cached = u
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens);
        let (cache_hit, cache_miss) = match (
            u.prompt_cache_hit_tokens,
            u.prompt_cache_miss_tokens,
            openai_cached,
        ) {
            (Some(h), Some(m), _) => (Some(h), Some(m)),
            (Some(h), None, _) => (Some(h), Some(u.prompt_tokens.saturating_sub(h))),
            (None, Some(m), _) => (Some(u.prompt_tokens.saturating_sub(m)), Some(m)),
            (None, None, Some(c)) => (Some(c), Some(u.prompt_tokens.saturating_sub(c))),
            (None, None, None) => (None, None),
        };
        LlmUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            total_tokens: total,
            estimated_cost_usd: pricing::calculate_cost(
                &llm.model,
                u.prompt_tokens,
                u.completion_tokens,
            ),
            model: llm.model.clone(),
            // NOTE: structured.rs is reached only when the dispatcher in
            // extract_structured() matched "openai". DeepSeek goes through
            // the lib.rs/llm.rs path and is tagged correctly there. If a
            // future caller routes DeepSeek through this file, this tag
            // must thread through too.
            provider: llm.provider.clone(),
            cache_hit_input_tokens: cache_hit,
            cache_miss_input_tokens: cache_miss,
            truncated: false,
            calls: 1,
            // R1 counters are aggregated in the /v1/search caller;
            // single-call sites always emit defaults.
            executed_summaries: 0,
            answer_executed: false,
        }
    });

    let choice = parsed
        .choices
        .first()
        .ok_or_else(|| CrwError::ExtractionError("OpenAI returned no choices".into()))?;

    // Try tool_calls first (function calling).
    if let Some(tool_calls) = &choice.message.tool_calls
        && let Some(call) = tool_calls.first()
    {
        let value: serde_json::Value =
            serde_json::from_str(&call.function.arguments).map_err(|e| {
                CrwError::ExtractionError(format!(
                    "Failed to parse OpenAI function call arguments: {e}"
                ))
            })?;
        return Ok((value, usage));
    }

    // Fallback: extract from content text.
    let raw_text = choice.message.content.clone().unwrap_or_default();
    let value = parse_json_response(&raw_text)?;
    Ok((value, usage))
}

/// Call an OpenAI Responses-compatible provider with a forced function tool.
/// The transport is shared with the text-generation path; this wrapper owns the
/// structured/judge concurrency permit just like [`call_openai`].
pub(crate) async fn call_responses(
    prompt: &str,
    schema: &serde_json::Value,
    llm: &LlmConfig,
    tool_name: &str,
    tool_desc: &str,
    timeout: Duration,
    force_tool: bool,
) -> CrwResult<(serde_json::Value, Option<LlmUsage>)> {
    let _llm_permit = crate::llm_gate::acquire_llm().await;
    crate::responses::call_tool(
        llm, prompt, schema, tool_name, tool_desc, timeout, force_tool,
    )
    .await
}

/// Parse JSON from LLM response, stripping markdown fences if present.
pub(crate) fn parse_json_response(text: &str) -> CrwResult<serde_json::Value> {
    let trimmed = text.trim();

    // Strip ```json ... ``` fences if LLM wrapped it
    let json_str = if trimmed.starts_with("```") {
        let inner = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .unwrap_or(trimmed);
        inner.strip_suffix("```").unwrap_or(inner).trim()
    } else {
        trimmed
    };

    serde_json::from_str(json_str).map_err(|e| {
        CrwError::ExtractionError(format!(
            "LLM returned invalid JSON: {e}\nResponse preview: {}",
            truncate_for_error(text)
        ))
    })
}

/// Truncate text for error messages to avoid leaking large responses.
fn truncate_for_error(text: &str) -> &str {
    if text.len() > 200 {
        &text[..text.floor_char_boundary(200)]
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_llm(provider: &str, base_url: String) -> LlmConfig {
        LlmConfig {
            provider: provider.into(),
            api_key: "test-key".into(),
            model: "test-model".into(),
            base_url: Some(base_url),
            max_tokens: 4096,
            ..Default::default()
        }
    }

    fn anthropic_tool_use_response(value: serde_json::Value) -> serde_json::Value {
        json!({
            "content": [{ "type": "tool_use", "id": "t1", "name": "extract_data", "input": value }],
            "usage": { "input_tokens": 20, "output_tokens": 10 }
        })
    }

    fn openai_tool_call_response(value: &serde_json::Value) -> serde_json::Value {
        json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "function": { "name": "extract_data", "arguments": value.to_string() }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 30, "completion_tokens": 15, "total_tokens": 45 }
        })
    }

    #[test]
    fn test_validate_against_schema_success() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" }
            },
            "required": ["name"]
        });
        let value = json!({ "name": "Alice", "age": 30 });
        assert!(validate_against_schema(&value, &schema).is_ok());
    }

    #[test]
    fn test_validate_against_schema_missing_required() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" }
            },
            "required": ["name"]
        });
        let value = json!({ "age": 30 });
        let err = validate_against_schema(&value, &schema).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("schema validation"), "got: {msg}");
    }

    /// jsonschema 0.48.1 fixed an upstream bug where `required` errors went
    /// missing from `evaluate()` on a schema pairing `properties` with a
    /// TWO-entry `required` array. Before that fix an LLM could omit a required
    /// field and still validate clean, so the caller got a silently incomplete
    /// extraction. This pins the exact shape, since the single-entry case above
    /// never reproduced it.
    #[test]
    fn test_validate_against_schema_missing_one_of_two_required() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "email": { "type": "string" }
            },
            "required": ["name", "email"]
        });

        // Second of the two missing.
        let err = validate_against_schema(&json!({ "name": "Alice" }), &schema).unwrap_err();
        assert!(
            err.to_string().contains("schema validation"),
            "missing `email` must fail, got: {err}"
        );

        // First of the two missing.
        let err = validate_against_schema(&json!({ "email": "a@b.c" }), &schema).unwrap_err();
        assert!(
            err.to_string().contains("schema validation"),
            "missing `name` must fail, got: {err}"
        );

        // Both present still passes, so the guard is not over-rejecting.
        assert!(
            validate_against_schema(&json!({ "name": "Alice", "email": "a@b.c" }), &schema).is_ok()
        );
    }

    #[test]
    fn test_validate_against_schema_wrong_type() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"]
        });
        let value = json!({ "name": 123 });
        let err = validate_against_schema(&value, &schema).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("schema validation"), "got: {msg}");
    }

    #[test]
    fn test_parse_json_response_plain() {
        let result = parse_json_response(r#"{"key": "value"}"#).unwrap();
        assert_eq!(result, json!({"key": "value"}));
    }

    #[test]
    fn test_parse_json_response_with_fences() {
        let result = parse_json_response("```json\n{\"key\": \"value\"}\n```").unwrap();
        assert_eq!(result, json!({"key": "value"}));
    }

    #[test]
    fn truncate_md_passes_through_short_input() {
        let s = "hello world";
        let (out, was) = truncate_md(s, 50_000);
        assert_eq!(out, s);
        assert!(!was);
    }

    #[test]
    fn truncate_md_clips_at_default_50k_byte_cutoff() {
        // Build a payload larger than DEFAULT_MAX_INPUT_BYTES (50_000) where
        // a multibyte char STRADDLES the 50_000-byte boundary. The 4-byte
        // rocket emoji at byte 49_998 occupies bytes 49_998..=50_001; a
        // naive slice at 50_000 would split it and panic. The safe
        // truncation must walk back to byte 49_998.
        let prefix = "a".repeat(49_998);
        let big = format!("{prefix}🚀{}", "z".repeat(10_000));
        assert!(big.len() > DEFAULT_MAX_INPUT_BYTES);
        let (out, was) = truncate_md(&big, DEFAULT_MAX_INPUT_BYTES);
        assert!(was, "expected truncation to fire above 50 KB");
        assert!(
            out.is_char_boundary(out.len()),
            "truncated slice must end on a UTF-8 char boundary"
        );
        // The walked-back boundary lands before the emoji, NOT mid-emoji.
        assert_eq!(out.len(), 49_998);
        // And the prefix is intact — every byte is 'a'.
        assert!(out.bytes().all(|b| b == b'a'));
    }

    #[test]
    fn truncate_md_honours_explicit_smaller_cap() {
        let s = format!("{}🚀tail", "a".repeat(99));
        let (out, was) = truncate_md(&s, 100);
        assert!(was);
        // 99 'a's fit; emoji starts at byte 99 (4 bytes) — must NOT split.
        assert!(out.len() <= 100);
        assert!(out.is_char_boundary(out.len()));
    }

    #[test]
    fn openai_url_base_ending_in_v1_is_not_doubled() {
        // Regression for the structured-extraction doubling bug: a base_url
        // ending in `/v1` (the OpenAI convention, exactly as the summary path
        // in `llm::call_openai` treats it) must append only `/chat/completions`
        // — never a second `/v1`. Otherwise structured extraction hits
        // `…/v1/v1/chat/completions` (→ 405) while summary hits the right URL.
        assert_eq!(
            openai_chat_url(Some("http://gateway:8080/v1"), "https://api.openai.com"),
            "http://gateway:8080/v1/chat/completions"
        );
    }

    #[test]
    fn openai_url_appends_path_to_base() {
        // The base_url carries the API-version segment (`/v1`), matching the
        // summary path: we only append `/chat/completions`.
        assert_eq!(
            openai_chat_url(
                Some("https://api.deepseek.com/v1"),
                "https://api.openai.com"
            ),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }

    #[test]
    fn openai_url_uses_full_endpoint_verbatim() {
        // Regression: a base_url that already includes the path must NOT be
        // doubled into `…/v1/chat/completions/v1/chat/completions` (→ 404).
        let full = "https://api.deepseek.com/v1/chat/completions";
        assert_eq!(openai_chat_url(Some(full), "https://api.openai.com"), full);
    }

    #[test]
    fn openai_url_falls_back_to_default_base() {
        // The default base is a bare host, so it still gets the full
        // `/v1/chat/completions` suffix.
        assert_eq!(
            openai_chat_url(None, "https://api.openai.com"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn openai_url_trims_trailing_slash() {
        assert_eq!(
            openai_chat_url(
                Some("https://api.deepseek.com/v1/"),
                "https://api.openai.com"
            ),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }

    #[test]
    fn anthropic_url_bare_host_gets_full_suffix() {
        // No user base_url: the bare default gets the full `/v1/messages` suffix.
        assert_eq!(
            anthropic_messages_url(None, "https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
        // A bare-host base_url behaves the same.
        assert_eq!(
            anthropic_messages_url(Some("https://proxy.internal"), "https://api.anthropic.com"),
            "https://proxy.internal/v1/messages"
        );
    }

    #[test]
    fn anthropic_url_base_ending_in_v1_is_not_doubled() {
        // Same bug class as the OpenAI path: a base ending in `/v1` must get only
        // `/messages` appended — never a second `/v1`.
        assert_eq!(
            anthropic_messages_url(
                Some("https://proxy.internal/v1"),
                "https://api.anthropic.com"
            ),
            "https://proxy.internal/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url(
                Some("https://proxy.internal/v1/"),
                "https://api.anthropic.com"
            ),
            "https://proxy.internal/v1/messages"
        );
    }

    #[test]
    fn anthropic_url_full_endpoint_is_verbatim() {
        // A full `…/v1/messages` endpoint — the value that also satisfies the
        // summary path (which uses base_url verbatim) — is used as-is, not doubled.
        assert_eq!(
            anthropic_messages_url(
                Some("https://proxy.internal/v1/messages"),
                "https://api.anthropic.com"
            ),
            "https://proxy.internal/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url(
                Some("https://proxy.internal/v1/messages/"),
                "https://api.anthropic.com"
            ),
            "https://proxy.internal/v1/messages"
        );
    }

    // ── validate_against_schema: nested / arrays / additionalProperties ────

    #[test]
    fn validate_schema_nested_object_missing_required_child_field() {
        let schema = json!({
            "type": "object",
            "properties": {
                "address": {
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }
            },
            "required": ["address"]
        });
        let err = validate_against_schema(&json!({ "address": {} }), &schema).unwrap_err();
        assert!(err.to_string().contains("schema validation"));
        assert!(
            validate_against_schema(&json!({ "address": { "city": "Belgrade" } }), &schema).is_ok()
        );
    }

    #[test]
    fn validate_schema_recursive_via_refs() {
        // Self-referencing schema: a tree node with optional `children`.
        let schema = json!({
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "children": {
                            "type": "array",
                            "items": { "$ref": "#/$defs/Node" }
                        }
                    },
                    "required": ["name"]
                }
            },
            "$ref": "#/$defs/Node"
        });
        let good = json!({
            "name": "root",
            "children": [{ "name": "child", "children": [] }]
        });
        assert!(validate_against_schema(&good, &schema).is_ok());

        // A grandchild missing the required `name` must fail, several levels deep.
        let bad = json!({
            "name": "root",
            "children": [{ "name": "child", "children": [{}] }]
        });
        assert!(validate_against_schema(&bad, &schema).is_err());
    }

    #[test]
    fn validate_schema_array_item_type_mismatch() {
        let schema = json!({
            "type": "object",
            "properties": {
                "tags": { "type": "array", "items": { "type": "string" } }
            }
        });
        let err =
            validate_against_schema(&json!({ "tags": ["ok", 5, "also ok"] }), &schema).unwrap_err();
        assert!(err.to_string().contains("schema validation"));
        assert!(validate_against_schema(&json!({ "tags": ["a", "b"] }), &schema).is_ok());
    }

    #[test]
    fn validate_schema_array_min_max_items() {
        let schema = json!({
            "type": "object",
            "properties": {
                "tags": { "type": "array", "minItems": 1, "maxItems": 2 }
            }
        });
        assert!(validate_against_schema(&json!({ "tags": [] }), &schema).is_err());
        assert!(validate_against_schema(&json!({ "tags": [1, 2, 3] }), &schema).is_err());
        assert!(validate_against_schema(&json!({ "tags": [1, 2] }), &schema).is_ok());
    }

    #[test]
    fn validate_schema_additional_properties_false_rejects_extra_field() {
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "additionalProperties": false
        });
        let err =
            validate_against_schema(&json!({ "name": "a", "extra": "nope" }), &schema).unwrap_err();
        assert!(err.to_string().contains("schema validation"));
        assert!(validate_against_schema(&json!({ "name": "a" }), &schema).is_ok());
    }

    #[test]
    fn validate_schema_additional_properties_default_allows_extra_field() {
        // No `additionalProperties` key at all -> the JSON Schema default is
        // permissive, so an extra field must NOT fail validation.
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });
        assert!(validate_against_schema(&json!({ "name": "a", "extra": 1 }), &schema).is_ok());
    }

    #[test]
    fn validate_schema_no_type_coercion_string_for_integer() {
        // JSON Schema does not coerce a numeric-looking string into an integer.
        let schema = json!({
            "type": "object",
            "properties": { "age": { "type": "integer" } },
            "required": ["age"]
        });
        assert!(validate_against_schema(&json!({ "age": "30" }), &schema).is_err());
        assert!(validate_against_schema(&json!({ "age": 30 }), &schema).is_ok());
    }

    #[test]
    fn validate_schema_enum_mismatch() {
        let schema = json!({
            "type": "object",
            "properties": { "status": { "enum": ["active", "inactive"] } }
        });
        assert!(validate_against_schema(&json!({ "status": "unknown" }), &schema).is_err());
        assert!(validate_against_schema(&json!({ "status": "active" }), &schema).is_ok());
    }

    #[test]
    fn validate_schema_one_of_exclusive_match() {
        let schema = json!({
            "oneOf": [
                { "type": "object", "properties": { "kind": { "const": "a" } }, "required": ["kind"] },
                { "type": "object", "properties": { "kind": { "const": "b" } }, "required": ["kind"] }
            ]
        });
        assert!(validate_against_schema(&json!({ "kind": "a" }), &schema).is_ok());
        assert!(validate_against_schema(&json!({ "kind": "c" }), &schema).is_err());
    }

    #[test]
    fn validate_schema_invalid_schema_itself_errors() {
        // `type` is not a recognized JSON Schema type name — the schema itself
        // is invalid, which must surface as "Invalid JSON schema", distinct
        // from a validation-failure error on the *value*.
        let bogus_schema = json!({ "type": "not-a-real-type" });
        let err = validate_against_schema(&json!({}), &bogus_schema).unwrap_err();
        assert!(
            err.to_string().contains("Invalid JSON schema"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_schema_deeply_nested_three_levels() {
        let schema = json!({
            "type": "object",
            "properties": {
                "a": { "type": "object", "properties": { "b": { "type": "object",
                    "properties": { "c": { "type": "string" } }, "required": ["c"] } },
                    "required": ["b"] }
            },
            "required": ["a"]
        });
        assert!(
            validate_against_schema(&json!({ "a": { "b": { "c": "leaf" } } }), &schema).is_ok()
        );
        assert!(validate_against_schema(&json!({ "a": { "b": {} } }), &schema).is_err());
    }

    // ── parse_json_response ─────────────────────────────────────────────

    #[test]
    fn test_parse_json_response_fenced_without_language_tag() {
        let result = parse_json_response("```\n{\"key\": \"value\"}\n```").unwrap();
        assert_eq!(result, json!({"key": "value"}));
    }

    #[test]
    fn test_parse_json_response_trims_surrounding_whitespace() {
        let result = parse_json_response("   \n  {\"key\": \"value\"}  \n\n  ").unwrap();
        assert_eq!(result, json!({"key": "value"}));
    }

    #[test]
    fn test_parse_json_response_empty_string_errors() {
        let err = parse_json_response("").unwrap_err();
        assert!(err.to_string().contains("LLM returned invalid JSON"));
    }

    #[test]
    fn test_parse_json_response_trailing_garbage_after_object_errors() {
        let err = parse_json_response(r#"{"key": "value"} trailing junk"#).unwrap_err();
        assert!(err.to_string().contains("LLM returned invalid JSON"));
    }

    #[test]
    fn test_parse_json_response_error_preview_is_truncated_and_included() {
        // A long non-JSON response must fail with a message that includes a
        // *truncated* preview, not the full multi-kilobyte body.
        let long = "not json ".repeat(200);
        let err = parse_json_response(&long).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Response preview:"), "got: {msg}");
        assert!(
            msg.len() < long.len(),
            "error message must not embed the full body verbatim"
        );
    }

    #[test]
    fn test_parse_json_response_preserves_unicode() {
        let result = parse_json_response(r#"{"city": "Beograd — Београд 🇷🇸"}"#).unwrap();
        assert_eq!(result["city"], "Beograd — Београд 🇷🇸");
    }

    #[test]
    fn test_parse_json_response_array_root() {
        let result = parse_json_response("[1, 2, 3]").unwrap();
        assert_eq!(result, json!([1, 2, 3]));
    }

    // ── truncate_md extras ───────────────────────────────────────────────

    #[test]
    fn truncate_md_zero_cap_yields_empty() {
        let (out, was) = truncate_md("anything", 0);
        assert_eq!(out, "");
        assert!(was);
    }

    #[test]
    fn truncate_md_cap_exactly_at_length_is_not_truncated() {
        let s = "exactly ten";
        let (out, was) = truncate_md(s, s.len());
        assert_eq!(out, s);
        assert!(!was);
    }

    // ── basis::reject_reason, exercised through the public entrypoint ──────
    // (extract_structured_with_basis rejects a bad schema BEFORE any network
    // call, so these are fully hermetic despite calling an `async fn`.)

    #[tokio::test]
    async fn basis_rejects_non_object_root_schema() {
        let llm = mock_llm("anthropic", "http://unused.invalid".into());
        let schema = json!({ "type": "array", "items": { "type": "string" } });
        let err =
            extract_structured_with_basis("md", &schema, None, &llm, None, "https://x.example")
                .await
                .unwrap_err();
        assert!(
            err.to_string()
                .contains("basis requires a 'jsonSchema' of type 'object'"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn basis_rejects_schema_with_basis_property_collision() {
        let llm = mock_llm("anthropic", "http://unused.invalid".into());
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "basis": { "type": "string" }
            }
        });
        let err =
            extract_structured_with_basis("md", &schema, None, &llm, None, "https://x.example")
                .await
                .unwrap_err();
        assert!(err.to_string().contains("name collision"), "got: {err}");
    }

    #[tokio::test]
    async fn basis_rejects_schema_with_zero_scalar_leaves() {
        let llm = mock_llm("anthropic", "http://unused.invalid".into());
        let schema = json!({
            "type": "object",
            "properties": {
                "meta": { "type": "object", "properties": { "x": { "type": "string" } } }
            }
        });
        let err =
            extract_structured_with_basis("md", &schema, None, &llm, None, "https://x.example")
                .await
                .unwrap_err();
        assert!(
            err.to_string()
                .contains("at least one top-level scalar property"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn basis_rejects_schema_too_large_for_max_tokens() {
        let mut llm = mock_llm("anthropic", "http://unused.invalid".into());
        llm.max_tokens = 100; // far below the ~890-token cost of even one leaf
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });
        let err =
            extract_structured_with_basis("md", &schema, None, &llm, None, "https://x.example")
                .await
                .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("basis_schema_too_large"), "got: {msg}");
        assert!(msg.contains("100"), "must name the configured limit: {msg}");
    }

    #[tokio::test]
    async fn basis_rejects_empty_api_key_before_reject_reason() {
        // Empty api_key is checked in `extract_inner`, reached AFTER the
        // `reject_reason` preflight — verify the schema-eligible case still
        // surfaces the api_key error, not a basis error.
        let llm = mock_llm("anthropic", "http://unused.invalid".into());
        let mut llm = llm;
        llm.api_key = String::new();
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let err =
            extract_structured_with_basis("md", &schema, None, &llm, None, "https://x.example")
                .await
                .unwrap_err();
        assert!(
            err.to_string().contains("LLM API key is empty"),
            "got: {err}"
        );
    }

    // ── extract_inner: empty api_key / unsupported provider (no network) ──

    #[tokio::test]
    async fn extract_structured_empty_api_key_errors() {
        let llm = mock_llm("anthropic", "http://unused.invalid".into());
        let mut llm = llm;
        llm.api_key = String::new();
        let schema = json!({ "type": "object" });
        let err = extract_structured("content", &schema, &llm)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("LLM API key is empty"));
    }

    #[tokio::test]
    async fn extract_structured_unsupported_provider_lists_supported_ones() {
        let llm = mock_llm("gemini", "http://unused.invalid".into());
        let schema = json!({ "type": "object" });
        let err = extract_structured("content", &schema, &llm)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Unsupported LLM provider: gemini"),
            "got: {msg}"
        );
        assert!(msg.contains("anthropic"), "got: {msg}");
        assert!(msg.contains("openai-responses"), "got: {msg}");
    }

    // ── Full round trip: anthropic tool_use ────────────────────────────────

    #[tokio::test]
    async fn extract_structured_anthropic_tool_use_round_trip() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(anthropic_tool_use_response(
                    json!({ "name": "Alice", "age": 30 }),
                )),
            )
            .expect(1)
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" }, "age": { "type": "integer" } },
            "required": ["name"]
        });
        let value = extract_structured("Alice is 30 years old.", &schema, &llm)
            .await
            .expect("extraction succeeds");
        assert_eq!(value, json!({ "name": "Alice", "age": 30 }));
    }

    #[tokio::test]
    async fn extract_structured_anthropic_text_fallback_when_no_tool_use_block() {
        // A model that ignores the forced tool and answers in plain text: the
        // fallback path must still recover valid JSON from the text block.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [{ "type": "text", "text": "```json\n{\"name\": \"Bob\"}\n```" }],
                "usage": { "input_tokens": 5, "output_tokens": 5 }
            })))
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let value = extract_structured("Bob.", &schema, &llm).await.unwrap();
        assert_eq!(value, json!({ "name": "Bob" }));
    }

    #[tokio::test]
    async fn extract_structured_anthropic_text_fallback_invalid_json_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [{ "type": "text", "text": "I cannot extract that." }],
                "usage": { "input_tokens": 5, "output_tokens": 5 }
            })))
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object" });
        let err = extract_structured("x", &schema, &llm).await.unwrap_err();
        assert!(err.to_string().contains("LLM returned invalid JSON"));
    }

    #[tokio::test]
    async fn extract_structured_anthropic_error_status_not_echoed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(429).set_body_string("rate limited, retry-after: 5s"),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object" });
        let err = extract_structured("x", &schema, &llm).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("429"), "got: {msg}");
        assert!(
            !msg.contains("retry-after"),
            "body must not be echoed: {msg}"
        );
    }

    #[tokio::test]
    async fn extract_structured_anthropic_malformed_json_body_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw("{{{not json", "application/json"),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object" });
        let err = extract_structured("x", &schema, &llm).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("Failed to parse Anthropic response")
        );
    }

    #[tokio::test]
    async fn extract_structured_anthropic_usage_without_cache_fields() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(anthropic_tool_use_response(json!({ "name": "Cara" }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let res = extract_structured_with_usage("Cara.", Some(&schema), None, &llm, None)
            .await
            .unwrap();
        let usage = res.usage.unwrap();
        assert_eq!(usage.input_tokens, 20);
        assert_eq!(usage.output_tokens, 10);
        assert!(usage.cache_hit_input_tokens.is_none());
        assert!(usage.cache_miss_input_tokens.is_none());
        assert_eq!(usage.provider, "anthropic");
    }

    #[tokio::test]
    async fn extract_structured_anthropic_usage_with_cache_fields() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [{ "type": "tool_use", "id": "t1", "name": "extract_data", "input": { "name": "D" } }],
                "usage": {
                    "input_tokens": 50, "output_tokens": 10,
                    "cache_read_input_tokens": 200, "cache_creation_input_tokens": 30
                }
            })))
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let res = extract_structured_with_usage("D.", Some(&schema), None, &llm, None)
            .await
            .unwrap();
        let usage = res.usage.unwrap();
        assert_eq!(usage.cache_hit_input_tokens, Some(200));
        // miss = plain input_tokens (50) + cache_creation (30)
        assert_eq!(usage.cache_miss_input_tokens, Some(80));
    }

    // ── Full round trip: OpenAI-compatible function calling ────────────────

    #[tokio::test]
    async fn extract_structured_openai_tool_calls_round_trip() {
        let server = MockServer::start().await;
        let value = json!({ "name": "Eve", "age": 22 });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_tool_call_response(&value)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let llm = mock_llm("openai", format!("{}/v1", server.uri()));
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" }, "age": { "type": "integer" } }
        });
        let got = extract_structured("Eve is 22.", &schema, &llm)
            .await
            .unwrap();
        assert_eq!(got, value);
    }

    #[tokio::test]
    async fn extract_structured_deepseek_provider_tag_routes_through_openai_path() {
        let server = MockServer::start().await;
        let value = json!({ "name": "Deep" });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_tool_call_response(&value)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let llm = mock_llm("deepseek", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let res = extract_structured_with_usage("Deep.", Some(&schema), None, &llm, None)
            .await
            .unwrap();
        assert_eq!(res.value, value);
        assert_eq!(res.usage.unwrap().provider, "deepseek");
    }

    #[tokio::test]
    async fn extract_structured_openai_compatible_provider_tag_routes_through_openai_path() {
        let server = MockServer::start().await;
        let value = json!({ "name": "Compat" });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_tool_call_response(&value)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let llm = mock_llm("openai-compatible", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        extract_structured("x", &schema, &llm).await.unwrap();
    }

    #[tokio::test]
    async fn extract_structured_openai_fallback_to_content_when_no_tool_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{ "message": { "content": "{\"name\": \"Fallback\"}" } }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
            })))
            .mount(&server)
            .await;

        let llm = mock_llm("openai", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object" });
        let got = extract_structured("x", &schema, &llm).await.unwrap();
        assert_eq!(got, json!({ "name": "Fallback" }));
    }

    #[tokio::test]
    async fn extract_structured_openai_no_choices_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "choices": [] })))
            .mount(&server)
            .await;

        let llm = mock_llm("openai", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object" });
        let err = extract_structured("x", &schema, &llm).await.unwrap_err();
        assert!(err.to_string().contains("OpenAI returned no choices"));
    }

    #[tokio::test]
    async fn extract_structured_openai_malformed_tool_call_arguments_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": {
                        "tool_calls": [{
                            "function": { "name": "extract_data", "arguments": "{not valid" }
                        }]
                    }
                }]
            })))
            .mount(&server)
            .await;

        let llm = mock_llm("openai", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object" });
        let err = extract_structured("x", &schema, &llm).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("Failed to parse OpenAI function call arguments")
        );
    }

    #[tokio::test]
    async fn extract_structured_openai_error_status_not_echoed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(401).set_body_string("Authorization: Bearer SENTINEL"),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("openai", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object" });
        let err = extract_structured("x", &schema, &llm).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("401"));
        assert!(!msg.contains("SENTINEL"));
    }

    #[tokio::test]
    async fn extract_structured_openai_usage_total_tokens_falls_back_to_sum() {
        let server = MockServer::start().await;
        let value = json!({ "name": "X" });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": { "tool_calls": [{
                        "function": { "name": "extract_data", "arguments": value.to_string() }
                    }] }
                }],
                "usage": { "prompt_tokens": 7, "completion_tokens": 3 }
            })))
            .mount(&server)
            .await;

        let llm = mock_llm("openai", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let res = extract_structured_with_usage("x", Some(&schema), None, &llm, None)
            .await
            .unwrap();
        assert_eq!(res.usage.unwrap().total_tokens, 10);
    }

    #[tokio::test]
    async fn extract_structured_openai_usage_deepseek_style_cache_fields() {
        let server = MockServer::start().await;
        let value = json!({ "name": "X" });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": { "tool_calls": [{
                        "function": { "name": "extract_data", "arguments": value.to_string() }
                    }] }
                }],
                "usage": {
                    "prompt_tokens": 1000, "completion_tokens": 20,
                    "prompt_cache_hit_tokens": 800, "prompt_cache_miss_tokens": 200
                }
            })))
            .mount(&server)
            .await;

        let llm = mock_llm("deepseek", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let res = extract_structured_with_usage("x", Some(&schema), None, &llm, None)
            .await
            .unwrap();
        let usage = res.usage.unwrap();
        assert_eq!(usage.cache_hit_input_tokens, Some(800));
        assert_eq!(usage.cache_miss_input_tokens, Some(200));
    }

    #[tokio::test]
    async fn extract_structured_openai_usage_compat_cached_tokens_style() {
        let server = MockServer::start().await;
        let value = json!({ "name": "X" });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": { "tool_calls": [{
                        "function": { "name": "extract_data", "arguments": value.to_string() }
                    }] }
                }],
                "usage": {
                    "prompt_tokens": 500, "completion_tokens": 20,
                    "prompt_tokens_details": { "cached_tokens": 100 }
                }
            })))
            .mount(&server)
            .await;

        let llm = mock_llm("openai", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let res = extract_structured_with_usage("x", Some(&schema), None, &llm, None)
            .await
            .unwrap();
        let usage = res.usage.unwrap();
        assert_eq!(usage.cache_hit_input_tokens, Some(100));
        assert_eq!(usage.cache_miss_input_tokens, Some(400));
    }

    // ── truncation flag threading ───────────────────────────────────────

    #[tokio::test]
    async fn extract_structured_truncation_flag_set_on_result_and_usage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(anthropic_tool_use_response(json!({ "name": "Truncated" }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let long_markdown = "word ".repeat(1000); // way over a tiny override cap
        let res =
            extract_structured_with_usage(&long_markdown, Some(&schema), None, &llm, Some(50))
                .await
                .unwrap();
        assert!(res.truncated);
        assert!(res.usage.unwrap().truncated);
    }

    #[tokio::test]
    async fn extract_structured_no_truncation_when_within_cap() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(anthropic_tool_use_response(json!({ "name": "Short" }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let res = extract_structured_with_usage("short text", Some(&schema), None, &llm, None)
            .await
            .unwrap();
        assert!(!res.truncated);
        assert!(!res.usage.unwrap().truncated);
    }

    // ── user_prompt threading ────────────────────────────────────────────

    #[tokio::test]
    async fn extract_structured_user_prompt_is_included_in_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(anthropic_tool_use_response(json!({ "name": "P" }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        extract_structured_with_usage(
            "content",
            Some(&schema),
            Some("Only extract the first name, uppercase it."),
            &llm,
            None,
        )
        .await
        .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let sent = body["messages"][0]["content"].as_str().unwrap();
        assert!(sent.contains("Only extract the first name, uppercase it."));
        assert!(sent.contains("Follow this instruction"));
    }

    #[tokio::test]
    async fn extract_structured_blank_user_prompt_falls_back_to_generic_instruction() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(anthropic_tool_use_response(json!({ "name": "P" }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        extract_structured_with_usage("content", Some(&schema), Some("   "), &llm, None)
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let sent = body["messages"][0]["content"].as_str().unwrap();
        assert!(sent.contains("according to the JSON schema"));
    }

    // ── forced tool_choice + one bounded schema-validation retry ─────────

    #[tokio::test]
    async fn openai_request_forces_the_single_tool() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(openai_tool_call_response(&json!({ "name": "P" }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("openai-compatible", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        extract_structured_with_usage("content", Some(&schema), None, &llm, None)
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            body["tool_choice"],
            json!({ "type": "function", "function": { "name": "extract_data" } })
        );
    }

    #[tokio::test]
    async fn anthropic_request_forces_the_single_tool() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(anthropic_tool_use_response(json!({ "name": "P" }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        extract_structured_with_usage("content", Some(&schema), None, &llm, None)
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            body["tool_choice"],
            json!({ "type": "tool", "name": "extract_data" })
        );
    }

    /// A gateway that rejects the `tool_choice` object form must not become a
    /// hard extraction failure where it worked before: drop the force once and
    /// retry, then succeed.
    #[tokio::test]
    async fn openai_4xx_on_forced_tool_choice_retries_without_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(400).set_body_string("unknown field tool_choice"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(openai_tool_call_response(&json!({ "name": "P" }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("openai-compatible", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let out = extract_structured_with_usage("content", Some(&schema), None, &llm, None)
            .await
            .unwrap();
        assert_eq!(out.value["name"], "P");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2, "expected exactly one un-forced retry");
        let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert!(first.get("tool_choice").is_some());
        assert!(
            second.get("tool_choice").is_none(),
            "the retry must not carry the field the gateway rejected"
        );
    }

    /// The incident this whole change exists for: the model returns an object
    /// that does not satisfy the caller's schema. One re-ask, with the concrete
    /// validation errors handed back.
    #[tokio::test]
    async fn schema_violation_is_retried_once_with_the_errors_fed_back() {
        let server = MockServer::start().await;
        // First reply omits the required property entirely (the real failure was
        // an object carrying only the injected `basis`).
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_tool_call_response(&json!({}))),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(openai_tool_call_response(&json!({ "name": "Dub" }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("openai-compatible", format!("{}/v1", server.uri()));
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        });
        let out = extract_structured_with_usage("content", Some(&schema), None, &llm, None)
            .await
            .unwrap();
        assert_eq!(out.value["name"], "Dub");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2, "exactly one retry, never a loop");
        let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        let sent = second["messages"][0]["content"].as_str().unwrap();
        assert!(
            sent.contains("did not satisfy the schema"),
            "the retry must tell the model what was wrong"
        );
        assert!(
            sent.contains("\"name\" is a required property"),
            "the concrete validation error must be fed back, not a generic nudge"
        );

        // Money path: the customer is billed off these tokens with a markup, so
        // the failed attempt's tokens must NOT be added in. One leg's tokens,
        // `calls` = 2 as the breadcrumb that a retry happened.
        let usage = out.usage.expect("usage");
        assert_eq!(usage.input_tokens, 30, "one leg's tokens, not the sum");
        assert_eq!(usage.output_tokens, 15);
        assert_eq!(usage.calls, 2, "the retry stays visible in telemetry");
    }

    /// Observed live under concurrency: a tool call truncated mid-string at ~1.9k
    /// characters, far below the token cap. That is the model being flaky, not a
    /// bad request, so it retries like a schema violation does.
    #[tokio::test]
    async fn an_unparseable_tool_call_is_retried_once() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{ "message": { "tool_calls": [{
                    "function": { "name": "extract_data", "arguments": "{\"name\": \"Du" }
                }] } }]
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(openai_tool_call_response(&json!({ "name": "Dub" }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("openai-compatible", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let out = extract_structured_with_usage("content", Some(&schema), None, &llm, None)
            .await
            .unwrap();
        assert_eq!(out.value["name"], "Dub");
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    /// But a provider-side or config error is NOT the model being flaky: it must
    /// surface on the first attempt rather than burning a second call.
    #[tokio::test]
    async fn a_provider_http_error_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(500).set_body_string("upstream exploded"))
            .mount(&server)
            .await;

        let llm = mock_llm("openai-compatible", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let err = extract_structured_with_usage("content", Some(&schema), None, &llm, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("OpenAI API error"), "{err}");
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "a 5xx is not the model being flaky; do not pay for a second call"
        );
    }

    /// Two bad replies must surface the validation error, not spin.
    #[tokio::test]
    async fn a_second_schema_violation_is_returned_to_the_caller() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_tool_call_response(&json!({}))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("openai-compatible", format!("{}/v1", server.uri()));
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        });
        let err = extract_structured_with_usage("content", Some(&schema), None, &llm, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("failed schema validation"), "{err}");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2, "one retry only");
    }

    /// Regression guard for a real near-miss: an early draft of the plan wrote the
    /// literal `"extract_data"` into tool_choice. The judge passes its own tool
    /// name, so that would have shipped `tools[0].name = "judge_change"` beside
    /// `tool_choice.name = "extract_data"` — a hard 400 on every monitor judge
    /// call, on every provider. Assert the two MATCH, not merely that one exists.
    #[tokio::test]
    async fn forced_tool_choice_names_the_tool_that_was_offered() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{ "message": { "tool_calls": [{
                    "function": { "name": "judge_change", "arguments": "{\"ok\":true}" }
                }] } }]
            })))
            .mount(&server)
            .await;

        let llm = mock_llm("openai-compatible", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "ok": { "type": "boolean" } } });
        call_openai(
            "p",
            &schema,
            &llm,
            "judge_change",
            "desc",
            LLM_REQUEST_TIMEOUT,
            true,
        )
        .await
        .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["tools"][0]["function"]["name"], "judge_change");
        assert_eq!(
            body["tool_choice"]["function"]["name"], body["tools"][0]["function"]["name"],
            "tool_choice must name the tool actually offered"
        );
        assert_eq!(body["parallel_tool_calls"], false);
    }

    /// Forcing removes the model's ability to decline. With no caller schema
    /// there is no validation behind it, so a forced call on a sparse page would
    /// return invented fields with nothing to catch them. Must stay unforced.
    #[tokio::test]
    async fn prompt_only_extraction_does_not_force_the_tool() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(openai_tool_call_response(&json!({ "anything": 1 }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("openai-compatible", format!("{}/v1", server.uri()));
        extract_structured_with_usage("content", None, Some("grab the name"), &llm, None)
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert!(
            body.get("tool_choice").is_none(),
            "prompt-only must not force the tool"
        );
    }

    /// The forced call must ship the licence to say nothing, or a sparse page
    /// becomes fabricated data that validates cleanly and gets billed.
    #[tokio::test]
    async fn the_prompt_tells_the_model_not_to_invent_values() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(openai_tool_call_response(&json!({ "name": "P" }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("openai-compatible", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        extract_structured_with_usage("content", Some(&schema), None, &llm, None)
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let sent = body["messages"][0]["content"].as_str().unwrap();
        assert!(sent.contains("Never invent a value"), "{sent}");
        assert!(sent.contains("never infer one from the URL alone"));
    }

    /// A prompt-only extraction has no schema to violate, so it must never retry.
    #[tokio::test]
    async fn prompt_only_extraction_never_retries() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_tool_call_response(&json!({}))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("openai-compatible", format!("{}/v1", server.uri()));
        extract_structured_with_usage("content", None, Some("grab the name"), &llm, None)
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
    }

    // ── basis end-to-end: attribution lifted out before schema validation ─

    #[tokio::test]
    async fn extract_structured_with_basis_lifts_basis_out_of_returned_value() {
        let server = MockServer::start().await;
        let source_url = "https://example.com/article";
        let model_output = json!({
            "name": "Grace",
            "basis": {
                "name": {
                    "value": "Grace",
                    "sourceUrl": source_url,
                    "excerpt": "Her name is Grace.",
                    "confidence": "high"
                }
            }
        });
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(anthropic_tool_use_response(model_output)),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        });
        let res = extract_structured_with_basis(
            "Her name is Grace.",
            &schema,
            None,
            &llm,
            None,
            source_url,
        )
        .await
        .expect("basis extraction succeeds");

        // The caller's own schema describes ONLY `name` — `basis` must be
        // stripped out before schema validation and before being returned.
        assert_eq!(res.value, json!({ "name": "Grace" }));
        assert!(res.value.get("basis").is_none());
        assert_eq!(res.basis.len(), 1);
        assert!(res.llm_input_hash.unwrap().starts_with("sha256:"));
    }

    #[tokio::test]
    async fn extract_structured_with_basis_model_that_ignores_basis_still_validates() {
        // A model that ignores the basis instruction entirely: the caller's
        // own object is still valid, every leaf just degrades to unsupported
        // rather than hard-failing the whole extraction.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(anthropic_tool_use_response(json!({ "name": "NoBasis" }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        });
        let res = extract_structured_with_basis(
            "content",
            &schema,
            None,
            &llm,
            None,
            "https://example.com",
        )
        .await
        .expect("must not hard-fail when the model omits basis");
        assert_eq!(res.value, json!({ "name": "NoBasis" }));
    }

    #[tokio::test]
    async fn extract_structured_with_basis_invalid_extraction_still_fails_schema() {
        // Basis mode does not weaken schema validation: a missing required
        // field still errors, same as the non-basis path.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(anthropic_tool_use_response(json!({}))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        });
        let err = extract_structured_with_basis(
            "content",
            &schema,
            None,
            &llm,
            None,
            "https://example.com",
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("schema validation"));
    }

    // ── call_anthropic / call_openai direct unit coverage ──────────────────

    #[tokio::test]
    async fn call_anthropic_direct_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(anthropic_tool_use_response(json!({ "ok": true }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object" });
        let (value, usage) = call_anthropic(
            "prompt",
            &schema,
            &llm,
            "extract_data",
            "desc",
            LLM_REQUEST_TIMEOUT,
            true,
        )
        .await
        .unwrap();
        assert_eq!(value, json!({ "ok": true }));
        assert!(usage.is_some());
    }

    #[tokio::test]
    async fn call_openai_direct_success() {
        let server = MockServer::start().await;
        let value = json!({ "ok": true });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_tool_call_response(&value)),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("openai", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object" });
        let (got, usage) = call_openai(
            "prompt",
            &schema,
            &llm,
            "extract_data",
            "desc",
            LLM_REQUEST_TIMEOUT,
            true,
        )
        .await
        .unwrap();
        assert_eq!(got, value);
        assert!(usage.is_some());
    }

    // ── openai-responses provider dispatch ──────────────────────────────

    #[tokio::test]
    async fn extract_structured_openai_responses_provider_routes_and_extracts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "output": [{
                    "type": "function_call",
                    "name": "extract_data",
                    "arguments": "{\"name\": \"Resp\"}"
                }],
                "usage": { "input_tokens": 8, "output_tokens": 4 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let llm = mock_llm("openai-responses", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let got = extract_structured("Resp.", &schema, &llm).await.unwrap();
        assert_eq!(got, json!({ "name": "Resp" }));
    }

    #[tokio::test]
    async fn extract_structured_openai_responses_error_status_not_echoed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(400).set_body_string("SENTINEL-BODY"))
            .mount(&server)
            .await;

        let llm = mock_llm("openai-responses", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object" });
        let err = extract_structured("x", &schema, &llm).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("400"));
        assert!(!msg.contains("SENTINEL-BODY"));
    }

    // ── more schema edge cases ──────────────────────────────────────────

    #[test]
    fn validate_schema_boolean_type() {
        let schema = json!({
            "type": "object",
            "properties": { "active": { "type": "boolean" } },
            "required": ["active"]
        });
        assert!(validate_against_schema(&json!({ "active": true }), &schema).is_ok());
        assert!(validate_against_schema(&json!({ "active": "true" }), &schema).is_err());
    }

    #[test]
    fn validate_schema_number_vs_integer_distinction() {
        let schema = json!({
            "type": "object",
            "properties": { "count": { "type": "integer" } }
        });
        // A float with a fractional part is not a valid integer.
        assert!(validate_against_schema(&json!({ "count": 1.5 }), &schema).is_err());
        // A whole-numbered float IS a valid integer in JSON Schema.
        assert!(validate_against_schema(&json!({ "count": 2.0 }), &schema).is_ok());
    }

    #[test]
    fn validate_schema_null_type_property() {
        let schema = json!({
            "type": "object",
            "properties": { "note": { "type": ["string", "null"] } }
        });
        assert!(validate_against_schema(&json!({ "note": null }), &schema).is_ok());
        assert!(validate_against_schema(&json!({ "note": "hi" }), &schema).is_ok());
        assert!(validate_against_schema(&json!({ "note": 5 }), &schema).is_err());
    }

    #[test]
    fn validate_schema_string_pattern_and_length_constraints() {
        let schema = json!({
            "type": "object",
            "properties": {
                "code": { "type": "string", "pattern": "^[A-Z]{3}$" },
                "bio": { "type": "string", "maxLength": 5 }
            }
        });
        assert!(validate_against_schema(&json!({ "code": "ABC" }), &schema).is_ok());
        assert!(validate_against_schema(&json!({ "code": "abc" }), &schema).is_err());
        assert!(validate_against_schema(&json!({ "bio": "toolong" }), &schema).is_err());
    }

    #[test]
    fn validate_schema_empty_value_against_empty_schema_is_permissive() {
        // The permissive fallback schema `extract_inner` uses when no caller
        // schema is supplied.
        let schema = json!({ "type": "object", "additionalProperties": true });
        assert!(validate_against_schema(&json!({}), &schema).is_ok());
        assert!(validate_against_schema(&json!({ "anything": [1, 2, {"x": 1}] }), &schema).is_ok());
    }

    // ── DEFAULT_MAX_INPUT_BYTES sanity ──────────────────────────────────

    #[test]
    fn default_max_input_bytes_is_fifty_kb() {
        assert_eq!(DEFAULT_MAX_INPUT_BYTES, 50_000);
    }

    // ── basis end-to-end: downgrade paths ──────────────────────────────

    #[tokio::test]
    async fn extract_structured_with_basis_excerpt_not_in_source_downgrades_to_unverified() {
        let server = MockServer::start().await;
        let source_url = "https://example.com/a";
        let model_output = json!({
            "name": "Henry",
            "basis": {
                "name": {
                    "value": "Henry",
                    "sourceUrl": source_url,
                    "excerpt": "this exact phrase is not in the source text",
                    "confidence": "high"
                }
            }
        });
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(anthropic_tool_use_response(model_output)),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        });
        let res = extract_structured_with_basis(
            "His name is Henry, born in 1990.",
            &schema,
            None,
            &llm,
            None,
            source_url,
        )
        .await
        .unwrap();

        assert_eq!(
            res.basis[0].status,
            crw_core::evidence::FieldStatus::Unverified
        );
        assert!(
            res.basis_warnings
                .iter()
                .any(|w| w.field == "name" && w.code == "excerpt_not_in_source"),
            "got: {:?}",
            res.basis_warnings
        );
        // Downgraded attribution never blocks the extraction itself.
        assert_eq!(res.value, json!({ "name": "Henry" }));
    }

    #[tokio::test]
    async fn extract_structured_with_basis_value_mismatch_downgrades_to_unsupported() {
        let server = MockServer::start().await;
        let source_url = "https://example.com/b";
        // The model's own claimed `value` contradicts the actual extracted field.
        let model_output = json!({
            "name": "Iris",
            "basis": {
                "name": {
                    "value": "SomeoneElse",
                    "sourceUrl": source_url,
                    "excerpt": "Iris",
                    "confidence": "low"
                }
            }
        });
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(anthropic_tool_use_response(model_output)),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        });
        let res =
            extract_structured_with_basis("Iris is here.", &schema, None, &llm, None, source_url)
                .await
                .unwrap();

        assert_eq!(
            res.basis[0].status,
            crw_core::evidence::FieldStatus::Unsupported
        );
        assert!(
            res.basis_warnings
                .iter()
                .any(|w| w.code == "basis_value_mismatch")
        );
        // `data` (schema-validated) wins — never rewritten to match the claim.
        assert_eq!(res.value["name"], "Iris");
    }

    #[tokio::test]
    async fn extract_structured_with_basis_missing_field_attribution_is_unsupported() {
        let server = MockServer::start().await;
        let source_url = "https://example.com/c";
        // The model extracted `name` but supplied no basis entry for it at all.
        let model_output = json!({ "name": "Jack", "basis": {} });
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(anthropic_tool_use_response(model_output)),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        });
        let res = extract_structured_with_basis("Jack.", &schema, None, &llm, None, source_url)
            .await
            .unwrap();

        assert_eq!(
            res.basis[0].status,
            crw_core::evidence::FieldStatus::Unsupported
        );
        assert!(res.basis_warnings.iter().any(|w| w.code == "basis_missing"));
    }

    #[tokio::test]
    async fn extract_structured_with_basis_field_absent_from_extraction_is_not_found() {
        let server = MockServer::start().await;
        let source_url = "https://example.com/d";
        // The schema declares a scalar `age` leaf the model never populated.
        let model_output = json!({ "name": "Kim" });
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(anthropic_tool_use_response(model_output)),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" }
            }
        });
        let res = extract_structured_with_basis("Kim.", &schema, None, &llm, None, source_url)
            .await
            .unwrap();

        let age_basis = res.basis.iter().find(|b| b.field == "age").unwrap();
        assert_eq!(age_basis.status, crw_core::evidence::FieldStatus::NotFound);
        assert!(age_basis.value.is_none());
    }

    #[tokio::test]
    async fn extract_structured_with_basis_wrong_source_url_is_unsupported() {
        let server = MockServer::start().await;
        let real_source_url = "https://example.com/real";
        // The model claims a DIFFERENT document than the one the server fetched.
        let model_output = json!({
            "name": "Liam",
            "basis": {
                "name": {
                    "value": "Liam",
                    "sourceUrl": "https://not-the-real-source.example",
                    "excerpt": "Liam",
                    "confidence": "high"
                }
            }
        });
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(anthropic_tool_use_response(model_output)),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });
        let res =
            extract_structured_with_basis("Liam.", &schema, None, &llm, None, real_source_url)
                .await
                .unwrap();

        assert_eq!(
            res.basis[0].status,
            crw_core::evidence::FieldStatus::Unsupported
        );
        assert!(
            res.basis_warnings
                .iter()
                .any(|w| w.code == "basis_source_unknown")
        );
    }

    // ── extract_structured_with_basis: schema without `required` still works ──

    #[tokio::test]
    async fn extract_structured_with_basis_schema_without_required_array() {
        // `tool_schema` must create a `required` array when the caller's schema
        // has none (every field optional) — otherwise `basis` itself would be
        // optional and silently skipped by the model.
        let server = MockServer::start().await;
        let source_url = "https://example.com/e";
        let model_output = json!({
            "title": "Report",
            "basis": {
                "title": {
                    "value": "Report",
                    "sourceUrl": source_url,
                    "excerpt": "Report",
                    "confidence": "medium"
                }
            }
        });
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(anthropic_tool_use_response(model_output)),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        // No `required` key at all.
        let schema = json!({
            "type": "object",
            "properties": { "title": { "type": "string" } }
        });
        let res = extract_structured_with_basis("Report.", &schema, None, &llm, None, source_url)
            .await
            .unwrap();
        assert_eq!(
            res.basis[0].status,
            crw_core::evidence::FieldStatus::Supported
        );
    }

    // ── max_input_bytes override edge cases ─────────────────────────────

    #[tokio::test]
    async fn extract_structured_with_usage_zero_max_input_bytes_truncates_everything() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(anthropic_tool_use_response(json!({ "name": "Z" }))),
            )
            .mount(&server)
            .await;

        let llm = mock_llm("anthropic", format!("{}/v1", server.uri()));
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let res =
            extract_structured_with_usage("non-empty markdown", Some(&schema), None, &llm, Some(0))
                .await
                .unwrap();
        assert!(res.truncated);
    }
}
