use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use crw_core::error::CrwError;
use crw_core::types::{CrawlRequest, CrawlStartResponse, CrawlState, CrawlStatus};
use uuid::Uuid;

use crate::error::AppError;
use crate::state::{AppState, validate_crawl_renderer};

/// Lift a nested `scrapeOptions` object onto the top level.
///
/// `CrawlRequest` is flat, but Firecrawl — and our own published OpenAPI spec,
/// and the API-reference page generated from it — nest the per-page settings
/// under `scrapeOptions`. A body written against that shape had every key
/// silently dropped: `scrapeOptions.formats` never reached the engine and the
/// crawl quietly returned markdown.
///
/// An explicit top-level key always wins, so every request that works today
/// keeps working; this only fills in what the caller expressed the other way.
pub(crate) fn lift_scrape_options(mut body: serde_json::Value) -> serde_json::Value {
    let Some(obj) = body.as_object_mut() else {
        return body;
    };
    let nested = obj
        .remove("scrapeOptions")
        .or_else(|| obj.remove("scrape_options"));
    if let Some(serde_json::Value::Object(nested)) = nested {
        for (k, v) in nested {
            obj.entry(k).or_insert(v);
        }
    }
    body
}

/// POST /v1/crawl — start a crawl job.
/// Response format matches Firecrawl: { success: true, id: "..." }
pub async fn start_crawl(
    State(state): State<AppState>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<CrawlStartResponse>, AppError> {
    let Json(raw) = body.map_err(AppError::from)?;
    let req: CrawlRequest = serde_json::from_value(lift_scrape_options(raw))
        .map_err(|e| CrwError::InvalidRequest(format!("Invalid crawl request: {e}")))?;
    let parsed_url = url::Url::parse(&req.url)
        .map_err(|e| CrwError::InvalidRequest(format!("Invalid URL: {e}")))?;
    crw_core::url_safety::validate_safe_url_resolved(&parsed_url)
        .await
        .map_err(CrwError::InvalidRequest)?;

    validate_crawl_renderer(&req, &state)?;

    let id = state.start_crawl_job(req).await;

    Ok(Json(CrawlStartResponse {
        success: true,
        id: id.to_string(),
    }))
}

/// DELETE /v1/crawl/:id — cancel an active crawl job.
pub async fn cancel_crawl(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut jobs = state.crawl_jobs.write().await;
    let job = jobs
        .get_mut(&id)
        .ok_or_else(|| CrwError::NotFound(format!("Crawl job {id} not found")))?;

    let status = job.rx.borrow().status;
    if matches!(
        status,
        CrawlStatus::Completed | CrawlStatus::Failed | CrawlStatus::Cancelled
    ) {
        return Err(AppError(CrwError::InvalidRequest(
            "Crawl job already finished".into(),
        )));
    }

    // Abort the spawned task, then mark the job terminal so status polls
    // return "cancelled" instead of "scraping" forever (and TTL cleanup
    // can evict it).
    if let Some(handle) = job.abort_handle.take() {
        handle.abort();
    }
    job.tx.send_modify(|st| st.status = CrawlStatus::Cancelled);

    Ok(Json(serde_json::json!({
        "success": true,
        "message": format!("Crawl job {id} cancelled")
    })))
}

/// GET /v1/crawl/:id — get crawl status.
/// Response format matches Firecrawl: { status, total, completed, data, ... }
pub async fn get_crawl(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<CrawlState>, AppError> {
    let jobs = state.crawl_jobs.read().await;
    let job = jobs
        .get(&id)
        .ok_or_else(|| CrwError::NotFound(format!("Crawl job {id} not found")))?;

    let current = job.rx.borrow().clone();
    Ok(Json(current))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crw_core::types::OutputFormat;
    use serde_json::json;

    fn parse(body: serde_json::Value) -> CrawlRequest {
        serde_json::from_value(lift_scrape_options(body)).unwrap()
    }

    /// The shape our own OpenAPI spec publishes. Before this, `formats` here
    /// was an unknown key on a flat struct and the crawl silently produced
    /// markdown — verified against production.
    #[test]
    fn nested_scrape_options_reach_the_request() {
        let req = parse(json!({
            "url": "https://example.com",
            "scrapeOptions": {
                "formats": ["html"],
                "onlyMainContent": false,
                "renderJs": false,
                "waitFor": 1500,
            },
        }));
        assert_eq!(req.formats, vec![OutputFormat::Html]);
        assert_eq!(req.only_main_content, Some(false));
        assert_eq!(req.render_js, Some(false));
        assert_eq!(req.wait_for, Some(1500));
    }

    #[test]
    fn a_flat_body_is_unchanged() {
        let req = parse(json!({
            "url": "https://example.com",
            "formats": ["links"],
            "renderJs": true,
        }));
        assert_eq!(req.formats, vec![OutputFormat::Links]);
        assert_eq!(req.render_js, Some(true));
    }

    /// Both shapes at once is a contradiction only the caller can resolve, so
    /// the explicit top-level value wins and nothing that works today changes.
    #[test]
    fn top_level_wins_over_nested() {
        let req = parse(json!({
            "url": "https://example.com",
            "formats": ["links"],
            "scrapeOptions": { "formats": ["html"] },
        }));
        assert_eq!(req.formats, vec![OutputFormat::Links]);
    }

    #[test]
    fn snake_case_alias_and_absent_options_are_handled() {
        let req = parse(json!({
            "url": "https://example.com",
            "scrape_options": { "formats": ["rawHtml"] },
        }));
        assert_eq!(req.formats, vec![OutputFormat::RawHtml]);

        // No scrapeOptions at all keeps the documented defaults.
        let req = parse(json!({ "url": "https://example.com" }));
        assert_eq!(req.formats, vec![OutputFormat::Markdown]);
        assert_eq!(req.only_main_content, None);
    }

    /// `headers` arrives either flat or nested, like every other per-page key.
    #[test]
    fn headers_lift_out_of_scrape_options() {
        let req = parse(json!({
            "url": "https://example.com",
            "scrapeOptions": { "headers": { "X-Env": "staging" } },
        }));
        assert_eq!(req.headers.get("X-Env"), Some(&"staging".to_string()));

        let req = parse(json!({
            "url": "https://example.com",
            "headers": { "X-Env": "prod" },
        }));
        assert_eq!(req.headers.get("X-Env"), Some(&"prod".to_string()));

        // Nothing supplied stays an empty map, so pre-existing bodies are
        // byte-for-byte unaffected.
        assert!(
            parse(json!({ "url": "https://example.com" }))
                .headers
                .is_empty()
        );
    }

    #[test]
    fn a_non_object_scrape_options_is_ignored_not_fatal() {
        // A caller sending `scrapeOptions: null` (some SDKs do) must not 400.
        let req = parse(json!({ "url": "https://example.com", "scrapeOptions": null }));
        assert_eq!(req.formats, vec![OutputFormat::Markdown]);
    }
}
