//! Firecrawl-shaped error responses for the `/v2` compat surface.
//!
//! `AppError` is our native mapping and stays exactly as it is — `/v1` is our
//! documented API and its callers branch on its status codes and `errorCode`
//! spellings today. This wrapper exists so `/v2` can answer the way Firecrawl
//! answers without dragging `/v1` along.
//!
//! Two things differ from `AppError`:
//!
//! 1. **The key.** Firecrawl's envelope is `{success, code, error}`. Both
//!    official SDKs read `code`; our `errorCode` is invisible to every one of
//!    their error paths. We emit `code` with Firecrawl's spelling AND keep
//!    `errorCode` with ours — their SDKs ignore unknown keys, and the SaaS
//!    reads `errorCode` for `RequestLog` (`upstreamErrorCode` in
//!    `api-handler.ts`), so dropping it would blind our own request log.
//!
//! 2. **The status.** Firecrawl's `controllers/v2/scrape.ts` maps a handful of
//!    codes explicitly and lets everything else fall to
//!    `timeoutErr ? 408 : 500`. Notably a DNS failure is **HTTP 200** with
//!    `success:false`, not a 4xx.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use crw_core::error::CrwError;
use crw_core::types::ApiResponse;

use crate::error::{AppError, ErrorCode};

pub struct V2Error(pub CrwError);

impl From<CrwError> for V2Error {
    fn from(e: CrwError) -> Self {
        Self(e)
    }
}

impl From<AppError> for V2Error {
    fn from(e: AppError) -> Self {
        Self(e.0)
    }
}

impl From<JsonRejection> for V2Error {
    fn from(r: JsonRejection) -> Self {
        Self(AppError::from(r).0)
    }
}

/// Firecrawl's `ErrorCodes` spelling for one of ours, or `None` where Firecrawl
/// sends no `code` at all.
///
/// The `None` arms are captured, not guessed: a real
/// `api.firecrawl.dev/v2/scrape` 429 answers
/// `{"success":false,"error":"Rate limit exceeded. ..."}` with no `code`, and
/// `crawl-status.ts` answers a missing job with a bare
/// `{success:false, error:"Job not found"}`.
fn firecrawl_code(e: &CrwError) -> Option<&'static str> {
    match e {
        // NOT `SCRAPE_DNS_RESOLUTION_ERROR`, the obvious-looking pairing.
        // Firecrawl splits what we merge (a name that does not resolve -> 200
        // DNS; a port that refuses -> 500 SITE_ERROR) and `TargetUnreachable`
        // comes from `reqwest`'s `is_connect()`, true for both with the same
        // message. Neither answer is safe to claim; see FIRECRAWL-DIFF.md §4b.
        CrwError::TargetUnreachable(_) => Some("SCRAPE_SITE_ERROR"),
        // lib/error.ts: ScrapeJobTimeoutError.
        CrwError::Timeout(_) => Some("SCRAPE_TIMEOUT"),
        // scrapeURL/error.ts: NoEnginesLeftError — "every engine came back
        // unusable", which is what all three mean for us.
        //
        // `UnsupportedContentType` is deliberately NOT
        // `SCRAPE_UNSUPPORTED_FILE_ERROR`: that code is Firecrawl's file-UPLOAD
        // path only. A URL serving binary just exhausts the waterfall —
        // captured against `mock.fastcrw.com/bytes/1024`. Our own `errorCode`
        // still rides along, so no detail is lost.
        CrwError::HttpError(_)
        | CrwError::RendererError(_)
        | CrwError::UnsupportedContentType(_) => Some("SCRAPE_ALL_ENGINES_FAILED"),
        // Firecrawl validates with zod and returns a 400 without a code; its
        // taxonomy carries BAD_REQUEST for the hand-rolled cases.
        CrwError::InvalidRequest(_) | CrwError::UrlParseError(_) => Some("BAD_REQUEST"),
        // No `code` on the wire for these.
        CrwError::NotFound(_) | CrwError::RateLimited => None,
        _ => Some("UNKNOWN_ERROR"),
    }
}

/// The HTTP status Firecrawl answers with.
///
/// `SCRAPE_DNS_RESOLUTION_ERROR` returning **200** is not a typo — it is an
/// explicit branch in `controllers/v2/scrape.ts`:
///
/// ```js
/// // DNS resolution errors should return 200 with success: false
/// if (e.code === "SCRAPE_DNS_RESOLUTION_ERROR") {
///   return res.status(200).json({ success: false, code: e.code, error: e.message });
/// }
/// ```
fn firecrawl_status(e: &CrwError) -> StatusCode {
    match e {
        // Left at 422, the status this surface already answered. See
        // `firecrawl_code` — we cannot tell a DNS failure (their 200) from a
        // refused connection (their 500), so neither of their answers is safe,
        // and 422 at least does not claim success.
        CrwError::TargetUnreachable(_) => StatusCode::UNPROCESSABLE_ENTITY,
        CrwError::Timeout(_) => StatusCode::REQUEST_TIMEOUT,
        CrwError::InvalidRequest(_) | CrwError::UrlParseError(_) => StatusCode::BAD_REQUEST,
        CrwError::NotFound(_) => StatusCode::NOT_FOUND,
        CrwError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        CrwError::SearchDisabled(_) | CrwError::SearchDegraded(_) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        // Left at 422 rather than swept into the catch-all below. This is what
        // `/v2/parse` answers for a corrupt upload, it is pinned by
        // `parse_tests::parse_corrupt_pdf_422`, and nothing captured or read
        // says Firecrawl's own parse controller answers differently — moving it
        // would be a guess that breaks a deliberate, tested behaviour.
        CrwError::ExtractionError(_) => StatusCode::UNPROCESSABLE_ENTITY,
        // Everything else, including the unsupported-file and all-engines-failed
        // cases that `AppError` reports as 422/502, lands on Firecrawl's
        // catch-all 500.
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl IntoResponse for V2Error {
    fn into_response(self) -> Response {
        let status = firecrawl_status(&self.0);
        let mut body =
            ApiResponse::<()>::err_with_code(self.0.to_string(), self.0.error_code().to_string());
        body.code = firecrawl_code(&self.0).map(str::to_string);
        let mut res = (status, Json(body)).into_response();
        res.extensions_mut().insert(ErrorCode(self.0.error_code()));
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives the real `into_response` and reads what it actually put on the
    /// wire. An earlier version of this rebuilt the envelope from
    /// `firecrawl_code` instead, which meant deleting the `body.code` line from
    /// `into_response` left every test here passing.
    async fn wire(e: CrwError) -> (StatusCode, serde_json::Value) {
        let res = V2Error(e).into_response();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("error body is always small and in memory");
        (status, serde_json::from_slice(&bytes).expect("valid JSON"))
    }

    /// Firecrawl answers a DNS failure with HTTP 200 and a refused connection
    /// with HTTP 500 (both captured live). `TargetUnreachable` comes from
    /// `reqwest::Error::is_connect()`, true for both, so we must claim neither.
    /// The 200 is the one worth guarding: answering it for a failed fetch is
    /// the outcome that actively misleads.
    #[tokio::test]
    async fn unreachable_target_does_not_claim_firecrawls_dns_answer() {
        let (st, body) = wire(CrwError::TargetUnreachable("nx.example".into())).await;
        assert_ne!(st, StatusCode::OK, "a failed fetch must not answer 200");
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["success"], false);
        assert_eq!(body["code"], "SCRAPE_SITE_ERROR");
    }

    #[tokio::test]
    async fn timeout_is_408_not_504() {
        let (st, body) = wire(CrwError::Timeout(30_000)).await;
        assert_eq!(st, StatusCode::REQUEST_TIMEOUT);
        assert_eq!(body["code"], "SCRAPE_TIMEOUT");
    }

    /// Captured against mock.fastcrw.com/bytes/1024. SCRAPE_UNSUPPORTED_FILE_ERROR
    /// is the upload path only; pinning that here is the mistake this prevents.
    #[tokio::test]
    async fn binary_url_is_500_all_engines_failed_not_unsupported_file() {
        let (st, body) = wire(CrwError::UnsupportedContentType("application/zip".into())).await;
        assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["code"], "SCRAPE_ALL_ENGINES_FAILED");
        assert_eq!(body["errorCode"], "unsupported_content_type");
    }

    /// `code` is what firecrawl-py / firecrawl-js read; `errorCode` is what the
    /// SaaS logs (`upstreamErrorCode`). Dropping either breaks one consumer.
    #[tokio::test]
    async fn both_code_and_error_code_are_emitted() {
        let (_, body) = wire(CrwError::Timeout(1)).await;
        assert_eq!(body["code"], "SCRAPE_TIMEOUT");
        assert_eq!(body["errorCode"], "timeout");
    }

    /// A captured Firecrawl 429 carries no `code`, and crawl-status.ts's "Job
    /// not found" carries none either. Emitting one would invent a field their
    /// SDKs would then see.
    #[tokio::test]
    async fn codeless_errors_omit_the_key_entirely() {
        let (_, rate) = wire(CrwError::RateLimited).await;
        assert!(rate.get("code").is_none());
        let (st, nf) = wire(CrwError::NotFound("crawl job x".into())).await;
        assert!(nf.get("code").is_none());
        assert_eq!(st, StatusCode::NOT_FOUND);
    }
}
