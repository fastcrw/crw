//! `POST /v2/scrape` (+ `GET /v2/scrape/{job_id}` Tier-3 stub).

use std::collections::HashMap;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crw_core::Deadline;
use crw_core::error::CrwError;
use crw_core::types::{
    OutputFormat, PARKED_DOMAIN_VENDOR, RequestedRenderer, STRUCTURAL_FAILURE_VENDOR, ScrapeRequest,
};
use crw_crawl::single::scrape_url;

use super::adapters::{V2Document, to_v2_document};
use super::error::V2Error;
use super::formats::{self, FormatSpec, decompose};
use crate::error::AppError;
use crate::state::{AppState, validate_renderer_pin};

/// v2 `/v2/scrape` request. Lenient: unknown fields the SDK may send
/// (`mobile`, `actions`, `blockAds`, `origin`, `integration`, …) are ignored by
/// serde. We must NOT `deny_unknown_fields` or a newer SDK build would 400.
/// `maxAge` and `storeInCache` used to be on that list; they are honoured now.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct V2ScrapeRequest {
    pub url: String,
    #[serde(default = "default_v2_formats")]
    pub formats: Vec<FormatSpec>,
    /// Absent = the server's `[extraction].only_main_content`.
    #[serde(default)]
    pub only_main_content: Option<bool>,
    #[serde(default)]
    pub include_tags: Vec<String>,
    #[serde(default)]
    pub exclude_tags: Vec<String>,
    #[serde(default)]
    pub wait_for: Option<u64>,
    /// How old a cached copy may be, in milliseconds. The SDK has always sent
    /// this and we always ignored it. Firecrawl's own default is two days,
    /// above our published 24 hour ceiling, so an unset value takes our default
    /// and an explicit larger one is clamped rather than honoured.
    #[serde(default, deserialize_with = "lenient_u64")]
    pub max_age: Option<u64>,
    /// Firecrawl's write switch: `false` reads the cache but stores nothing.
    #[serde(default, deserialize_with = "lenient_bool")]
    pub store_in_cache: Option<bool>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// v2 `location` object. `country` is lowercased and mapped to the engine's
    /// 2-letter proxy-egress country.
    #[serde(default)]
    pub location: Option<V2Location>,
    /// v2 proxy mode. Default "auto" (NOT v1's "basic"). "stealth" routes to the
    /// residential chrome tier; everything else is reported as "basic".
    #[serde(default = "default_proxy")]
    pub proxy: String,
    /// BYOP proxy pool (crw extension) rotated per `proxy_rotation`. Distinct
    /// from the `proxy` MODE above — these are actual proxy URLs. Accepts the
    /// snake_case `proxy_list` alias (what the managed layer injects).
    #[serde(default, alias = "proxy_list")]
    pub proxy_list: Vec<String>,
    #[serde(default, alias = "proxy_rotation")]
    pub proxy_rotation: Option<crw_core::proxy::ProxyRotation>,
    /// v2 `timeout` (ms) → engine `deadline_ms`.
    #[serde(default)]
    pub timeout: Option<u64>,
    // BYOK passthrough (same names as v1 so the SaaS header path is unchanged).
    #[serde(default)]
    pub llm_api_key: Option<String>,
    #[serde(default)]
    pub llm_provider: Option<String>,
    #[serde(default)]
    pub llm_model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub summary_prompt: Option<String>,
    /// Optional explicit renderer pin (crw extension, tolerated alongside v2).
    #[serde(default)]
    pub renderer: Option<RequestedRenderer>,
    /// JS-rendering preference (crw extension, tolerated alongside v2 — upstream
    /// Firecrawl has no equivalent). Same semantics as `/v1/scrape`: null =
    /// auto/server default, true = force JS, false = never reach a browser tier.
    /// Accepts the snake_case alias for parity with the v1 wire.
    #[serde(default, alias = "render_js")]
    pub render_js: Option<bool>,
    /// Firecrawl `parsers` — document parsing directives. Accepts `["pdf"]` or
    /// `[{"type":"pdf","maxPages":N}]`. Omitted = auto-parse PDFs; `[]` = leave
    /// raw. See [`crw_core::types::ParserSpec`].
    #[serde(default)]
    pub parsers: Option<Vec<crw_core::types::ParserSpec>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct V2Location {
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub languages: Option<Vec<String>>,
}

fn default_proxy() -> String {
    "auto".to_string()
}
fn default_v2_formats() -> Vec<FormatSpec> {
    vec![FormatSpec::String("markdown".to_string())]
}

/// `{ success, data, warning? }` envelope.
#[derive(Debug, Serialize)]
pub struct V2ScrapeResponse {
    pub success: bool,
    pub data: V2Document,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// Anti-bot block message (vendor + reason); present iff `success == false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Build an `Accept-Language` header value from Firecrawl's `location.languages`
/// list. Blank entries are dropped; an all-blank or empty list yields `None` so
/// no header is added. Values are joined as-is (`["en-US","de"]` → `en-US, de`).
pub(crate) fn accept_language_from(languages: &[String]) -> Option<String> {
    let joined = languages
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    (!joined.is_empty()).then_some(joined)
}

/// What `/v2/scrape` should do with a finished `ScrapeData`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum V2Verdict {
    /// Return the document with `success: true`. Includes every 4xx/5xx page.
    Document,
    /// No engine produced anything usable → a 500 `SCRAPE_ALL_ENGINES_FAILED`.
    NothingUsable,
    /// A real anti-bot wall on an otherwise-fine response → `success: false`.
    Blocked,
}

/// Firecrawl v2's success rule, as one function.
///
/// **The origin's status code never decides success.** Firecrawl's
/// `scrapeURL/index.ts` ("Success factors"):
///
/// ```js
/// const isGoodStatusCode = (s >= 200 && s < 300) || s === 304;
/// const hasRequiredOutput = isParsedImage || isLongEnough || !isGoodStatusCode;
/// ```
///
/// `!isGoodStatusCode` is an OR term, so a bad status is *sufficient* to accept
/// the result — even with an empty body — and `controllers/v2/scrape.ts` then
/// never reads `metadata.statusCode` at all. Captured from the live API against
/// `mock.fastcrw.com`, 401/403/404/429/500/503 each answer:
///
/// ```text
/// HTTP 200  {"success":true,"data":{"metadata":{"statusCode":404,
///            "error":"Not Found", ...}}}
/// ```
///
/// Two things follow, and both are the point of this function:
///
/// 1. The old gate (`ScrapeData::http_error`) was **size-dependent** — it only
///    fires under `ERROR_PAGE_MAX_TEXT` (200 bytes) — so a terse 404 failed and
///    a chatty 404 succeeded. Nothing here is size-dependent.
/// 2. An empty body is only a failure when the status was *good*, because
///    `isLongEnough` and `!isGoodStatusCode` are alternatives. A 404 with an
///    empty body is a success; a 200 with an empty body is not.
///
/// `/v1` is untouched: `http_error()` is still what the native surface, crawl
/// and batch consult. Only this Firecrawl-compat surface stops asking.
pub(crate) fn v2_verdict(
    status_code: u16,
    block: Option<&crw_core::types::BlockOutcome>,
    has_no_content: bool,
) -> V2Verdict {
    // A 4xx/5xx page is a document, full stop. The status explains it.
    if status_code >= 400 {
        return V2Verdict::Document;
    }
    match block {
        // `BlockOutcome` carries two different kinds of verdict and v2 has to
        // split them — `BlockOutcome::message` already words them differently
        // for exactly this reason.
        //
        // `structural_failure` / `parked_domain` mean "we got a page and there
        // is nothing usable in it". Not a wall. That is precisely Firecrawl's
        // `EngineUnsuccessfulError`, which its waterfall turns into
        // `NoEnginesLeftError` → `SCRAPE_ALL_ENGINES_FAILED`.
        Some(b) if b.vendor == STRUCTURAL_FAILURE_VENDOR || b.vendor == PARKED_DOMAIN_VENDOR => {
            V2Verdict::NothingUsable
        }
        // A real vendor verdict on a 2xx. Deliberately NOT relaxed to match
        // Firecrawl: a challenge shell served with HTTP 200 (Cloudflare "Just a
        // moment...") has text, so `isLongEnough` is true and Firecrawl returns
        // the interstitial AS the page's content. We have already paid for that
        // behaviour — see `is_cdn_origin_error` in crw-crawl, where a dead
        // origin behind Cloudflare shipped as `success:true` with the CDN's
        // error text as its markdown, billed, for one customer on the same
        // source for months. Copying it back would re-open a closed,
        // customer-visible bug. This is the one place the two surfaces are
        // allowed to disagree, and `conformance/mock_parity.py` records the
        // disagreement rather than hiding it.
        Some(_) => V2Verdict::Blocked,
        None if has_no_content => V2Verdict::NothingUsable,
        None => V2Verdict::Document,
    }
}

/// Resolved proxy tier reported in `metadata.proxyUsed`.
pub(crate) fn proxy_tier(proxy: &str) -> &'static str {
    if proxy.eq_ignore_ascii_case("stealth") {
        "stealth"
    } else {
        "basic"
    }
}

/// Convert a v2 scrape request into the internal `ScrapeRequest` + the
/// decomposed-format side-data + the resolved proxy tier.
/// Accept anything for `maxAge` and fall back to "unset" rather than 400.
///
/// These two fields were previously unknown-and-ignored, so an SDK sending a
/// string or a float got a working request. Typing them must not turn that into
/// a rejection: this surface exists to swallow whatever the SDK sends.
fn lenient_u64<'de, D>(d: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Number(n) => {
            n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64))
        }
        serde_json::Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    })
}

/// Same leniency for `storeInCache`.
fn lenient_bool<'de, D>(d: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Bool(b) => Some(b),
        serde_json::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    })
}

pub(crate) fn to_internal(
    v2: V2ScrapeRequest,
) -> Result<(ScrapeRequest, formats::DecomposedFormats, String), CrwError> {
    let decomposed = decompose(&v2.formats).map_err(CrwError::InvalidRequest)?;
    let tier = proxy_tier(&v2.proxy).to_string();
    // "stealth" → residential chrome tier; otherwise let the renderer chain
    // decide ("auto"). An explicit `renderer` pin always wins.
    let renderer = v2.renderer.or(if v2.proxy.eq_ignore_ascii_case("stealth") {
        Some(RequestedRenderer::ChromeProxy)
    } else {
        None
    });
    let country = v2
        .location
        .as_ref()
        .and_then(|l| l.country.as_ref())
        .map(|c| c.to_lowercase());

    // Firecrawl's `location.languages` becomes an `Accept-Language` header — the
    // engine has no separate locale knob, and this is what the docs promise. A
    // caller's own explicit `Accept-Language` always wins, so this only fills a
    // gap. Takes effect on the HTTP tier and, since #351, on the browser tiers.
    let mut headers = v2.headers;
    if let Some(accept_language) = v2
        .location
        .as_ref()
        .and_then(|l| l.languages.as_deref())
        .and_then(accept_language_from)
        && !headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("accept-language"))
    {
        headers.insert("Accept-Language".to_string(), accept_language);
    }

    let req = ScrapeRequest {
        url: v2.url,
        formats: decomposed.formats.clone(),
        only_main_content: v2.only_main_content,
        include_tags: v2.include_tags,
        exclude_tags: v2.exclude_tags,
        wait_for: v2.wait_for,
        headers,
        json_schema: decomposed.json_schema.clone(),
        // A `{"type":"json","prompt":...}` format object carries the extraction
        // instruction; it reaches the LLM only via `extract.prompt`.
        extract: decomposed
            .json_prompt
            .clone()
            .map(|prompt| crw_core::types::ExtractOptions {
                schema: None,
                prompt: Some(prompt),
            }),
        change_tracking: decomposed.change_tracking.clone(),
        screenshot_full_page: decomposed.screenshot_full_page,
        max_age: v2.max_age,
        store_in_cache: v2.store_in_cache,
        country,
        deadline_ms: v2.timeout,
        llm_api_key: v2.llm_api_key,
        llm_provider: v2.llm_provider,
        llm_model: v2.llm_model,
        base_url: v2.base_url,
        summary_prompt: v2.summary_prompt,
        renderer,
        render_js: v2.render_js,
        parsers: v2.parsers,
        proxy_list: v2.proxy_list,
        proxy_rotation: v2.proxy_rotation,
        ..Default::default()
    };
    Ok((req, decomposed, tier))
}

pub async fn scrape(
    State(state): State<AppState>,
    body: Result<Json<V2ScrapeRequest>, JsonRejection>,
) -> Result<Json<V2ScrapeResponse>, V2Error> {
    let Json(v2) = body.map_err(AppError::from)?;

    let parsed_url = url::Url::parse(&v2.url)
        .map_err(|e| CrwError::InvalidRequest(format!("Invalid URL: {e}")))?;
    crw_core::url_safety::validate_safe_url_resolved(&parsed_url)
        .await
        .map_err(CrwError::InvalidRequest)?;

    let (req, decomposed, tier) = to_internal(v2)?;
    validate_renderer_pin(req.renderer, req.render_js, &state)?;

    let llm_config = state.config.extraction.llm.as_ref();
    if req.formats.contains(&OutputFormat::Summary)
        && llm_config.is_none()
        && req.llm_api_key.is_none()
    {
        return Err(V2Error::from(CrwError::InvalidRequest(
            "summary format requires LLM config: set CRW_EXTRACTION__LLM__API_KEY \
             in server config or pass llm_api_key in the request body"
                .into(),
        )));
    }

    let user_agent = &state.config.crawler.user_agent;
    let default_stealth =
        state.config.crawler.stealth.enabled && state.config.crawler.stealth.inject_headers;
    let deadline = Deadline::from_request_ms(
        state
            .config
            .effective_deadline_ms(req.deadline_ms, req.wait_for),
    );

    let data = scrape_url(
        &req,
        &state.renderer,
        llm_config,
        &state.config.extraction,
        user_agent,
        default_stealth,
        state.config.renderer.render_js_default,
        deadline,
    )
    .await?;

    let warning = formats::unsupported_warning(&decomposed.unsupported);

    // The whole rule, and where it comes from, is on `v2_verdict`.
    let verdict = v2_verdict(
        data.metadata.status_code,
        data.block.as_ref(),
        data.has_no_content(&req.formats),
    );
    let mut data = data;
    let (success, error) = match verdict {
        // An error RESPONSE, not a 200 carrying `success:false` — captured from
        // the live API against `mock.fastcrw.com/html/empty`. `RendererError`
        // is the arm that carries `SCRAPE_ALL_ENGINES_FAILED` and a 500 (see
        // `super::error`). Billing is unaffected: the SaaS refunds an engine 5xx
        // and an envelope `success:false` alike.
        V2Verdict::NothingUsable => {
            return Err(V2Error(CrwError::RendererError(
                data.block
                    .as_ref()
                    .map(|b| b.reason.clone())
                    .or_else(|| data.warning.clone())
                    .or_else(|| data.warnings.first().cloned())
                    .unwrap_or_else(|| "no engine returned usable content for this URL".into()),
            )));
        }
        // Drop the challenge shell, so the caller gets a clean block rather than
        // the interstitial text as the page's content.
        V2Verdict::Blocked => {
            let msg = data.block.as_ref().map(|b| b.message());
            data.clear_body();
            (false, msg)
        }
        V2Verdict::Document => (true, None),
    };
    let doc = to_v2_document(data, &tier, Uuid::new_v4().to_string());
    Ok(Json(V2ScrapeResponse {
        success,
        data: doc,
        warning,
        error,
    }))
}

/// `GET /v2/scrape/{job_id}` (Tier-3). crw scrape is synchronous, so a scrape
/// "job" never exists to poll — the SDK only hits this when it used an async
/// scrape path we don't expose. Return a clear 404 so the SDK surfaces a
/// meaningful error rather than hanging.
pub async fn get_scrape_job(Path(job_id): Path<String>) -> Result<Json<V2ScrapeResponse>, V2Error> {
    Err(V2Error::from(CrwError::NotFound(format!(
        "scrape job {job_id} not found — this engine performs scrapes synchronously; \
         use POST /v2/scrape and read the response directly"
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn internal(body: serde_json::Value) -> ScrapeRequest {
        let v2: V2ScrapeRequest = serde_json::from_value(body).unwrap();
        to_internal(v2).unwrap().0
    }

    #[test]
    fn accept_language_from_joins_and_trims() {
        assert_eq!(
            accept_language_from(&["en-US".into(), " de ".into()]),
            Some("en-US, de".to_string())
        );
        assert_eq!(accept_language_from(&[]), None);
        assert_eq!(accept_language_from(&["  ".into()]), None);
    }

    #[test]
    fn location_languages_becomes_accept_language() {
        let req = internal(serde_json::json!({
            "url": "http://example.com",
            "location": { "languages": ["de-DE", "en"] },
        }));
        assert_eq!(
            req.headers.get("Accept-Language").map(String::as_str),
            Some("de-DE, en")
        );
    }

    #[test]
    fn explicit_accept_language_header_wins() {
        // A caller's own Accept-Language must not be overwritten by languages.
        let req = internal(serde_json::json!({
            "url": "http://example.com",
            "location": { "languages": ["de-DE"] },
            "headers": { "Accept-Language": "fr-FR" },
        }));
        assert_eq!(
            req.headers.get("Accept-Language").map(String::as_str),
            Some("fr-FR")
        );
    }

    #[test]
    fn no_languages_adds_no_header() {
        let req = internal(serde_json::json!({
            "url": "http://example.com",
            "location": { "country": "de" },
        }));
        assert!(
            !req.headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("accept-language"))
        );
    }

    #[test]
    fn v2_scrape_accepts_snake_case_proxy_alias_and_threads_to_internal() {
        // The managed layer injects snake_case proxy_list/proxy_rotation; the v2
        // wire is camelCase, so the alias must accept both and to_internal must
        // thread them into the engine ScrapeRequest (not drop via Default).
        let body = serde_json::json!({
            "url": "http://example.com",
            "proxy_list": ["http://u:p@1.2.3.4:8080"],
            "proxy_rotation": "round_robin",
        });
        let v2: V2ScrapeRequest = serde_json::from_value(body).unwrap();
        assert_eq!(v2.proxy_list, vec!["http://u:p@1.2.3.4:8080"]);
        assert_eq!(
            v2.proxy_rotation,
            Some(crw_core::proxy::ProxyRotation::RoundRobin)
        );
        let (req, _, _) = to_internal(v2).unwrap();
        assert_eq!(req.proxy_list, vec!["http://u:p@1.2.3.4:8080"]);
        assert_eq!(
            req.proxy_rotation,
            Some(crw_core::proxy::ProxyRotation::RoundRobin)
        );
    }

    /// Regression for #346: `renderJs` used to have no field on the v2 wire, so
    /// the lenient-unknown-fields policy swallowed it and `to_internal` left
    /// `render_js: None` (auto). With a browser tier configured, auto escalates,
    /// which made `renderJs:false` indistinguishable from `renderJs:true`.
    /// Asserted through `to_internal` — deserialization alone would still pass
    /// if the forwarding line were dropped.
    #[test]
    fn v2_scrape_threads_render_js_to_internal() {
        for (wire, expected) in [
            (serde_json::json!(false), Some(false)),
            (serde_json::json!(true), Some(true)),
        ] {
            let body = serde_json::json!({ "url": "http://example.com", "renderJs": wire });
            let v2: V2ScrapeRequest = serde_json::from_value(body).unwrap();
            let (req, _, _) = to_internal(v2).unwrap();
            assert_eq!(req.render_js, expected, "renderJs {wire} must survive");
        }
    }

    #[test]
    fn v2_scrape_render_js_snake_case_alias() {
        let body = serde_json::json!({ "url": "http://example.com", "render_js": false });
        let v2: V2ScrapeRequest = serde_json::from_value(body).unwrap();
        let (req, _, _) = to_internal(v2).unwrap();
        assert_eq!(req.render_js, Some(false));
    }

    #[test]
    fn v2_scrape_omitted_render_js_stays_none() {
        // Absent must remain None so the server's render_js_default still
        // applies — omitting the field is not the same as sending false.
        let body = serde_json::json!({ "url": "http://example.com" });
        let v2: V2ScrapeRequest = serde_json::from_value(body).unwrap();
        let (req, _, _) = to_internal(v2).unwrap();
        assert_eq!(req.render_js, None);
    }

    fn minimal_doc() -> V2Document {
        use crw_core::types::{PageMetadata, ScrapeData};
        let data = ScrapeData {
            markdown: Some("# hi".into()),
            source_hash: None,
            html: None,
            raw_html: None,
            plain_text: None,
            links: None,
            images: None,
            json: None,
            summary: None,
            llm_usage: None,
            chunks: None,
            warning: None,
            warnings: vec![],
            render_decision: None,
            credit_cost: 1,
            basis: None,
            basis_warnings: Vec::new(),
            llm_input_hash: None,
            metadata: PageMetadata {
                title: None,
                description: None,
                og_title: None,
                og_description: None,
                og_image: None,
                canonical_url: None,
                source_url: "https://example.com".into(),
                final_url: None,
                language: None,
                status_code: 200,
                rendered_with: None,
                elapsed_ms: 0,
                page_count: None,
                source_filename: None,
                extra: Default::default(),
            },
            debug_extraction: None,
            content_type: Some("text/html".into()),
            change_tracking: None,
            screenshot: None,
            block: None,
            truncated: false,
            cached: false,
        };
        to_v2_document(data, "basic", "id".into())
    }

    #[test]
    fn v2_response_blocked_serializes_success_false_with_error() {
        let resp = V2ScrapeResponse {
            success: false,
            data: minimal_doc(),
            warning: None,
            error: Some("Blocked by anti-bot (cloudflare): CF challenge".into()),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["success"], false);
        assert_eq!(v["error"], "Blocked by anti-bot (cloudflare): CF challenge");
    }

    #[test]
    fn v2_response_clean_omits_error_key() {
        let resp = V2ScrapeResponse {
            success: true,
            data: minimal_doc(),
            warning: None,
            error: None,
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["success"], true);
        assert!(v.get("error").is_none(), "error omitted on success");
    }

    #[test]
    fn v2_scrape_camelcase_proxy_list_also_works_and_mode_is_separate() {
        let body = serde_json::json!({
            "url": "http://example.com",
            "proxyList": ["http://1.2.3.4:8080"],
            "proxy": "stealth",
        });
        let v2: V2ScrapeRequest = serde_json::from_value(body).unwrap();
        assert_eq!(v2.proxy_list, vec!["http://1.2.3.4:8080"]);
        // `proxy` MODE is independent of the proxy_list BYOP URLs.
        assert_eq!(v2.proxy, "stealth");
        let (req, _, _) = to_internal(v2).unwrap();
        assert_eq!(req.proxy_list.len(), 1);
    }
}

#[cfg(test)]
mod verdict_tests {
    use super::*;
    use crw_core::types::BlockOutcome;

    fn block(vendor: &str) -> BlockOutcome {
        BlockOutcome {
            vendor: vendor.to_string(),
            reason: "reason".to_string(),
        }
    }

    /// Captured from api.firecrawl.dev against mock.fastcrw.com/status/<code>:
    /// every one answers `HTTP 200 {"success":true,...,"statusCode":<code>}`.
    /// Before this, a 4xx/5xx under 200 bytes was `success:false`.
    #[test]
    fn origin_error_statuses_are_documents_not_failures() {
        for code in [400, 401, 403, 404, 410, 429, 500, 502, 503] {
            assert_eq!(
                v2_verdict(code, None, false),
                V2Verdict::Document,
                "HTTP {code} must be returned as a document"
            );
        }
    }

    /// The precedence check. `isLongEnough || !isGoodStatusCode` are
    /// alternatives, so an empty body only fails on a GOOD status.
    #[test]
    fn empty_body_fails_on_200_but_not_on_404() {
        assert_eq!(v2_verdict(200, None, true), V2Verdict::NothingUsable);
        assert_eq!(v2_verdict(404, None, true), V2Verdict::Document);
    }

    /// The old gate fired only under ERROR_PAGE_MAX_TEXT (200 bytes), so the
    /// same site returned success:false for a terse 404 and success:true for a
    /// chatty one. Nothing reaches this function that could reintroduce that:
    /// there is no length input at all.
    #[test]
    fn verdict_does_not_depend_on_body_size() {
        assert_eq!(v2_verdict(404, None, true), v2_verdict(404, None, false));
    }

    /// `structural_failure` is "we got a page with nothing in it", which is
    /// Firecrawl's EngineUnsuccessfulError, not a wall. Captured:
    /// mock.fastcrw.com/html/empty answers 500 SCRAPE_ALL_ENGINES_FAILED.
    #[test]
    fn thin_and_parked_pages_are_engine_failures_not_walls() {
        assert_eq!(
            v2_verdict(200, Some(&block(STRUCTURAL_FAILURE_VENDOR)), false),
            V2Verdict::NothingUsable
        );
        assert_eq!(
            v2_verdict(200, Some(&block(PARKED_DOMAIN_VENDOR)), false),
            V2Verdict::NothingUsable
        );
    }

    /// The one deliberate deviation from Firecrawl: a vendor wall served with
    /// HTTP 200 still fails. Firecrawl would return the challenge shell as the
    /// page's content — see `v2_verdict`'s doc comment for what that cost.
    #[test]
    fn vendor_wall_on_a_200_still_fails() {
        assert_eq!(
            v2_verdict(200, Some(&block("cloudflare")), false),
            V2Verdict::Blocked
        );
    }

    /// ...but the same wall on a 4xx does not: the status already explains the
    /// page, and that is the case this whole change is about.
    #[test]
    fn vendor_wall_on_a_403_is_a_document() {
        assert_eq!(
            v2_verdict(403, Some(&block("cloudflare")), false),
            V2Verdict::Document
        );
    }
}
