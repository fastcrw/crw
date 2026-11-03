use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use crw_core::error::CrwError;
use crw_core::types::{MapData, MapRequest, MapResponse};
use crw_crawl::crawl::{DiscoverOptions, discover_urls};
use crw_crawl::url_filter::{RequestOverrides, UrlFilterCfg};
use std::sync::Arc;
use std::time::Duration;

use crate::error::AppError;
use crate::state::AppState;

/// Maximum number of keys in any single `extra_*_params` / `preserveParams`
/// list on a /map request. Anything over → 422.
const EXTRA_PARAMS_CAP: usize = 64;

fn check_cap(name: &str, list: &Option<Vec<String>>) -> Result<(), CrwError> {
    if let Some(v) = list
        && v.len() > EXTRA_PARAMS_CAP
    {
        return Err(CrwError::InvalidRequest(format!(
            "{name} exceeds {EXTRA_PARAMS_CAP}-key cap"
        )));
    }
    Ok(())
}

/// Resolve effective /map filter cfg for a single request.
///
/// Precedence (outermost wins):
/// 1. `ignoreQueryParameters` is the outermost gate when `Some(_)`.
/// 2. Per-request granular flag, when `Some(_)`, overrides TOML.
/// 3. TOML value (built into `state.url_filter`) is the base.
/// 4. When `state.url_filter` is `None`, the whole filter is off — the
///    request can still flip it on by sending `ignoreQueryParameters: true`
///    or granular flags, in which case the compile-time defaults fire.
fn resolve_filter_cfg(state: &AppState, req: &MapRequest) -> Option<Arc<UrlFilterCfg>> {
    let touches_filter = req.strip_tracking_params.is_some()
        || req.drop_action_urls.is_some()
        || req.ignore_query_parameters.is_some()
        || req.extra_tracking_params.is_some()
        || req.extra_action_params.is_some()
        || req.preserve_params.is_some();

    let base = match (&state.url_filter, touches_filter) {
        (Some(arc), false) => return Some(arc.clone()),
        (Some(arc), true) => (**arc).clone(),
        (None, false) => return None,
        (None, true) => UrlFilterCfg::defaults_on(),
    };

    let overrides = RequestOverrides {
        strip_tracking: req.strip_tracking_params,
        drop_actions: req.drop_action_urls,
        coarse_strip_all: req.ignore_query_parameters,
        extra_tracking: req.extra_tracking_params.clone(),
        extra_action: req.extra_action_params.clone(),
        preserve: req.preserve_params.clone(),
    };
    Some(Arc::new(base.with_overrides(overrides)))
}

/// POST /v1/map — discover URLs.
/// Response format matches Firecrawl: { success: true, links: [...] }
pub async fn map(
    State(state): State<AppState>,
    body: Result<Json<MapRequest>, JsonRejection>,
) -> Result<Json<MapResponse>, AppError> {
    let Json(req) = body.map_err(AppError::from)?;
    let parsed_url = url::Url::parse(&req.url)
        .map_err(|e| CrwError::InvalidRequest(format!("Invalid URL: {e}")))?;
    crw_core::url_safety::validate_safe_url_resolved(&parsed_url)
        .await
        .map_err(CrwError::InvalidRequest)?;

    check_cap("extra_tracking_params", &req.extra_tracking_params)?;
    check_cap("extra_action_params", &req.extra_action_params)?;
    check_cap("preserve_params", &req.preserve_params)?;

    let url_filter = resolve_filter_cfg(&state, &req);

    let max_depth = req
        .max_depth
        .unwrap_or(state.config.crawler.default_max_depth);

    let timeout_secs = req.timeout.unwrap_or(120).min(300);
    let discover_future = discover_urls(DiscoverOptions {
        base_url: &req.url,
        max_depth,
        use_sitemap: req.use_sitemap,
        renderer: &state.renderer,
        max_concurrency: state.config.crawler.max_concurrency,
        requests_per_second: state.config.crawler.requests_per_second,
        user_agent: &state.config.crawler.user_agent,
        proxy: state.config.crawler.proxy.clone(),
        deadline_ms_per_page: state.config.effective_deadline_ms(None, None),
        per_host_max_concurrent: state.config.crawler.per_host_max_concurrent,
        crawl_fallback: req.crawl_fallback,
        url_filter,
        max_urls: req
            .limit
            .unwrap_or(crw_crawl::crawl::DEFAULT_MAX_DISCOVERED_URLS),
        // Discovery stops just before the backstop below and returns what it has.
        // Previously the backstop was the only bound: it dropped the future and
        // every discovered URL with it, so a slow site got a 504 with zero links.
        overall_deadline: crw_crawl::crawl::discovery_deadline(Duration::from_secs(timeout_secs)),
        respect_robots: state.config.crawler.respect_robots_txt,
        filter_by_path: req.filter_by_path,
    });

    let result =
        match tokio::time::timeout(Duration::from_secs(timeout_secs), discover_future).await {
            Ok(r) => r?,
            Err(_) => {
                return Err(AppError(CrwError::Timeout(timeout_secs * 1000)));
            }
        };

    Ok(Json(MapResponse::ok(MapData {
        links: result.urls,
        dropped_action_count: result.dropped_action_count,
        stripped_tracking_count: result.stripped_tracking_count,
        sitemaps: result.sitemaps,
    })))
}
