use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};

use crate::routes::{self, method_not_allowed};
use crate::state::AppState;

/// All `/v1/*` routes. Lifted verbatim out of `app.rs` so the v1 surface stays
/// byte-identical to its pre-#62 wiring — only the location changed. Returns a
/// `Router<AppState>` (state not yet applied) so `app.rs` can `.merge()` it with
/// the v2 router and apply auth + rate-limit + state once to the whole.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/scrape",
            post(routes::scrape::scrape).fallback(method_not_allowed),
        )
        .route(
            "/v1/crawl",
            post(routes::crawl::start_crawl).fallback(method_not_allowed),
        )
        .route(
            // Batch submits carry up to `max_batch_urls` URLs — needs more than
            // the global 1 MB JSON cap (innermost DefaultBodyLimit wins).
            "/v1/batch/scrape",
            post(routes::batch::start_batch)
                .layer(DefaultBodyLimit::max(routes::batch::MAX_BATCH_BODY_BYTES))
                .fallback(method_not_allowed),
        )
        .route(
            "/v1/batch/scrape/{id}",
            get(routes::batch::get_batch)
                .delete(routes::batch::cancel_batch)
                .fallback(method_not_allowed),
        )
        .route(
            "/v1/crawl/{id}",
            get(routes::crawl::get_crawl)
                .delete(routes::crawl::cancel_crawl)
                .fallback(method_not_allowed),
        )
        .route(
            "/v1/extract",
            post(routes::extract::start_extract).fallback(method_not_allowed),
        )
        .route(
            "/v1/extract/{id}",
            get(routes::extract::get_extract)
                .delete(routes::extract::cancel_extract)
                .fallback(method_not_allowed),
        )
        .route(
            "/v1/enrich",
            post(routes::enrich::enrich).fallback(method_not_allowed),
        )
        .route(
            "/v1/map",
            post(routes::map::map).fallback(method_not_allowed),
        )
        .route(
            "/v1/search",
            post(routes::search::search).fallback(method_not_allowed),
        )
        .route(
            "/v1/capabilities",
            get(routes::capabilities::capabilities).fallback(method_not_allowed),
        )
        .route(
            "/v1/change-tracking/diff",
            post(routes::change_tracking::diff).fallback(method_not_allowed),
        )
        .route(
            "/v1/search/research/papers",
            get(routes::research::search_papers).fallback(method_not_allowed),
        )
        .route(
            "/v1/search/research/papers/{id}",
            get(routes::research::get_paper).fallback(method_not_allowed),
        )
        .route(
            "/v1/search/research/papers/{id}/similar",
            get(routes::research::similar).fallback(method_not_allowed),
        )
        .route(
            "/v1/search/research/github",
            get(routes::research::github).fallback(method_not_allowed),
        )
}
