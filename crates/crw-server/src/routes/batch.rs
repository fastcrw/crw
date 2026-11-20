//! Native `POST /v1/batch/scrape` (+ status / cancel).
//!
//! Scrapes an explicit URL list, reusing the crawl-job machinery over that list
//! (`AppState::start_batch_job`) — no link discovery, no same-origin filtering,
//! no dedup. Input order is recoverable via each document's `metadata.sourceURL`.
//!
//! Shapes follow the native v1 conventions (strict camelCase, the same status
//! envelope as `GET /v1/crawl/{id}`), NOT the Firecrawl-compatible `/v2` surface
//! — `/v2/batch/scrape` keeps `invalidURLs` for SDK parity and must not change.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use futures::StreamExt;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crw_core::error::CrwError;
use crw_core::types::{CrawlState, ScrapeRequest};

use crate::error::AppError;
use crate::state::AppState;

/// Per-route body cap for batch submits: a 10k-URL list at a few hundred bytes
/// per URL overflows the global 1 MB JSON cap; 8 MB keeps 10k long URLs
/// submittable while staying DoS-bounded. Mirrors the `/v2` batch route.
pub const MAX_BATCH_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Per-URL SSRF validation resolves DNS. Bounded concurrency keeps a 10k-URL
/// submit at seconds rather than minutes (cold DNS dominates); order is
/// preserved via `buffered`.
const VALIDATE_CONCURRENCY: usize = 256;

/// `POST /v1/batch/scrape` response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchStartResponse {
    pub success: bool,
    pub id: String,
    /// URLs rejected by parsing or the SSRF guard, returned verbatim. Empty
    /// unless `ignoreInvalidUrls` accepted a partially-valid list.
    pub invalid_urls: Vec<String>,
}

/// `POST /v1/batch/scrape` — start a batch scrape over `urls`.
///
/// Body: `{ "urls": [...], "ignoreInvalidUrls": true, "maxConcurrency": 10, ...scrapeOptions }`
/// where `scrapeOptions` are the same fields `POST /v1/scrape` accepts (minus `url`).
pub async fn start_batch(
    State(state): State<AppState>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<BatchStartResponse>, AppError> {
    let Json(mut raw) = body.map_err(AppError::from)?;
    let obj = raw
        .as_object_mut()
        .ok_or_else(|| CrwError::InvalidRequest("request body must be a JSON object".into()))?;

    let urls_val = obj
        .remove("urls")
        .ok_or_else(|| CrwError::InvalidRequest("`urls` is required".into()))?;
    let urls: Vec<String> = serde_json::from_value(urls_val)
        .map_err(|e| CrwError::InvalidRequest(format!("invalid `urls`: {e}")))?;
    if urls.is_empty() {
        return Err(AppError::from(CrwError::InvalidRequest(
            "`urls` must contain at least one URL".into(),
        )));
    }
    let max_urls = state.config.crawler.max_batch_urls;
    if urls.len() > max_urls {
        return Err(AppError::from(CrwError::InvalidRequest(format!(
            "`urls` exceeds the maximum of {max_urls} URLs per batch (got {})",
            urls.len()
        ))));
    }

    let ignore_invalid = obj
        .remove("ignoreInvalidUrls")
        .as_ref()
        .and_then(Value::as_bool)
        .unwrap_or(true);

    // Optional per-job OUTER pipeline width (the SaaS injects this per plan
    // tier). Removed symmetric with `urls` so it can't leak into the per-URL
    // scrape template. `start_batch_job` clamps it to a safe range — a
    // client-supplied value is never authoritative.
    let max_concurrency_override = obj
        .remove("maxConcurrency")
        .as_ref()
        .and_then(Value::as_u64)
        .map(|n| n as usize);

    // Build the per-page scrape template from the remaining (native scrape)
    // options. A placeholder URL satisfies the required field; it's cleared
    // afterward, and `start_batch_job` stamps each URL onto a clone.
    obj.insert(
        "url".to_string(),
        Value::String("https://placeholder.invalid/".into()),
    );
    let mut template: ScrapeRequest = serde_json::from_value(Value::Object(obj.clone()))
        .map_err(|e| CrwError::InvalidRequest(format!("invalid batch scrape options: {e}")))?;
    // Reject an unavailable pinned renderer up front (as /v1/scrape and /v1/crawl
    // do) instead of failing every URL individually deep in the pipeline.
    crate::state::validate_renderer_pin(template.renderer, template.render_js, &state)?;
    // Same rejections `/v1/scrape` applies at the top of the scrape. Left to
    // the per-URL path they would come back as N placeholder documents each
    // labelled as an anti-bot block, for a fault in the caller's own request.
    crw_crawl::single::validate_scrape_template(&template)?;
    template.url = String::new();

    // Partition URLs into valid / invalid (SSRF-checked, same guard as
    // `/v1/scrape`), with bounded concurrency and preserved input order.
    let checks = futures::stream::iter(urls.into_iter().map(|u| async move {
        let ok = match url::Url::parse(&u) {
            Ok(parsed) => crw_core::url_safety::validate_safe_url_resolved(&parsed)
                .await
                .is_ok(),
            Err(_) => false,
        };
        (u, ok)
    }))
    .buffered(VALIDATE_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    for (u, ok) in checks {
        if ok { valid.push(u) } else { invalid.push(u) }
    }
    if valid.is_empty() {
        return Err(AppError::from(CrwError::InvalidRequest(
            "no valid URLs to scrape".into(),
        )));
    }
    if !invalid.is_empty() && !ignore_invalid {
        return Err(AppError::from(CrwError::InvalidRequest(format!(
            "invalid URLs: {}",
            invalid.join(", ")
        ))));
    }

    let id = state
        .start_batch_job(valid, template, max_concurrency_override)
        .await;

    Ok(Json(BatchStartResponse {
        success: true,
        id: id.to_string(),
        invalid_urls: invalid,
    }))
}

/// `GET /v1/batch/scrape/{id}` — batch status. Same envelope as
/// `GET /v1/crawl/{id}`: `{ success, status, total, completed, data, error? }`.
pub async fn get_batch(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<CrawlState>, AppError> {
    let jobs = state.crawl_jobs.read().await;
    let job = jobs
        .get(&id)
        .ok_or_else(|| CrwError::NotFound(format!("Batch job {id} not found")))?;
    Ok(Json(job.rx.borrow().clone()))
}

/// `DELETE /v1/batch/scrape/{id}` — cancel an active batch job. Flips the job to
/// the terminal `cancelled` state so status polls stop reporting `scraping`.
///
/// Batch jobs live in the same `crawl_jobs` map as crawls, so this is exactly
/// `cancel_crawl`. Delegate rather than copy it: the terminal-state guard has to
/// stay in lockstep with every other `Completed | Failed | Cancelled` check, and
/// a duplicate would drift the first time a state is added. `/v2/batch/scrape`
/// delegates the same way.
pub async fn cancel_batch(state: State<AppState>, id: Path<Uuid>) -> Result<Json<Value>, AppError> {
    super::crawl::cancel_crawl(state, id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, State};
    use crw_core::config::AppConfig;
    use crw_core::types::CrawlStatus;
    use serde_json::json;

    #[test]
    fn batch_start_response_is_camel_case() {
        // The v1 surface is strict camelCase: this must serialize `invalidUrls`
        // (not the v2/Firecrawl `invalidURLs`, nor a snake_case `invalid_urls`).
        let v = serde_json::to_value(BatchStartResponse {
            success: true,
            id: "job-1".into(),
            invalid_urls: vec!["bad".into()],
        })
        .unwrap();
        assert!(
            v.get("invalidUrls").is_some(),
            "expected camelCase invalidUrls"
        );
        assert!(v.get("invalid_urls").is_none(), "snake_case key leaked");
        assert!(
            v.get("invalidURLs").is_none(),
            "v2 capital-URL key leaked into v1"
        );
    }

    fn default_state() -> AppState {
        let config: AppConfig = toml::from_str("").unwrap();
        AppState::new(config).unwrap()
    }

    async fn call(state: &AppState, body: Value) -> Result<BatchStartResponse, AppError> {
        start_batch(State(state.clone()), Ok(Json(body)))
            .await
            .map(|Json(r)| r)
    }

    fn invalid_request_message(err: &AppError) -> String {
        match &err.0 {
            CrwError::InvalidRequest(msg) => msg.clone(),
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn start_batch_rejects_invalid_actions_before_any_url_work() {
        let state = default_state();
        let err = call(
            &state,
            json!({ "urls": ["https://example.com/"], "actions": [{ "type": "hover" }] }),
        )
        .await
        .unwrap_err();
        assert!(
            invalid_request_message(&err).contains("actions[0]"),
            "got: {}",
            invalid_request_message(&err)
        );
    }

    #[tokio::test]
    async fn start_batch_rejects_screenshot_without_js_before_any_url_work() {
        let state = default_state();
        let err = call(
            &state,
            json!({ "urls": ["https://example.com/"], "formats": ["screenshot"], "renderJs": false }),
        )
        .await
        .unwrap_err();
        assert!(
            invalid_request_message(&err).contains("screenshot"),
            "got: {}",
            invalid_request_message(&err)
        );
    }

    #[tokio::test]
    async fn start_batch_requires_urls_field() {
        let state = default_state();
        let err = call(&state, json!({})).await.unwrap_err();
        assert_eq!(invalid_request_message(&err), "`urls` is required");
    }

    #[tokio::test]
    async fn start_batch_rejects_a_non_object_body() {
        let state = default_state();
        let err = call(&state, json!([1, 2, 3])).await.unwrap_err();
        assert_eq!(
            invalid_request_message(&err),
            "request body must be a JSON object"
        );
    }

    #[tokio::test]
    async fn start_batch_rejects_an_empty_urls_list() {
        let state = default_state();
        let err = call(&state, json!({"urls": []})).await.unwrap_err();
        assert_eq!(
            invalid_request_message(&err),
            "`urls` must contain at least one URL"
        );
    }

    #[tokio::test]
    async fn start_batch_rejects_urls_of_the_wrong_type() {
        let state = default_state();
        let err = call(&state, json!({"urls": "https://example.com"}))
            .await
            .unwrap_err();
        assert!(invalid_request_message(&err).starts_with("invalid `urls`"));
    }

    #[tokio::test]
    async fn start_batch_rejects_a_list_over_the_max_batch_urls_cap_before_any_url_work() {
        let config: AppConfig = toml::from_str("[crawler]\nmax_batch_urls = 2\n").unwrap();
        let state = AppState::new(config).unwrap();
        let urls: Vec<String> = (0..3).map(|i| format!("https://example.com/{i}")).collect();
        let err = call(&state, json!({"urls": urls})).await.unwrap_err();
        let msg = invalid_request_message(&err);
        assert!(
            msg.contains("exceeds the maximum of 2 URLs per batch (got 3)"),
            "message was: {msg}"
        );
    }

    #[tokio::test]
    async fn start_batch_accepts_a_list_exactly_at_the_max_batch_urls_cap() {
        // At the cap, the request must pass the size guard and move on to
        // per-URL validation (all invalid here, so it fails downstream —
        // proving the cap check itself did not reject it).
        let config: AppConfig = toml::from_str("[crawler]\nmax_batch_urls = 2\n").unwrap();
        let state = AppState::new(config).unwrap();
        let err = call(&state, json!({"urls": ["not-a-url", "also-not-a-url"]}))
            .await
            .unwrap_err();
        assert_eq!(invalid_request_message(&err), "no valid URLs to scrape");
    }

    #[tokio::test]
    async fn start_batch_rejects_an_unavailable_pinned_renderer_before_url_validation() {
        // Default config builds no CDP tier, so any pinned renderer is
        // unavailable. This must be caught up front, before the (also
        // invalid) URLs are even checked.
        let state = default_state();
        let err = call(&state, json!({"urls": ["not-a-url"], "renderer": "chrome"}))
            .await
            .unwrap_err();
        let msg = invalid_request_message(&err);
        assert!(
            msg.contains("renderer 'chrome' not available"),
            "message was: {msg}"
        );
    }

    #[tokio::test]
    async fn start_batch_rejects_invalid_scrape_options() {
        let state = default_state();
        let err = call(
            &state,
            json!({"urls": ["https://example.com"], "formats": "not-an-array"}),
        )
        .await
        .unwrap_err();
        assert!(invalid_request_message(&err).starts_with("invalid batch scrape options"),);
    }

    #[tokio::test]
    async fn start_batch_rejects_when_every_url_is_invalid() {
        let state = default_state();
        // "not-a-url" fails to parse; the loopback address fails the SSRF
        // host check. Neither needs DNS, so this stays hermetic.
        let err = call(&state, json!({"urls": ["not-a-url", "http://127.0.0.1/"]}))
            .await
            .unwrap_err();
        assert_eq!(invalid_request_message(&err), "no valid URLs to scrape");
    }

    #[tokio::test]
    async fn get_batch_errors_not_found_for_a_missing_id() {
        let state = default_state();
        let err = get_batch(State(state), Path(Uuid::new_v4()))
            .await
            .unwrap_err();
        assert!(matches!(err.0, CrwError::NotFound(_)));
    }

    #[tokio::test]
    async fn get_batch_returns_the_jobs_current_snapshot() {
        let state = default_state();
        let id = Uuid::new_v4();
        let (tx, rx) = tokio::sync::watch::channel(CrawlState {
            id,
            success: true,
            status: CrawlStatus::InProgress,
            total: 3,
            completed: 1,
            blocked: 0,
            data: vec![],
            error: None,
        });
        state.crawl_jobs.write().await.insert(
            id,
            crate::state::CrawlJob {
                rx,
                tx,
                created_at: std::time::Instant::now(),
                abort_handle: None,
            },
        );

        let Json(snapshot) = get_batch(State(state), Path(id))
            .await
            .unwrap_or_else(|_| panic!("get_batch failed"));
        assert_eq!(snapshot.status, CrawlStatus::InProgress);
        assert_eq!(snapshot.total, 3);
        assert_eq!(snapshot.completed, 1);
    }

    #[tokio::test]
    async fn cancel_batch_errors_not_found_for_a_missing_id() {
        let state = default_state();
        let err = cancel_batch(State(state), Path(Uuid::new_v4()))
            .await
            .unwrap_err();
        assert!(matches!(err.0, CrwError::NotFound(_)));
    }
}
