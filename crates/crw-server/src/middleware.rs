use axum::Json;
use axum::extract::{MatchedPath, Request};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use crw_core::types::ApiResponse;
use std::sync::Arc;
use std::time::Instant;

use crate::error::ErrorCode;

/// Routes polled on a timer: liveness, readiness and the Prometheus endpoints.
/// [`request_log`] skips them, since at a probe every few seconds they would
/// outnumber real traffic and carry no signal.
pub fn is_probe_route(route: &str) -> bool {
    matches!(route, "/health" | "/ready" | "/metrics") || route.starts_with("/metrics/")
}

/// One line per request, written when the response is ready: method, the
/// matched route template, status, latency and, for an error envelope, its
/// `errorCode`. 5xx at WARN, everything else at INFO. Probes are logged only
/// when they fail, so a healthy poll costs no line and a failing one is seen.
///
/// The route is axum's `MatchedPath` (`/v1/crawl/{id}`), never the raw URI:
/// query strings and target URLs can carry customer tokens. No headers, no
/// bodies. Must be installed with `Router::layer` so routing has already run
/// and `MatchedPath` is in the request extensions.
pub async fn request_log(req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned());
    let probe = route.as_deref().is_some_and(is_probe_route);
    let method = req.method().clone();
    let started = Instant::now();
    let res = next.run(req).await;
    if probe && !res.status().is_server_error() {
        return res;
    }

    let latency_ms = started.elapsed().as_millis() as u64;
    let status = res.status().as_u16();
    let route = route.as_deref().unwrap_or("<unmatched>");
    // `Option` records nothing when `None`, so a success line has no field.
    let error_code = res.extensions().get::<ErrorCode>().map(|c| c.0);
    if res.status().is_server_error() {
        tracing::warn!(%method, route, status, latency_ms, error_code, "request failed");
    } else {
        tracing::info!(%method, route, status, latency_ms, error_code, "request");
    }
    res
}

/// Constant-time byte comparison to prevent timing attacks.
/// Does not leak the length of valid keys via timing side-channels:
/// always iterates over the longer of the two inputs.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let len = a.len().max(b.len());
    let mut result = (a.len() != b.len()) as u8;
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        result |= x ^ y;
    }
    result == 0
}

/// Exposed for integration tests only.
#[cfg(feature = "test-utils")]
pub fn constant_time_eq_pub(a: &[u8], b: &[u8]) -> bool {
    constant_time_eq(a, b)
}

/// Bearer token authentication middleware.
pub async fn auth_middleware(
    axum::extract::State(api_keys): axum::extract::State<Arc<Vec<String>>>,
    req: Request,
    next: Next,
) -> Response {
    if api_keys.is_empty() {
        return next.run(req).await;
    }

    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(header) if header.starts_with("Bearer ") => {
            let token = &header[7..];
            // Compare against all keys without short-circuiting to avoid
            // leaking which key index matched via timing.
            if api_keys.iter().fold(false, |found, k| {
                constant_time_eq(k.as_bytes(), token.as_bytes()) | found
            }) {
                next.run(req).await
            } else {
                let body = ApiResponse::<()>::err("Invalid API key");
                (StatusCode::UNAUTHORIZED, Json(body)).into_response()
            }
        }
        _ => {
            let body = ApiResponse::<()>::err("Missing Authorization header");
            (StatusCode::UNAUTHORIZED, Json(body)).into_response()
        }
    }
}
