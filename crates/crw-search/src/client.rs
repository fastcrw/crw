//! HTTP client for SearXNG's JSON search API.
//!
//! Mirrors `crw-saas/src/lib/searxng-client.ts`. The shape of the response
//! follows the SearXNG `search_api` docs and the `result_types/index` page —
//! every per-result field except `url`, `title`, and `engine` is treated as
//! optional because real-world engines are uneven.

use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

use crate::params::SearxngParams;

/// Hard cap on a SearXNG JSON response body (10 MiB). Real responses are
/// well under 1 MiB; anything bigger is a sign of upstream misbehavior or a
/// memory-amplification attack, so we abort the read instead of allocating it.
const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

/// Tighter cap for non-2xx error bodies. We only surface the first 500 chars
/// to the caller anyway, so a 64 KiB ceiling is plenty for diagnostics while
/// closing the door on hostile upstreams that retaliate to invalid params
/// with multi-megabyte error pages.
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Signals to the search backend that this request may use a metered rescue
/// tier if the free legs come back empty. Sent ONLY when
/// [`SearxngParams::paid_rescue`] is set, which only crw-saas can cause.
///
/// Positive opt-in on purpose. The inverse design — "spend unless told not to"
/// — cannot fail closed: a caller that forgets the header, or a self-host
/// pointing at a backend that honours it, would silently start spending.
pub const PAID_RESCUE_HEADER: &str = "X-Crw-Paid-Rescue";

async fn read_capped(response: reqwest::Response, cap: usize) -> Result<Vec<u8>, SearchError> {
    if let Some(declared) = response.content_length()
        && declared as usize > cap
    {
        return Err(SearchError::InvalidResponse(format!(
            "response too large: declared {declared} bytes exceeds {cap} cap"
        )));
    }
    let (buf, truncated) = crw_core::body::read_capped(response.bytes_stream(), cap)
        .await
        .map_err(|e: reqwest::Error| {
            // Same reason as the `send()` arm below (issue #90): the embedded
            // request URL can carry the backend host and its credentials.
            SearchError::Transport(crw_core::error::reqwest_message(e))
        })?;
    if truncated {
        return Err(SearchError::InvalidResponse(format!(
            "response too large: exceeded {cap}-byte cap"
        )));
    }
    Ok(buf)
}

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("SearXNG request timed out")]
    Timeout,
    #[error("SearXNG upstream error (status {status}): {body}")]
    Upstream { status: u16, body: String },
    #[error("SearXNG returned an invalid JSON response: {0}")]
    InvalidResponse(String),
    #[error("SearXNG transport error: {0}")]
    Transport(String),
}

/// A single result row from SearXNG. Every field is `Option` because real
/// engines occasionally return malformed rows (missing url/title/engine in
/// flaky upstream responses). The transform layer drops any row missing the
/// load-bearing fields rather than failing the entire search response — see
/// `transform.rs`.
#[derive(Debug, Clone, Deserialize)]
pub struct SearxngResult {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub engine: Option<String>,
    /// Snippet / description. SearXNG calls this `content`; the public API
    /// renames it to `description`.
    #[serde(default)]
    pub content: Option<String>,
    /// Relevance score (higher is better). Missing on engines that don't rank.
    #[serde(default)]
    pub score: Option<f64>,
    /// Per-engine identifiers that returned this row (SearXNG `format=json`
    /// emits this when a result is found by more than one engine). Used by the
    /// re-rank pipeline for engine-aware bookkeeping; harmless on the raw path.
    #[serde(default)]
    pub engines: Vec<String>,
    /// Per-engine ranks for this row (one entry per engine in `engines`).
    /// Drives Reciprocal Rank Fusion in the re-rank pipeline. Empty on the
    /// rare engines that don't report a position.
    #[serde(default)]
    pub positions: Vec<u32>,
    /// Top-level category bucket reported by SearXNG (`general`, `news`,
    /// `images`, `videos`, ...).
    #[serde(default)]
    pub category: Option<String>,
    /// Template hint (`default.html`, `images.html`, `videos.html`,
    /// `paper.html`, ...). Useful as a fallback when `category` is missing.
    #[serde(default)]
    pub template: Option<String>,
    /// ISO-formatted publish date for news results.
    #[serde(default, rename = "publishedDate")]
    pub published_date: Option<String>,
    /// Image URL — populated for image-template results.
    #[serde(default)]
    pub img_src: Option<String>,
    /// Thumbnail URL — populated for image / video results.
    #[serde(default)]
    pub thumbnail_src: Option<String>,
    #[serde(default)]
    pub img_format: Option<String>,
    #[serde(default)]
    pub resolution: Option<String>,
}

/// Top-level SearXNG `format=json` response envelope.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SearxngResponse {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub number_of_results: u64,
    #[serde(default)]
    pub results: Vec<SearxngResult>,
    #[serde(default)]
    pub answers: Vec<serde_json::Value>,
    #[serde(default)]
    pub corrections: Vec<String>,
    #[serde(default)]
    pub infoboxes: Vec<serde_json::Value>,
    #[serde(default)]
    pub suggestions: Vec<String>,
    #[serde(default)]
    pub unresponsive_engines: Vec<serde_json::Value>,
    /// Explicit degraded flag an upstream orchestrator may set. A plain
    /// SearXNG backend never sets this, hence the serde default.
    #[serde(default)]
    pub degraded: bool,
}

impl SearxngResponse {
    /// True when the backend answered with nothing AND reported that engines
    /// failed. Emptiness is a PREREQUISITE for both signals: a response that
    /// some later leg rescued into a non-empty pool is never degraded.
    pub fn is_degraded(&self) -> bool {
        self.results.is_empty() && (self.degraded || !self.unresponsive_engines.is_empty())
    }
}

/// Thin async client for SearXNG. One instance per server; reuse across
/// requests so the underlying `reqwest::Client` connection pool is hot.
#[derive(Debug, Clone)]
pub struct SearxngClient {
    http: Arc<reqwest::Client>,
    base_url: String,
    timeout: Duration,
}

impl SearxngClient {
    pub fn new(http: Arc<reqwest::Client>, base_url: impl Into<String>, timeout: Duration) -> Self {
        let base_url = base_url.into();
        let trimmed = base_url.trim_end_matches('/').to_string();
        Self {
            http,
            base_url: trimmed,
            timeout,
        }
    }

    /// Configured base URL (trailing slash trimmed). Exposed so the route layer
    /// can name the host in `target_unreachable` errors without leaking it raw
    /// (callers sanitize to the origin first — see crw-server `diagnostics`).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Issue a JSON search request. Errors surface as a typed [`SearchError`]
    /// — the route layer maps them onto `CrwError` for HTTP responses.
    pub async fn fetch(&self, params: &SearxngParams) -> Result<SearxngResponse, SearchError> {
        let mut url = url::Url::parse(&format!("{}/search", self.base_url))
            .map_err(|e| SearchError::Transport(format!("invalid base_url: {e}")))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("format", "json");
            q.append_pair("q", &params.q);
            if let Some(c) = &params.categories {
                q.append_pair("categories", c);
            }
            if let Some(l) = &params.language {
                q.append_pair("language", l);
            }
            if let Some(t) = &params.time_range {
                q.append_pair("time_range", t);
            }
            if let Some(e) = &params.engines {
                q.append_pair("engines", e);
            }
            if let Some(p) = params.pageno {
                q.append_pair("pageno", &p.to_string());
            }
            if let Some(s) = params.safesearch {
                q.append_pair("safesearch", &s.to_string());
            }
        }

        // A HEADER, not a query parameter: the entitlement belongs to the
        // caller, not to the query, so it must not widen the search surface nor
        // enter the backend's cache key (two callers asking the same thing must
        // still share one cached answer). Absent header = today's exact request,
        // byte for byte, which is what keeps self-host and every non-SaaS caller
        // unchanged.
        let mut req = self.http.get(url).timeout(self.timeout);
        if params.paid_rescue {
            req = req.header(PAID_RESCUE_HEADER, "1");
        }
        let response = req.send().await.map_err(|e: reqwest::Error| {
            if e.is_timeout() {
                SearchError::Timeout
            } else {
                // `without_url()` strips reqwest's embedded request URL from
                // the Display string — that URL can carry credentials/tokens
                // (issue #90). The route layer logs the sanitized origin instead.
                SearchError::Transport(crw_core::error::reqwest_message(e))
            }
        })?;

        let status = response.status();
        if !status.is_success() {
            // Apply the same streaming cap to the error path. Without it, a
            // hostile upstream could retaliate to an invalid query with a
            // multi-megabyte 4xx body and push us into unbounded allocation
            // — even though we only display the first 500 chars.
            let body_bytes = read_capped(response, MAX_ERROR_BODY_BYTES)
                .await
                .unwrap_or_default();
            let body = String::from_utf8_lossy(&body_bytes);
            let trimmed: String = body.chars().take(500).collect();
            return Err(SearchError::Upstream {
                status: status.as_u16(),
                body: trimmed,
            });
        }

        // Stream the body with a hard byte cap so a misbehaving upstream
        // can't push us into unbounded allocation. We refuse to parse past
        // `MAX_RESPONSE_BYTES`. `Content-Length` is not trusted (chunked
        // encoding sets none) — `read_capped` enforces on the running sum.
        let buf = read_capped(response, MAX_RESPONSE_BYTES).await?;
        serde_json::from_slice::<SearxngResponse>(&buf)
            .map_err(|e| SearchError::InvalidResponse(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_degraded_true_on_empty_results_with_unresponsive_engines() {
        let resp = SearxngResponse {
            unresponsive_engines: vec![serde_json::json!(["google", "timeout"])],
            ..Default::default()
        };
        assert!(resp.is_degraded());
    }

    #[test]
    fn is_degraded_false_on_genuine_zero_results() {
        // Empty results, no unresponsive engines, no degraded flag: a real
        // zero-result query, not a backend failure — must stay a normal 200.
        let resp = SearxngResponse::default();
        assert!(!resp.is_degraded());
    }

    #[test]
    fn is_degraded_false_when_results_non_empty() {
        // Emptiness is a prerequisite: a later leg that rescued the pool into
        // non-empty results is never degraded, even if engines failed.
        let resp: SearxngResponse = serde_json::from_value(serde_json::json!({
            "results": [{"url": "https://example.com", "title": "Example", "engine": "google"}],
            "unresponsive_engines": [["google", "timeout"]],
        }))
        .unwrap();
        assert!(!resp.is_degraded());
    }

    // --- is_degraded: the explicit `degraded` flag, independent of unresponsive_engines ---

    #[test]
    fn is_degraded_true_via_the_explicit_flag_with_no_unresponsive_engines() {
        let resp = SearxngResponse {
            degraded: true,
            ..Default::default()
        };
        assert!(resp.is_degraded());
    }

    #[test]
    fn is_degraded_false_when_the_explicit_flag_is_set_but_results_are_non_empty() {
        let resp: SearxngResponse = serde_json::from_value(serde_json::json!({
            "results": [{"url": "https://example.com", "title": "Example", "engine": "google"}],
            "degraded": true,
        }))
        .unwrap();
        assert!(!resp.is_degraded());
    }

    // --- SearchError Display ---

    #[test]
    fn search_error_display_messages() {
        assert_eq!(
            SearchError::Timeout.to_string(),
            "SearXNG request timed out"
        );
        assert_eq!(
            SearchError::Upstream {
                status: 403,
                body: "forbidden".into()
            }
            .to_string(),
            "SearXNG upstream error (status 403): forbidden"
        );
        assert_eq!(
            SearchError::InvalidResponse("bad json".into()).to_string(),
            "SearXNG returned an invalid JSON response: bad json"
        );
        assert_eq!(
            SearchError::Transport("connection refused".into()).to_string(),
            "SearXNG transport error: connection refused"
        );
    }

    #[test]
    fn paid_rescue_header_name_is_stable() {
        // Self-host and non-SaaS callers depend on this header NEVER being
        // sent; a silent rename here would flip the fail-closed default.
        assert_eq!(PAID_RESCUE_HEADER, "X-Crw-Paid-Rescue");
    }

    // --- SearxngResult / SearxngResponse deserialization ---

    #[test]
    fn searxng_result_deserializes_with_only_the_three_load_bearing_fields() {
        let v = serde_json::json!({"url": "https://a.com/", "title": "A", "engine": "google"});
        let r: SearxngResult = serde_json::from_value(v).unwrap();
        assert_eq!(r.url.as_deref(), Some("https://a.com/"));
        assert!(r.content.is_none());
        assert!(r.score.is_none());
        assert!(r.engines.is_empty());
        assert!(r.positions.is_empty());
        assert!(r.category.is_none());
    }

    #[test]
    fn searxng_result_deserializes_from_a_completely_empty_object() {
        // Every field is `#[serde(default)]`, including url/title/engine —
        // the transform layer, not serde, is responsible for dropping rows
        // that are missing them.
        let r: SearxngResult = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(r.url.is_none() && r.title.is_none() && r.engine.is_none());
    }

    #[test]
    fn searxng_response_defaults_every_collection_when_absent() {
        let resp: SearxngResponse = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(resp.results.is_empty());
        assert!(resp.answers.is_empty());
        assert!(resp.corrections.is_empty());
        assert!(resp.infoboxes.is_empty());
        assert!(resp.suggestions.is_empty());
        assert!(resp.unresponsive_engines.is_empty());
        assert!(!resp.degraded);
        assert_eq!(resp.number_of_results, 0);
    }

    #[test]
    fn searxng_response_ignores_unknown_fields() {
        let v = serde_json::json!({
            "query": "belgrade pizza",
            "number_of_results": 3,
            "results": [],
            "engine_data": {"totally": "unexpected"},
        });
        let resp: SearxngResponse = serde_json::from_value(v).unwrap();
        assert_eq!(resp.query, "belgrade pizza");
        assert_eq!(resp.number_of_results, 3);
    }

    // --- SearxngClient::new / base_url ---

    #[test]
    fn new_trims_a_single_trailing_slash() {
        let c = SearxngClient::new(
            std::sync::Arc::new(reqwest::Client::new()),
            "http://localhost:8080/",
            Duration::from_secs(1),
        );
        assert_eq!(c.base_url(), "http://localhost:8080");
    }

    #[test]
    fn new_trims_all_repeated_trailing_slashes() {
        let c = SearxngClient::new(
            std::sync::Arc::new(reqwest::Client::new()),
            "http://localhost:8080///",
            Duration::from_secs(1),
        );
        assert_eq!(c.base_url(), "http://localhost:8080");
    }

    #[test]
    fn new_leaves_a_url_without_a_trailing_slash_unchanged() {
        let c = SearxngClient::new(
            std::sync::Arc::new(reqwest::Client::new()),
            "http://localhost:8080",
            Duration::from_secs(1),
        );
        assert_eq!(c.base_url(), "http://localhost:8080");
    }

    // --- fetch(): real loopback HTTP against a local wiremock server ---
    // TESTABILITY: unlike `research.rs` (hardcoded api.openalex.org /
    // semanticscholar.org / export.arxiv.org base URLs), `SearxngClient` takes
    // its base_url as a constructor argument, so its HTTP layer IS fully
    // coverable against a local `wiremock::MockServer` — no production change
    // needed.

    fn test_client(base_url: &str) -> SearxngClient {
        SearxngClient::new(
            std::sync::Arc::new(reqwest::Client::new()),
            base_url,
            Duration::from_secs(5),
        )
    }

    fn params(q: &str) -> SearxngParams {
        SearxngParams {
            q: q.to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn fetch_parses_a_normal_success_response() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/search"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({
                    "query": "rust",
                    "number_of_results": 1,
                    "results": [{"url": "https://a.com/", "title": "Rust", "engine": "google", "content": "a language"}],
                }),
            ))
            .mount(&server)
            .await;

        let resp = test_client(&server.uri())
            .fetch(&params("rust"))
            .await
            .unwrap();
        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0].title.as_deref(), Some("Rust"));
        assert!(!resp.is_degraded());
    }

    #[tokio::test]
    async fn fetch_zero_results_with_no_unresponsive_engines_is_not_degraded() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/search"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"query": "asdkjhaskjdh", "results": []})),
            )
            .mount(&server)
            .await;

        let resp = test_client(&server.uri())
            .fetch(&params("asdkjhaskjdh"))
            .await
            .unwrap();
        assert!(resp.results.is_empty());
        assert!(
            !resp.is_degraded(),
            "a genuine zero-result query must not be reported as degraded"
        );
    }

    #[tokio::test]
    async fn fetch_zero_results_with_unresponsive_engines_is_search_degraded() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/search"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "query": "rust",
                    "results": [],
                    "unresponsive_engines": [["google", "timeout"]],
                })),
            )
            .mount(&server)
            .await;

        let resp = test_client(&server.uri())
            .fetch(&params("rust"))
            .await
            .unwrap();
        assert!(
            resp.is_degraded(),
            "empty results + an unresponsive engine must read as degraded, not a clean empty"
        );
    }

    #[tokio::test]
    async fn fetch_403_json_api_disabled_maps_to_upstream_error() {
        // The stock SearXNG image ships with the JSON API OFF; asking it for
        // `format=json` without enabling it returns a 403.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/search"))
            .respond_with(wiremock::ResponseTemplate::new(403).set_body_string("Forbidden"))
            .mount(&server)
            .await;

        let err = test_client(&server.uri())
            .fetch(&params("rust"))
            .await
            .unwrap_err();
        match err {
            SearchError::Upstream { status, body } => {
                assert_eq!(status, 403);
                assert_eq!(body, "Forbidden");
            }
            other => panic!("expected Upstream(403), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_5xx_maps_to_upstream_error() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/search"))
            .respond_with(wiremock::ResponseTemplate::new(502).set_body_string("Bad Gateway"))
            .mount(&server)
            .await;

        let err = test_client(&server.uri())
            .fetch(&params("rust"))
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::Upstream { status: 502, .. }));
    }

    #[tokio::test]
    async fn fetch_error_body_is_truncated_to_500_chars() {
        let long_body = "x".repeat(2_000);
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/search"))
            .respond_with(wiremock::ResponseTemplate::new(400).set_body_string(&long_body))
            .mount(&server)
            .await;

        let err = test_client(&server.uri())
            .fetch(&params("rust"))
            .await
            .unwrap_err();
        match err {
            SearchError::Upstream { body, .. } => assert_eq!(body.len(), 500),
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_oversized_error_body_degrades_to_an_empty_body_instead_of_failing() {
        // A hostile/misbehaving upstream retaliating to a bad request with a
        // multi-megabyte 4xx body must not blow up the caller: `read_capped`
        // errors internally, `unwrap_or_default()` swallows it, and the
        // caller still gets a typed Upstream error (with an empty body)
        // rather than a generic transport failure.
        let huge_body = "x".repeat(MAX_ERROR_BODY_BYTES + 1024);
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/search"))
            .respond_with(wiremock::ResponseTemplate::new(400).set_body_string(&huge_body))
            .mount(&server)
            .await;

        let err = test_client(&server.uri())
            .fetch(&params("rust"))
            .await
            .unwrap_err();
        match err {
            SearchError::Upstream { status, body } => {
                assert_eq!(status, 400);
                assert!(body.is_empty());
            }
            other => panic!("expected Upstream(400), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_invalid_json_body_is_an_invalid_response_error() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/search"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("not json at all"))
            .mount(&server)
            .await;

        let err = test_client(&server.uri())
            .fetch(&params("rust"))
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn fetch_times_out_when_the_upstream_is_slower_than_the_client_timeout() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/search"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"results": []}))
                    .set_delay(Duration::from_millis(60)),
            )
            .mount(&server)
            .await;

        let client = SearxngClient::new(
            std::sync::Arc::new(reqwest::Client::new()),
            server.uri(),
            Duration::from_millis(5),
        );
        let err = client.fetch(&params("rust")).await.unwrap_err();
        assert!(matches!(err, SearchError::Timeout));
    }

    #[tokio::test]
    async fn fetch_sends_all_optional_query_params_and_the_paid_rescue_header() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/search"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"results": []})),
            )
            .mount(&server)
            .await;

        let full = SearxngParams {
            q: "belgrade pizza".into(),
            categories: Some("general".into()),
            language: Some("en".into()),
            time_range: Some("day".into()),
            engines: Some("google,bing".into()),
            pageno: Some(2),
            safesearch: Some(1),
            paid_rescue: true,
        };
        test_client(&server.uri()).fetch(&full).await.unwrap();

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        let req = &received[0];
        let qp: std::collections::HashMap<String, String> =
            req.url.query_pairs().into_owned().collect();
        assert_eq!(qp.get("format").map(String::as_str), Some("json"));
        assert_eq!(qp.get("q").map(String::as_str), Some("belgrade pizza"));
        assert_eq!(qp.get("categories").map(String::as_str), Some("general"));
        assert_eq!(qp.get("language").map(String::as_str), Some("en"));
        assert_eq!(qp.get("time_range").map(String::as_str), Some("day"));
        assert_eq!(qp.get("engines").map(String::as_str), Some("google,bing"));
        assert_eq!(qp.get("pageno").map(String::as_str), Some("2"));
        assert_eq!(qp.get("safesearch").map(String::as_str), Some("1"));
        assert_eq!(
            req.headers
                .get(PAID_RESCUE_HEADER)
                .map(|v| v.to_str().unwrap()),
            Some("1")
        );
    }

    #[tokio::test]
    async fn fetch_omits_the_paid_rescue_header_by_default() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/search"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"results": []})),
            )
            .mount(&server)
            .await;

        test_client(&server.uri())
            .fetch(&params("rust"))
            .await
            .unwrap();

        let received = server.received_requests().await.unwrap();
        assert!(received[0].headers.get(PAID_RESCUE_HEADER).is_none());
    }

    #[tokio::test]
    async fn fetch_rejects_an_unparseable_base_url_without_making_a_request() {
        // Malformed base_url is caught synchronously by `url::Url::parse`
        // inside `fetch`, before any I/O — no mock server needed at all.
        let client = SearxngClient::new(
            std::sync::Arc::new(reqwest::Client::new()),
            "not a url \n with whitespace",
            Duration::from_secs(1),
        );
        let result = client.fetch(&params("rust")).await;
        match result {
            Err(SearchError::Transport(msg)) => assert!(msg.starts_with("invalid base_url:")),
            other => panic!("expected Transport(invalid base_url), got {other:?}"),
        }
    }
}
