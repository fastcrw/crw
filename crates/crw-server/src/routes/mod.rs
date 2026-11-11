pub mod batch;
pub mod breakers;
pub mod capabilities;
pub mod change_tracking;
pub mod crawl;
pub mod enrich;
pub mod extract;
pub mod health;
pub mod kimi;
pub mod map;
pub mod mcp;
pub mod metrics;
pub mod openapi;
pub mod preferences;
pub mod research;
pub mod scrape;
pub mod search;
pub mod v1;
pub mod v2;

use axum::http::StatusCode;
use axum::response::IntoResponse;

/// Shared 405 fallback used by every method-specific route in both the v1 and
/// v2 routers. Returns the standard `ApiResponse` error envelope so a wrong
/// method (e.g. `GET /v1/scrape`) is reported as JSON, not an empty body.
pub(crate) async fn method_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        axum::Json(crw_core::types::ApiResponse::<()>::err_with_code(
            "Method not allowed",
            "method_not_allowed",
        )),
    )
}
