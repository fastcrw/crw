//! Cloak Turnstile-solver recovery tier (opt-in, REST — not CDP).
//!
//! Backed by a `cloudflarebypassforscraping` / CloakBrowser REST sidecar. Fired
//! ONLY as a Cloudflare-managed-challenge recovery arm (see `lib.rs`), never in
//! the normal ladder. Model B: the sidecar owns the stealth browser (mint), the
//! `cf_clearance` cache, and the curl_cffi warm replay; this tier just calls its
//! **mirror endpoint** and gets back the real upstream HTML.
//!
//! ## Request (mirror endpoint)
//! `GET {base}/{path}{?query}` with headers:
//! - `x-hostname: <host>` (required — the sidecar mirrors to `https://<host>/…`)
//! - `x-proxy: http://<user>__cr.<cc>;sessid.stick<id>:<pass>@<host:port>`
//!   (DataImpulse sticky exit; a **stable per-host sessid** for warm reuse, a
//!   **fresh** one per retry for a new IP)
//! - `x-bypass-cache: true` on the retry (force a fresh solve)
//!
//! The returned body is the real upstream response; a still-challenged body is
//! re-checked with the shared [`crate::detector::looks_like_cloudflare_challenge`]
//! accept-gate and surfaced as a retryable [`CrwError::RendererError`].

use async_trait::async_trait;
use crw_core::Deadline;
use crw_core::error::{CrwError, CrwResult};
use crw_core::types::FetchResult;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::traits::PageFetcher;

/// Budget for the `is_available` health probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Fail-fast connect timeout so a down sidecar errors quickly instead of
/// eating the full per-request budget.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a proven-good sticky sessid is remembered per host. Under the
/// sidecar's `cf_clearance` cache TTL (29 min) so we re-mint before it dies.
const SESSID_TTL: Duration = Duration::from_secs(29 * 60);
/// Max solve attempts per fetch (1 sticky + 1 fresh-IP). A second COLD attempt
/// rarely fits the recovery budget, so this mainly recovers the warm/poison
/// case; best-result-wins leaves a clean block on exhaustion.
const MAX_ATTEMPTS: usize = 2;

/// Opt-in cloak recovery renderer. Construct via [`CloakRenderer::new`].
pub struct CloakRenderer {
    name: String,
    /// Trimmed base URL, no trailing slash (e.g. `http://cloak-sidecar:8000`).
    base_url: String,
    /// Bearer token; empty string means no `Authorization` header is sent.
    api_key: String,
    /// Per-attempt solve budget (`config.cloak_timeout()`).
    timeout: Duration,
    /// DataImpulse base credentials `(user, pass)` from config
    /// (`proxy_base_user`/`proxy_base_pass`). `None` on a self-host without a
    /// proxy → the sidecar egresses direct.
    proxy_base: Option<(String, String)>,
    /// Default country for the `__cr.<cc>` suffix (`config.proxy_default_country`).
    default_country: Option<String>,
    /// Proxy `scheme://host:port` for cloak to self-provision a residential exit
    /// when the per-request `REQUEST_PROXY` is absent (creds come from
    /// `proxy_base`). `None` (unset/empty) keeps today's REQUEST_PROXY-only
    /// behavior, so a build without this configured is byte-identical.
    proxy_host: Option<String>,
    client: reqwest::Client,
    /// host → (proven-good sessid, minted_at). Lets a host that only works on a
    /// fresh IP promote that sessid to the primary attempt so it warms.
    sessid_map: Arc<Mutex<HashMap<String, (String, Instant)>>>,
}

impl CloakRenderer {
    pub fn new(
        name: &str,
        base_url: &str,
        api_key: &str,
        timeout_ms: u64,
        proxy_base: Option<(String, String)>,
        default_country: Option<String>,
        proxy_host: Option<String>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            name: name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            timeout: Duration::from_millis(timeout_ms),
            proxy_base,
            default_country,
            proxy_host: proxy_host.filter(|h| !h.trim().is_empty()),
            client,
            sessid_map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.api_key.is_empty() {
            rb
        } else {
            rb.bearer_auth(&self.api_key)
        }
    }

    fn call_budget(&self, deadline: &Deadline) -> Duration {
        deadline.remaining().min(self.timeout)
    }

    /// Compose the DataImpulse sticky proxy URL for one attempt, injecting the
    /// country suffix + `;sessid.stick<id>` into the base credentials, and the
    /// creds into the proxy `scheme://host:port`. Returns `None` when no proxy
    /// is configured (self-host → sidecar egresses direct). Mirrors the
    /// `<user>__cr.<cc>` composition in `cdp.rs`.
    fn sticky_proxy_url(&self, sessid: &str) -> Option<String> {
        let (user, pass) = self.proxy_base.as_ref()?;
        // Host:port comes from the request's REQUEST_PROXY entry (creds-free).
        // Prefer the per-request managed proxy; fall back to the configured cloak
        // proxy host so the recovery arm still gets a residential exit when no
        // managed proxy was injected (the common case). Unset host => None => the
        // `?` returns None => no x-proxy, exactly like today.
        let server = crate::REQUEST_PROXY
            .try_with(|p| p.as_ref().map(|e| e.chrome_proxy_server().to_string()))
            .ok()
            .flatten()
            .or_else(|| self.proxy_host.clone())?;
        let cc = crate::REQUEST_COUNTRY
            .try_with(|c| c.clone())
            .ok()
            .flatten()
            .or_else(|| self.default_country.clone())
            .map(|s| s.trim().to_lowercase())
            .filter(|s| s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic()));
        let username = match cc {
            Some(cc) => format!("{user}__cr.{cc};sessid.stick{sessid}"),
            None => format!("{user};sessid.stick{sessid}"),
        };
        // server is "scheme://host:port" (no creds); inject "user:pass@".
        Some(server.replacen("://", &format!("://{username}:{pass}@"), 1))
    }

    /// One mirror call. `bypass_cache` forces a fresh sidecar solve.
    async fn mirror_once(
        &self,
        host: &str,
        path_and_query: &str,
        sessid: &str,
        bypass_cache: bool,
        deadline: &Deadline,
    ) -> CrwResult<(u16, String)> {
        let budget = self.call_budget(deadline);
        if budget.is_zero() {
            return Err(CrwError::Timeout(deadline.requested_ms()));
        }
        let url = format!("{}{}", self.base_url, path_and_query);
        let mut rb = self.auth(self.client.get(&url)).header("x-hostname", host);
        if let Some(px) = self.sticky_proxy_url(sessid) {
            rb = rb.header("x-proxy", px);
        }
        if bypass_cache {
            rb = rb.header("x-bypass-cache", "true");
        }
        // Propagate our per-call budget so the sidecar can deliver a slow cold
        // solve in-band (up to what we will actually wait) instead of fail-opening
        // at its conservative default. UNCONDITIONAL: must be sent on every mirror
        // call, including attempt 0 (bypass=false) — the generous-deadline
        // first-request case this exists for. `budget` is guarded non-zero above.
        rb = rb.header("x-deadline-ms", budget.as_millis().to_string());
        let resp = tokio::time::timeout(budget, rb.send())
            .await
            .map_err(|_| CrwError::Timeout(budget.as_millis() as u64))?
            .map_err(|e| {
                // Same rule as camoufox: the mirror base URL is ours, not the
                // caller's. Log it, do not return it.
                tracing::warn!(base_url = %self.base_url, "cloak request failed: {e}");
                CrwError::RendererError(format!(
                    "cloak GET {path_and_query}: {}",
                    crw_core::error::reqwest_message(e)
                ))
            })?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(|e| {
            CrwError::RendererError(format!(
                "cloak GET {path_and_query} body: {}",
                crw_core::error::reqwest_message(e)
            ))
        })?;
        Ok((status, body))
    }
}

#[async_trait]
impl PageFetcher for CloakRenderer {
    async fn fetch(
        &self,
        url: &str,
        _headers: &HashMap<String, String>,
        _wait_for_ms: Option<u64>,
        deadline: Deadline,
    ) -> CrwResult<FetchResult> {
        if deadline.expired() {
            return Err(CrwError::Timeout(deadline.requested_ms()));
        }
        let start = Instant::now();
        let parsed = url::Url::parse(url)
            .map_err(|e| CrwError::RendererError(format!("cloak: bad url {url}: {e}")))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| CrwError::RendererError(format!("cloak: url has no host: {url}")))?
            .to_string();
        let path_and_query = match parsed.query() {
            Some(q) => format!("{}?{}", parsed.path(), q),
            None => parsed.path().to_string(),
        };

        // A proven-good sessid for this host if we have a warm one (survived the
        // TTL). We do NOT fall back to a deterministic per-host sessid on a cold
        // host: that pinned every cold attempt to one fixed exit IP, so if that IP
        // was blocked (common on hard CF hosts) a tight recovery budget gave the
        // host a single doomed shot. A fresh random sessid = a new exit IP per
        // attempt — measured ~87% CF-solve vs ~8% for the pinned path — and still
        // warms the cache on success below.
        let warm_sessid: Option<String> = {
            let mut map = self.sessid_map.lock().expect("cloak sessid map poisoned");
            match map.get(&host) {
                Some((s, at)) if at.elapsed() < SESSID_TTL => Some(s.clone()),
                _ => {
                    map.remove(&host);
                    None
                }
            }
        };

        let mut last_err: Option<CrwError> = None;
        for attempt in 0..MAX_ATTEMPTS {
            if deadline.remaining() < crate::MIN_TIER_BUDGET {
                break;
            }
            let (sessid, bypass) = match (attempt, &warm_sessid) {
                // Warm reuse: a proven-good exit IP, use the sidecar cache.
                (0, Some(s)) => (s.clone(), false),
                // Cold host, or the warm attempt already failed: a fresh random
                // exit IP + a forced fresh solve.
                _ => {
                    let n: u64 = rand::random();
                    (format!("crwr{n:016x}"), true)
                }
            };
            match self
                .mirror_once(&host, &path_and_query, &sessid, bypass, &deadline)
                .await
            {
                // Accept a body that is NOT itself a CF challenge and carries
                // enough text to be a page. Do NOT gate on 2xx: after the sidecar
                // clears the challenge, the real upstream may legitimately return
                // 403/451 (paywall/geo) with usable content, which the caller's
                // own `r_ok` (content-quality, not status) accepts. The status is
                // preserved in `status_code`.
                //
                // The length floor is the second criterion because the sidecar
                // FAILS OPEN with an empty 200 on load-shed, on timeout and on any
                // internal error, and an empty body is not a CF challenge. Without
                // this floor that fail-open was ACCEPTED here, with three
                // consequences: the `for attempt in 0..MAX_ATTEMPTS` loop returned
                // on the first pass so the fresh-sessid retry below was dead code
                // exactly when it was needed; the sessid bank a few lines down
                // recorded an exit that had proven nothing as good for SESSID_TTL
                // (29 min); and the caller then measured 0 bytes, scored `r_ok`
                // false and booked a RenderError with no way to tell the
                // sidecar's own fault apart from the site's. Rejecting here
                // routes it to `last_err` instead, which is what the retry needs.
                // The arms then split the verdict: `is_cloak_content_verdict`
                // (renderer `lib.rs`) matches this message and books it
                // host-only, so a body the sidecar returned and we rejected is
                // the page's verdict, not a tier fault; only a transport
                // failure (unreachable sidecar or proxy) reaches the global
                // cloak breaker.
                //
                // The same floor the caller applies, so nothing that would have
                // been accepted downstream is rejected here.
                Ok((status, body))
                    if !crate::detector::looks_like_cloudflare_challenge(&body)
                        && crate::html_body_text_len(&body)
                            >= crate::FallbackRenderer::MIN_RENDERED_TEXT_LEN =>
                {
                    // Success: remember the winning sessid so future requests to
                    // this host reuse the working exit IP (warm the cache).
                    {
                        let mut map = self.sessid_map.lock().expect("cloak sessid map poisoned");
                        map.retain(|_, (_, at)| at.elapsed() < SESSID_TTL);
                        map.insert(host.clone(), (sessid, Instant::now()));
                    }
                    return Ok(FetchResult {
                        url: url.to_string(),
                        final_url: None,
                        status_code: status,
                        html: body,
                        content_type: None,
                        raw_bytes: None,
                        rendered_with: Some(self.name.clone()),
                        elapsed_ms: start.elapsed().as_millis() as u64,
                        warning: None,
                        render_decision: None,
                        credit_cost: 0,
                        warnings: Vec::new(),
                        truncated: false,
                        deadline_exceeded: deadline.remaining().is_zero(),
                        captured_responses: Vec::new(),
                        screenshot: None,
                        block: None,
                    });
                }
                Ok((status, body)) => {
                    let reason = if crate::html_body_text_len(&body)
                        < crate::FallbackRenderer::MIN_RENDERED_TEXT_LEN
                    {
                        "empty or unusably thin body (sidecar fail-open)"
                    } else {
                        "still challenged"
                    };
                    last_err = Some(CrwError::RendererError(format!(
                        "cloak: {reason} (HTTP {status}) for {host}"
                    )));
                }
                Err(e) => last_err = Some(e),
            }
        }
        // Fallback: if the loop broke before any attempt (deadline pressure),
        // surface a Timeout so the breaker classifies it as deadline-clamped
        // rather than a render failure — mirrors the ladder's budget-drained idiom.
        Err(last_err.unwrap_or_else(|| CrwError::Timeout(deadline.requested_ms())))
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn supports_js(&self) -> bool {
        true
    }

    /// Health probe against `GET {base}/cache/stats` (the sidecar has no
    /// `/health` route — its catch-all mirror would 400 without `x-hostname`).
    async fn is_available(&self) -> bool {
        let url = format!("{}/cache/stats", self.base_url);
        let fut = self.auth(self.client.get(&url)).send();
        matches!(
            tokio::time::timeout(PROBE_TIMEOUT, fut).await,
            Ok(Ok(r)) if r.status().is_success()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // A body carrying a Cloudflare strong marker → looks_like_cloudflare_challenge==true.
    const CHALLENGE_BODY: &str = "<html><head><title>Just a moment...</title></head><body>\
        <script src=\"/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1\"></script></body></html>";
    const REAL_BODY: &str = "<html><body><h1>Northwestern Mutual Reviews</h1>\
        <p>real review content that is definitely not a challenge page</p></body></html>";

    fn renderer(base_url: &str) -> CloakRenderer {
        // No proxy_base → no x-proxy header (sidecar egresses direct in tests).
        CloakRenderer::new("cloak", base_url, "", 30_000, None, None, None)
    }

    // With no REQUEST_PROXY scoped, sticky_proxy_url falls back to the configured
    // cloak proxy host + proxy_base creds + default country. (No task-local scope
    // here → try_with is Err → .ok() None → the or_else fallback fires.)
    #[tokio::test]
    async fn self_provisions_proxy_from_host_when_request_proxy_absent() {
        let r = CloakRenderer::new(
            "cloak",
            "http://sidecar:8000",
            "",
            30_000,
            Some(("user".to_string(), "pass".to_string())),
            Some("us".to_string()),
            Some("http://gw.dataimpulse.com:823".to_string()),
        );
        assert_eq!(
            r.sticky_proxy_url("abc").unwrap(),
            "http://user__cr.us;sessid.stickabc:pass@gw.dataimpulse.com:823"
        );
    }

    // Unset proxy host + no REQUEST_PROXY → None (byte-identical to pre-change).
    #[tokio::test]
    async fn no_host_and_no_request_proxy_yields_none() {
        let r = CloakRenderer::new(
            "cloak",
            "http://sidecar:8000",
            "",
            30_000,
            Some(("user".to_string(), "pass".to_string())),
            Some("us".to_string()),
            None,
        );
        assert!(r.sticky_proxy_url("abc").is_none());
    }

    // Empty-string host is normalized to None at construction (no accidental
    // "://:@..." when the env is set-but-empty).
    #[tokio::test]
    async fn empty_host_normalizes_to_none() {
        let r = CloakRenderer::new(
            "cloak",
            "http://sidecar:8000",
            "",
            30_000,
            Some(("user".to_string(), "pass".to_string())),
            Some("us".to_string()),
            Some("   ".to_string()),
        );
        assert!(r.sticky_proxy_url("abc").is_none());
    }

    #[tokio::test]
    async fn happy_path_returns_upstream_html() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(ResponseTemplate::new(200).set_body_string(REAL_BODY))
            .mount(&server)
            .await;
        let r = renderer(&server.uri())
            .fetch(
                &format!("{}/page", server.uri()),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(30_000),
            )
            .await
            .expect("cloak fetch should succeed");
        assert_eq!(r.status_code, 200);
        assert!(r.html.contains("Northwestern Mutual"));
        assert_eq!(r.rendered_with.as_deref(), Some("cloak"));
    }

    #[tokio::test]
    async fn persistent_challenge_body_is_retryable_error() {
        let server = MockServer::start().await;
        // Every attempt gets a 200 challenge shell → the accept-gate rejects it.
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(ResponseTemplate::new(200).set_body_string(CHALLENGE_BODY))
            .expect(2) // both attempts fire
            .mount(&server)
            .await;
        let err = renderer(&server.uri())
            .fetch(
                &format!("{}/page", server.uri()),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(30_000),
            )
            .await
            .expect_err("a persistent challenge must surface as an error, never fake success");
        assert!(matches!(err, CrwError::RendererError(_)));
    }

    #[tokio::test]
    async fn empty_fail_open_body_is_an_error_not_a_thin_ok() {
        let server = MockServer::start().await;
        // The sidecar fails open with an EMPTY 200 on load-shed, on timeout and
        // on any internal error of its own. That body is not a CF challenge, so
        // it used to be accepted here: the retry loop returned on the first
        // pass, the sessid was banked as proven-good for 29 minutes, and the
        // caller scored it `r_ok = false` and booked a RenderError, which tripped
        // the GLOBAL cloak breaker and disabled the tier for every host because
        // of the sidecar's own fault.
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .expect(2) // rejecting it is what revives the second attempt
            .mount(&server)
            .await;
        let err = renderer(&server.uri())
            .fetch(
                &format!("{}/page", server.uri()),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(30_000),
            )
            .await
            .expect_err("an empty fail-open body must never be returned as a page");
        match err {
            CrwError::RendererError(m) => assert!(
                m.contains("fail-open"),
                "the error must name the sidecar's fail-open so an operator can \
                 tell it apart from a site that is still challenged; got {m}"
            ),
            other => panic!("expected RendererError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recovers_on_fresh_sessid_retry_with_bypass_cache() {
        let server = MockServer::start().await;
        // Attempt 1 carries x-bypass-cache → real body (higher priority wins).
        Mock::given(method("GET"))
            .and(path("/page"))
            .and(header("x-bypass-cache", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_string(REAL_BODY))
            .with_priority(1)
            .mount(&server)
            .await;
        // Attempt 0 (no bypass header) → challenge.
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(ResponseTemplate::new(200).set_body_string(CHALLENGE_BODY))
            .with_priority(5)
            .mount(&server)
            .await;
        let r = renderer(&server.uri())
            .fetch(
                &format!("{}/page", server.uri()),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(30_000),
            )
            .await
            .expect("second attempt (fresh sessid + bypass) should recover");
        assert!(r.html.contains("real review content"));
    }

    #[tokio::test]
    async fn cold_host_uses_fresh_bypass_on_first_attempt() {
        // A cold host (no warm sessid) goes straight to the fresh-IP + bypass
        // strategy on attempt 0 — the pinned deterministic-sessid path is gone,
        // so the non-bypass challenge branch must never be hit.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .and(header("x-bypass-cache", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_string(REAL_BODY))
            .with_priority(1)
            .mount(&server)
            .await;
        // The old pinned (no-bypass) attempt-0 path must NOT be taken: assert it
        // is matched zero times (verified on server drop).
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(ResponseTemplate::new(200).set_body_string(CHALLENGE_BODY))
            .with_priority(5)
            .expect(0)
            .mount(&server)
            .await;
        let r = renderer(&server.uri())
            .fetch(
                &format!("{}/page", server.uri()),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(30_000),
            )
            .await
            .expect("cold host must recover via fresh+bypass on attempt 0");
        assert!(r.html.contains("real review content"));
    }

    #[tokio::test]
    async fn non_2xx_real_content_is_accepted_not_rejected() {
        // After the sidecar clears CF, a legit upstream 403 (paywall/geo) with
        // usable content must be RETURNED (status preserved), not discarded — the
        // caller's r_ok judges content, not status.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(ResponseTemplate::new(403).set_body_string(REAL_BODY))
            .expect(1) // accepted on attempt 0, no retry
            .mount(&server)
            .await;
        let r = renderer(&server.uri())
            .fetch(
                &format!("{}/page", server.uri()),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(30_000),
            )
            .await
            .expect("non-2xx real content must be accepted");
        assert_eq!(r.status_code, 403);
        assert!(r.html.contains("real review content"));
    }

    #[tokio::test]
    async fn expired_deadline_short_circuits_without_http() {
        // No mock mounted: if it made an HTTP call it would error differently.
        let r = renderer("http://127.0.0.1:1") // unroutable
            .fetch(
                "http://example.com/page",
                &HashMap::new(),
                None,
                Deadline::from_request_ms(0),
            )
            .await;
        assert!(matches!(r, Err(CrwError::Timeout(_))));
    }

    #[tokio::test]
    async fn is_available_probes_cache_stats() {
        let up = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cache/stats"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "cached_entries": 0
            })))
            .mount(&up)
            .await;
        assert!(renderer(&up.uri()).is_available().await);

        let down = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cache/stats"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&down)
            .await;
        assert!(!renderer(&down.uri()).is_available().await);
    }
}
