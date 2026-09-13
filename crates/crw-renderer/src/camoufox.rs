//! Camoufox stealth renderer tier (opt-in, REST — not CDP).
//!
//! Backed by a [`camofox-browser`](https://github.com/jo-inc/camofox-browser)
//! REST sidecar (default port `9377`) which drives a Camoufox (Firefox-fork)
//! browser with C++-level fingerprint spoofing. This tier covers
//! fingerprint/bot-challenge blocks (e.g. Cloudflare 403) that the CDP tiers
//! cannot pass.
//!
//! ## Why REST, not CDP
//! Camoufox does not expose a usable Chromium-CDP `Fetch.*` surface, so it is
//! reached over the sidecar's HTTP API instead of the [`crate::cdp`] path. The
//! renderer is a plain [`reqwest::Client`] — it never touches `cdp.rs`.
//!
//! ## Sequence (per request, fresh session — no cookie carry-over)
//! 1. `POST {base}/tabs` `{userId, sessionKey, url}` → `{tabId, url}` (the
//!    `url` body navigates the new tab as it is created).
//! 2. `POST {base}/tabs/{tabId}/evaluate` `{userId, expression}` with
//!    `expression = "document.documentElement.outerHTML"` → `{ok, result}`.
//!    `result` is the fully JS-rendered DOM string — exactly what CRW's
//!    post-render pipeline (`only_main_content`, tag filters, markdown) needs.
//! 3. `DELETE {base}/sessions/{userId}` — ALWAYS, even on error, so the sidecar
//!    never leaks a server-side session.
//!
//! The endpoint contract above is from the camofox-browser `openapi.json`.

use async_trait::async_trait;
use crw_core::Deadline;
use crw_core::config::CAMOUFOX_DEFAULT_CHALLENGE_WAIT_MS;
use crw_core::error::{CrwError, CrwResult};
use crw_core::types::FetchResult;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::traits::PageFetcher;

/// JS expression evaluated to retrieve the post-render DOM.
const OUTER_HTML_EXPR: &str = "document.documentElement.outerHTML";
/// Fixed budget for the best-effort session cleanup. Deliberately NOT derived
/// from the request deadline: a hung cleanup must never consume the caller's
/// remaining budget.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
/// Budget for the `is_available` health probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Delay between polls while a bot challenge is clearing. Same cadence as the
/// CDP tiers' `cdp::CHALLENGE_POLL_INTERVAL_MS`; kept as a separate constant
/// because the `camoufox` feature does not imply `cdp`, so that module is not
/// compiled in a camoufox-only build.
const CHALLENGE_POLL_INTERVAL: Duration = Duration::from_secs(3);
/// Smallest request-deadline remainder worth spending on one more poll
/// evaluate. Below it the call can only time out, which would replace the
/// informative wall error with a bare "timed out".
const MIN_POLL_EVALUATE_BUDGET: Duration = Duration::from_millis(250);
/// What the poll can still spend after its ceiling before the caller sees the
/// result: one last evaluate at its floor plus the awaited session teardown.
/// A reserve handed to [`CamoufoxRenderer::with_challenge_reserve`] on behalf
/// of something that runs after this tier has to include it.
#[cfg(feature = "cloak")]
pub(crate) const POLL_HANDOFF_MS: u64 =
    MIN_POLL_EVALUATE_BUDGET.as_millis() as u64 + CLEANUP_TIMEOUT.as_millis() as u64;

/// Opt-in Camoufox stealth renderer. Construct via [`CamoufoxRenderer::new`].
pub struct CamoufoxRenderer {
    name: String,
    /// Trimmed base URL, no trailing slash (e.g. `http://localhost:9377`).
    base_url: String,
    /// Bearer token; empty string means no `Authorization` header is sent.
    api_key: String,
    /// Overall per-request REST budget (`config.camoufox_timeout()`).
    timeout: Duration,
    /// Ceiling for polling a tab while a bot challenge clears
    /// (`config.camoufox_challenge_wait()`). `ZERO` disables the poll.
    challenge_wait: Duration,
    /// Request-deadline time the poll must leave untouched, so a recovery arm
    /// that runs after the ladder (the cloak arm, gated on
    /// `CLOAK_ARM_FLOOR_MS`) still finds its floor. Zero when nothing runs
    /// after this tier.
    challenge_reserve: Duration,
    client: reqwest::Client,
}

impl CamoufoxRenderer {
    pub fn new(name: &str, base_url: &str, api_key: &str, timeout_ms: u64) -> Self {
        Self {
            name: name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            timeout: Duration::from_millis(timeout_ms),
            challenge_wait: Duration::from_millis(CAMOUFOX_DEFAULT_CHALLENGE_WAIT_MS),
            challenge_reserve: Duration::ZERO,
            client: reqwest::Client::new(),
        }
    }

    /// Override the bot-challenge poll ceiling. Mirrors
    /// `CdpRenderer::with_challenge_retries`, so the tier is configured the
    /// same way the CDP tiers are. `0` disables the poll.
    pub fn with_challenge_wait(mut self, challenge_wait_ms: u64) -> Self {
        self.challenge_wait = Duration::from_millis(challenge_wait_ms);
        self
    }

    /// Keep `reserve_ms` of the request deadline out of the challenge poll.
    /// The ladder sets this when a recovery arm runs after camoufox, so the
    /// poll cannot spend the floor that arm needs to fire.
    pub fn with_challenge_reserve(mut self, reserve_ms: u64) -> Self {
        self.challenge_reserve = Duration::from_millis(reserve_ms);
        self
    }

    /// Attach the bearer header when an API key is configured.
    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.api_key.is_empty() {
            rb
        } else {
            rb.bearer_auth(&self.api_key)
        }
    }

    /// Random opaque identifier (`<prefix>` + 32 hex chars). Fresh per request
    /// so sessions never share cookies — safer for stealth.
    fn rand_id(prefix: &str) -> String {
        let n: u128 = rand::random();
        format!("{prefix}{n:032x}")
    }

    /// Remaining budget for the next REST call: the smaller of the request
    /// deadline's remaining time and the configured camoufox timeout. This is
    /// the single source of the clamp formula — every REST call uses it.
    fn call_budget(&self, deadline: &Deadline) -> Duration {
        deadline.remaining().min(self.timeout)
    }

    /// Send a JSON POST, honoring the deadline, and parse a JSON response.
    /// Maps transport/timeout/non-2xx into the appropriate [`CrwError`].
    async fn post_json(
        &self,
        path: &str,
        body: &serde_json::Value,
        deadline: &Deadline,
    ) -> CrwResult<serde_json::Value> {
        let budget = self.call_budget(deadline);
        if budget.is_zero() {
            return Err(CrwError::Timeout(deadline.requested_ms()));
        }
        let url = format!("{}{}", self.base_url, path);
        let fut = self.auth(self.client.post(&url).json(body)).send();
        let resp = tokio::time::timeout(budget, fut)
            .await
            .map_err(|_| CrwError::Timeout(budget.as_millis() as u64))?
            .map_err(|e| {
                // The sidecar base URL is internal infrastructure: operators get it
                // from the log, callers only get the failure shape.
                tracing::warn!(base_url = %self.base_url, path, "camoufox request failed: {e}");
                CrwError::RendererError(format!(
                    "camoufox POST {path}: {}",
                    crw_core::error::reqwest_message(e)
                ))
            })?;
        let status = resp.status();
        let value: serde_json::Value = resp.json().await.map_err(|e| {
            CrwError::RendererError(format!(
                "camoufox POST {path} body: {}",
                crw_core::error::reqwest_message(e)
            ))
        })?;
        if !status.is_success() {
            let msg = value
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown error");
            return Err(CrwError::RendererError(format!(
                "camoufox POST {path} -> HTTP {}: {msg}",
                status.as_u16()
            )));
        }
        Ok(value)
    }

    /// Create a fresh tab navigated to `url`. Returns its `tabId`. Some sidecar
    /// builds intermittently omit `tabId` under proxy-IP races on the first
    /// attempt, so we retry exactly once before failing.
    async fn create_tab(
        &self,
        url: &str,
        user_id: &str,
        session_key: &str,
        deadline: &Deadline,
    ) -> CrwResult<String> {
        let body = serde_json::json!({
            "userId": user_id,
            "sessionKey": session_key,
            "url": url,
        });
        for attempt in 0..2u8 {
            let value = self.post_json("/tabs", &body, deadline).await?;
            if let Some(tab_id) = value.get("tabId").and_then(|t| t.as_str()) {
                return Ok(tab_id.to_string());
            }
            tracing::debug!(
                renderer = %self.name,
                attempt,
                "camoufox create tab returned no tabId, retrying"
            );
        }
        Err(CrwError::RendererError(
            "camoufox: create tab returned no tabId after retry".into(),
        ))
    }

    /// Evaluate `document.documentElement.outerHTML` and return the DOM string.
    async fn evaluate_outer_html(
        &self,
        tab_id: &str,
        user_id: &str,
        deadline: &Deadline,
    ) -> CrwResult<String> {
        let body = serde_json::json!({
            "userId": user_id,
            "expression": OUTER_HTML_EXPR,
        });
        let value = self
            .post_json(&format!("/tabs/{tab_id}/evaluate"), &body, deadline)
            .await?;
        // Shape: { ok: bool, result: <any> }. For outerHTML, result is a string.
        if value.get("ok").and_then(|o| o.as_bool()) == Some(false) {
            return Err(CrwError::RendererError(
                "camoufox: evaluate returned ok=false".into(),
            ));
        }
        match value.get("result").and_then(|r| r.as_str()) {
            Some(html) => Ok(html.to_string()),
            None => Err(CrwError::RendererError(
                "camoufox: evaluate result was not an HTML string".into(),
            )),
        }
    }

    /// Best-effort session teardown. Uses [`CLEANUP_TIMEOUT`] (NOT the request
    /// deadline) and swallows every error — a failed cleanup is logged, never
    /// propagated.
    async fn destroy_session(&self, user_id: &str) {
        let url = format!("{}/sessions/{}", self.base_url, user_id);
        let fut = self.auth(self.client.delete(&url)).send();
        match tokio::time::timeout(CLEANUP_TIMEOUT, fut).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(
                renderer = %self.name, user_id, "camoufox session cleanup failed: {e}"
            ),
            Err(_) => tracing::warn!(
                renderer = %self.name,
                user_id,
                "camoufox session cleanup timed out after {}s (leaked server-side session)",
                CLEANUP_TIMEOUT.as_secs()
            ),
        }
    }

    /// Full create → evaluate sequence with GUARANTEED cleanup. The inner
    /// result is bound first, then `destroy_session` runs at statement level so
    /// it fires on both the Ok and Err paths, and only afterward is the result
    /// returned.
    async fn run_sequence(
        &self,
        url: &str,
        user_id: &str,
        session_key: &str,
        deadline: &Deadline,
    ) -> CrwResult<(u16, String)> {
        let inner = self
            .run_sequence_inner(url, user_id, session_key, deadline)
            .await;
        self.destroy_session(user_id).await;
        inner
    }

    async fn run_sequence_inner(
        &self,
        url: &str,
        user_id: &str,
        session_key: &str,
        deadline: &Deadline,
    ) -> CrwResult<(u16, String)> {
        let tab_id = self.create_tab(url, user_id, session_key, deadline).await?;
        let mut html = self.evaluate_outer_html(&tab_id, user_id, deadline).await?;

        // A Cloudflare-style JS challenge resolves client-side, empirically
        // 5-25s after navigation. A single immediate evaluate() can therefore
        // only ever observe the interstitial, and this tier reported a wall for
        // pages it would have gotten had it looked again. Poll until the
        // challenge clears or the budget runs out.
        //
        // Only `"challenge"` is polled, never `"wall"`. A wall is a terminal
        // refusal rather than work in progress: waiting on it would spend the
        // whole budget to arrive at the same error.
        //
        // The ceiling is clamped by the deadline's own remaining time, minus
        // whatever a recovery arm after this tier needs, so a challenge that
        // never clears cannot outlive the request or starve that arm. A clean
        // page never enters the loop: `looks_like_wall` is `None` on the first
        // evaluate, and a deployment that sets the ceiling to 0 keeps exactly
        // today's single-shot behaviour.
        // The reserve only applies while there is more deadline left than the
        // reserve itself. Below that the poll is deliberately preferred over
        // the arm it would protect: holding the whole remainder back would
        // leave this tier a single look, and the arm may not be reachable at
        // all on this request.
        let remaining_now = deadline.remaining();
        let remaining_for_poll = if remaining_now > self.challenge_reserve {
            remaining_now - self.challenge_reserve
        } else {
            remaining_now
        };
        let challenge_budget = self.challenge_wait.min(remaining_for_poll);
        let poll_start = Instant::now();
        let mut missed_poll = false;
        while looks_like_wall(&html) == Some("challenge") {
            let remaining = challenge_budget.saturating_sub(poll_start.elapsed());
            if remaining.is_zero() {
                break;
            }
            tokio::time::sleep(CHALLENGE_POLL_INTERVAL.min(remaining)).await;
            // The sleep can consume what was left of the shared deadline. Stop
            // here rather than dispatching an evaluate that cannot finish: that
            // call would return `Timeout`, replacing this tier's informative
            // wall error (and the antibot attribution that rides on it) with a
            // bare "timed out".
            if deadline.remaining() < MIN_POLL_EVALUATE_BUDGET {
                break;
            }
            // The challenge clears by reloading the tab, and an evaluate that
            // lands in that window fails ("execution context destroyed",
            // surfaced as ok:false or a non-2xx). One such miss is part of the
            // poll, not a failure of the tier; two in a row means the sidecar
            // itself is broken and the error is the honest answer. The flag
            // resets on every good evaluate, so it tolerates one miss per
            // streak, bounded by the ceiling. A `Timeout` counts as a miss too:
            // the loop then ends on the budget and reports the challenge it
            // saw rather than a bare "timed out".
            // Bound the evaluate by what is left of the ceiling (with a small
            // floor so the final look can still complete), not by the whole
            // request deadline: otherwise one slow evaluate could spend the
            // reserve the ceiling was clamped to keep.
            let poll_left = challenge_budget.saturating_sub(poll_start.elapsed());
            let evaluate_deadline = Deadline::now_plus(
                poll_left
                    .max(MIN_POLL_EVALUATE_BUDGET)
                    .min(deadline.remaining()),
            );
            match self
                .evaluate_outer_html(&tab_id, user_id, &evaluate_deadline)
                .await
            {
                Ok(next) => {
                    html = next;
                    missed_poll = false;
                }
                Err(e) if !missed_poll => {
                    tracing::debug!(
                        renderer = %self.name,
                        "camoufox: challenge poll evaluate failed, retrying once: {e}"
                    );
                    missed_poll = true;
                }
                Err(e) => return Err(e),
            }
        }

        // A bot wall or an empty body is a failure for THIS tier — surface it as
        // a retryable RendererError so the fallback loop / breaker can react.
        if html.trim().is_empty() {
            return Err(CrwError::RendererError(
                "camoufox: evaluate returned empty HTML".into(),
            ));
        }
        if let Some(kind) = looks_like_wall(&html) {
            return Err(CrwError::RendererError(format!(
                "camoufox: bot {kind} detected in rendered HTML"
            )));
        }
        // The /evaluate path does not surface the page's HTTP status; the
        // navigation in `create_tab` already succeeded server-side, so 200.
        Ok((200, html))
    }
}

/// Heuristic detection of a bot wall / challenge interstitial in rendered HTML.
/// Returns the wall kind when matched. These markers are CRW-specific (the
/// stock camofox-browser client does not classify walls); they let the tier
/// report a retryable failure instead of returning a useless challenge page.
fn looks_like_wall(html: &str) -> Option<&'static str> {
    let h = html.to_ascii_lowercase();
    // The one terminal refusal is checked FIRST: the "Attention Required"
    // title fronts the 1020 access-denied, the 1009 country ban and the
    // interactive captcha, none of which clears by waiting, and it can share a
    // page with a challenge marker (the legacy interstitial paired it with
    // "checking your browser"). Matching in list order would poll such a page
    // for the whole ceiling to arrive at the same refusal. The CDP tier's
    // `is_challenge_page` does poll this title; the divergence is deliberate,
    // since this tier is normally the last one and its poll is far longer.
    if h.contains("attention required! | cloudflare") {
        return Some("wall");
    }
    // `challenge-platform/h/`, not the bare `/cdn-cgi/challenge-platform`
    // directory. Both live under it and they mean opposite things: the
    // orchestrator is always served from `/h/`, while `scripts/jsd/main.js` is
    // the ordinary Bot-Management loader that Cloudflare re-injects into pages
    // that have ALREADY cleared. Matching the bare directory therefore fires on
    // a solved page. `crw_crawl::single::classify_block` and
    // `detector::looks_like_cloudflare_challenge` both dropped it for exactly
    // that reason, each after a live capture; this list had kept it.
    const CHALLENGES: &[&str] = &[
        "just a moment",
        "verifying you are human",
        "checking your browser before",
        "cf-challenge",
        "challenge-platform/h/",
    ];
    if CHALLENGES.iter().any(|needle| h.contains(needle)) {
        return Some("challenge");
    }
    // The noscript line ships ON the managed-challenge interstitial itself
    // (`crates/crw-renderer/tests/egress_latch_no_latch_on_cf.rs` carries the
    // canonical capture), so it must never outrank a challenge marker. Alone,
    // with no challenge around it, it is a plain refusal, unless it only sits
    // inside `<noscript>`: a rendered page keeps that hidden fallback text in
    // its outerHTML, and a page that rendered is not a refusal.
    if without_noscript(&h).contains("enable javascript and cookies to continue") {
        return Some("wall");
    }
    None
}

/// The document with every `<noscript>...</noscript>` block removed. Input is
/// already lowercased. An unterminated block is dropped to the end.
fn without_noscript(h: &str) -> std::borrow::Cow<'_, str> {
    if !h.contains("<noscript") {
        return std::borrow::Cow::Borrowed(h);
    }
    let mut out = String::with_capacity(h.len());
    let mut rest = h;
    while let Some(start) = rest.find("<noscript") {
        out.push_str(&rest[..start]);
        rest = match rest[start..].find("</noscript>") {
            Some(end) => &rest[start + end + "</noscript>".len()..],
            None => "",
        };
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

#[async_trait]
impl PageFetcher for CamoufoxRenderer {
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
        let user_id = Self::rand_id("crw_");
        let session_key = Self::rand_id("task_");
        let (status, html) = self
            .run_sequence(url, &user_id, &session_key, &deadline)
            .await?;

        Ok(FetchResult {
            url: url.to_string(),
            final_url: None,
            status_code: status,
            html,
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
            // Camoufox is an HTTP sidecar (no CDP) — it never captures.
            screenshot: None,
            block: None,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn supports_js(&self) -> bool {
        true
    }

    /// Health probe against `GET {base}/health`. Mirrors the camofox-browser
    /// client's availability check. Cheap (~5s cap) so the FallbackRenderer can
    /// skip a down sidecar instead of paying the full per-request budget.
    async fn is_available(&self) -> bool {
        let url = format!("{}/health", self.base_url);
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
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn renderer(base_url: &str) -> CamoufoxRenderer {
        CamoufoxRenderer::new("camoufox", base_url, "", 30_000)
    }

    fn deadline() -> Deadline {
        Deadline::from_request_ms(30_000)
    }

    /// A spent deadline must report the budget the caller was given, not how
    /// many milliseconds late we noticed. Prod told customers holding a 30s
    /// budget that their scrape "timed out after 1ms" 326 times in ten days.
    /// This exercises the real call site: `fetch` checks `expired()` before it
    /// touches the network, so no server is needed.
    #[tokio::test]
    async fn expired_deadline_reports_the_requested_budget() {
        let r = renderer("http://127.0.0.1:1");
        let spent = Deadline::from_request_ms(0);
        assert!(spent.expired());

        let err = r
            .fetch("https://example.com", &HashMap::new(), None, spent)
            .await
            .expect_err("an expired deadline must not attempt a fetch");

        match err {
            CrwError::Timeout(ms) => assert_eq!(
                ms, 0,
                "must echo the caller's budget, not the overrun since it lapsed"
            ),
            other => panic!("expected Timeout, got {other:?}"),
        }

        // And the message a customer actually reads.
        let long = Deadline::from_request_ms(30_000);
        std::thread::sleep(std::time::Duration::from_millis(1));
        let budget = long.requested_ms();
        assert_eq!(
            CrwError::Timeout(budget).to_string(),
            "Timeout after 30000ms"
        );
    }

    async fn mount_delete_session(server: &MockServer) {
        Mock::given(method("DELETE"))
            .and(path_regex(r"^/sessions/.+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true, "closed": 1
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn happy_path_returns_html_and_cleans_up() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tabs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tabId": "t1", "url": "https://example.com"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": "<html><body>real content well over the empty threshold here</body></html>"
            })))
            .mount(&server)
            .await;
        // Expect cleanup to fire exactly once.
        Mock::given(method("DELETE"))
            .and(path_regex(r"^/sessions/.+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;

        let r = renderer(&server.uri());
        let res = r
            .fetch("https://example.com", &HashMap::new(), None, deadline())
            .await
            .expect("fetch should succeed");
        assert_eq!(res.rendered_with.as_deref(), Some("camoufox"));
        assert_eq!(res.status_code, 200);
        assert!(res.html.contains("real content"));
        // server drop verifies the expect(1) on the DELETE mock
    }

    #[tokio::test]
    async fn cleanup_fires_on_evaluate_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tabs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"tabId": "t1"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(
                ResponseTemplate::new(500).set_body_json(serde_json::json!({"error": "boom"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(r"^/sessions/.+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .expect(1) // cleanup MUST fire even though evaluate failed
            .mount(&server)
            .await;

        let r = renderer(&server.uri());
        let err = r
            .fetch("https://example.com", &HashMap::new(), None, deadline())
            .await
            .expect_err("evaluate 500 should error");
        assert!(matches!(err, CrwError::RendererError(_)));
    }

    /// Helper: mock create-tab OK + evaluate returning `eval_body`, with a
    /// cleanup mock that MUST fire once. Returns the started server.
    async fn server_with_evaluate(eval_body: serde_json::Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tabs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"tabId": "t1"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(eval_body))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(r"^/sessions/.+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .expect(1) // cleanup must fire on every error path below
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn empty_html_errors_and_cleans_up() {
        let server = server_with_evaluate(serde_json::json!({"ok": true, "result": "   "})).await;
        let err = renderer(&server.uri())
            .fetch("https://example.com", &HashMap::new(), None, deadline())
            .await
            .expect_err("empty HTML should error");
        match err {
            CrwError::RendererError(m) => assert!(m.contains("empty HTML")),
            other => panic!("expected RendererError, got {other:?}"),
        }
        // server drop verifies cleanup expect(1)
    }

    #[tokio::test]
    async fn evaluate_ok_false_errors_and_cleans_up() {
        let server = server_with_evaluate(serde_json::json!({"ok": false})).await;
        let err = renderer(&server.uri())
            .fetch("https://example.com", &HashMap::new(), None, deadline())
            .await
            .expect_err("ok=false should error");
        match err {
            CrwError::RendererError(m) => assert!(m.contains("ok=false")),
            other => panic!("expected RendererError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn evaluate_non_string_result_errors_and_cleans_up() {
        // result is a number, not the expected HTML string.
        let server = server_with_evaluate(serde_json::json!({"ok": true, "result": 42})).await;
        let err = renderer(&server.uri())
            .fetch("https://example.com", &HashMap::new(), None, deadline())
            .await
            .expect_err("non-string result should error");
        match err {
            CrwError::RendererError(m) => assert!(m.contains("not an HTML string")),
            other => panic!("expected RendererError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_tab_id_retries_once_then_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tabs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(2) // first attempt + one retry
            .mount(&server)
            .await;
        mount_delete_session(&server).await;

        let r = renderer(&server.uri());
        let err = r
            .fetch("https://example.com", &HashMap::new(), None, deadline())
            .await
            .expect_err("missing tabId should error");
        match err {
            CrwError::RendererError(m) => assert!(m.contains("no tabId")),
            other => panic!("expected RendererError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wall_detection_returns_retryable_renderer_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tabs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"tabId": "t1"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": "<html><head><title>Just a moment...</title></head><body>verifying you are human</body></html>"
            })))
            .mount(&server)
            .await;
        mount_delete_session(&server).await;

        // Tiny challenge ceiling: this challenge never clears, so the poll must
        // exhaust its budget. 50ms keeps that in the millisecond range instead
        // of spending the production ceiling in CI, while still running the
        // loop body at least once.
        let r = renderer(&server.uri()).with_challenge_wait(50);
        let err = r
            .fetch("https://example.com", &HashMap::new(), None, deadline())
            .await
            .expect_err("wall should error");
        match err {
            CrwError::RendererError(m) => assert!(m.contains("challenge")),
            other => panic!("expected RendererError, got {other:?}"),
        }
    }

    /// The point of the poll: a challenge that clears between evaluates now
    /// yields the real page instead of the interstitial.
    #[tokio::test]
    async fn challenge_that_clears_on_a_later_poll_is_returned_as_content() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tabs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"tabId": "t1"})),
            )
            .mount(&server)
            .await;
        // First evaluate: the interstitial. Registered first and capped at one
        // call, so the second evaluate falls through to the real page below.
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": "<html><head><title>Just a moment...</title></head></html>"
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // The cleared page, shaped like a real one: Cloudflare re-injects the
        // Bot-Management telemetry loader into pages that have already passed,
        // so a fixture without it would let a predicate that matches the bare
        // `/cdn-cgi/challenge-platform` directory pass this test while polling
        // every real managed site to the ceiling and then discarding it.
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": "<html><head><script src=\"/cdn-cgi/challenge-platform/scripts/jsd/main.js\"></script></head><body><h1>real page</h1></body></html>"
            })))
            .mount(&server)
            .await;
        mount_delete_session(&server).await;

        // 200ms ceiling: one poll, sub-second in CI. The loop sleeps
        // `CHALLENGE_POLL_INTERVAL.min(remaining)`, so the budget bounds the
        // wait rather than the production cadence.
        let r = renderer(&server.uri()).with_challenge_wait(200);
        let res = r
            .fetch("https://example.com", &HashMap::new(), None, deadline())
            .await
            .expect("challenge cleared, so the page must be returned");
        assert_eq!(res.status_code, 200);
        assert!(res.html.contains("real page"), "got: {}", res.html);
    }

    /// A challenge clears by reloading the tab, so an evaluate that lands in
    /// that window fails. One such miss must not end the tier: the next poll
    /// picks up the cleared page.
    #[tokio::test]
    async fn evaluate_failure_mid_poll_is_a_miss_not_a_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tabs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"tabId": "t1"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": "<html><head><title>Just a moment...</title></head></html>"
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Second evaluate: the context was destroyed by the reload.
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": false})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": "<html><body><h1>real page</h1></body></html>"
            })))
            .mount(&server)
            .await;
        mount_delete_session(&server).await;

        // One full poll interval for the failed look, then the remainder for
        // the look that finds the cleared page, with slack for a loaded runner.
        let r = renderer(&server.uri()).with_challenge_wait(4_500);
        let res = r
            .fetch("https://example.com", &HashMap::new(), None, deadline())
            .await
            .expect("one failed poll must not fail the tier");
        assert!(res.html.contains("real page"), "got: {}", res.html);
    }

    /// Two failed polls in a row mean the sidecar is broken, and that error is
    /// the honest answer rather than a wall the page never showed.
    #[tokio::test]
    async fn two_consecutive_evaluate_failures_surface_the_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tabs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"tabId": "t1"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": "<html><head><title>Just a moment...</title></head></html>"
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": false})),
            )
            .mount(&server)
            .await;
        mount_delete_session(&server).await;

        let r = renderer(&server.uri()).with_challenge_wait(30_000);
        let err = r
            .fetch("https://example.com", &HashMap::new(), None, deadline())
            .await
            .expect_err("a broken sidecar is an error");
        match err {
            CrwError::RendererError(m) => assert!(m.contains("ok=false"), "got: {m}"),
            other => panic!("expected RendererError, got {other:?}"),
        }
        let evaluates = server
            .received_requests()
            .await
            .expect("recorded requests")
            .iter()
            .filter(|r| r.url.path() == "/tabs/t1/evaluate")
            .count();
        assert_eq!(
            evaluates, 3,
            "interstitial, one tolerated miss, then the error"
        );
    }

    /// The reserve keeps the poll from spending the deadline a recovery arm
    /// after this tier needs: with almost all of it reserved, the poll stops
    /// after a single look instead of running out the 30s ceiling.
    #[tokio::test]
    async fn challenge_reserve_bounds_the_poll() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tabs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"tabId": "t1"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": "<html><head><title>Just a moment...</title></head></html>"
            })))
            .mount(&server)
            .await;
        mount_delete_session(&server).await;

        // 30s deadline, 30s ceiling, 29.9s reserved: 100ms of poll budget.
        let r = renderer(&server.uri())
            .with_challenge_wait(30_000)
            .with_challenge_reserve(29_900);
        let err = r
            .fetch("https://example.com", &HashMap::new(), None, deadline())
            .await
            .expect_err("the challenge never clears");
        match err {
            CrwError::RendererError(m) => assert!(m.contains("challenge"), "got: {m}"),
            other => panic!("expected RendererError, got {other:?}"),
        }
        let evaluates = server
            .received_requests()
            .await
            .expect("recorded requests")
            .iter()
            .filter(|r| r.url.path() == "/tabs/t1/evaluate")
            .count();
        assert!(
            evaluates <= 2,
            "a 100ms budget allows at most one poll, got {evaluates}"
        );
    }

    /// `looks_like_wall` separates a clearing "challenge" from a terminal
    /// "wall". Only the former is worth polling; a wall must fail immediately
    /// rather than burn the whole ceiling to reach the same error.
    #[tokio::test]
    async fn terminal_wall_is_not_polled() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tabs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"tabId": "t1"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": "<html><head><title>Attention Required! | Cloudflare</title></head></html>"
            })))
            .mount(&server)
            .await;
        mount_delete_session(&server).await;

        // A ceiling far larger than one poll interval: if the wall were polled
        // this would take seconds.
        let r = renderer(&server.uri()).with_challenge_wait(30_000);
        let err = r
            .fetch("https://example.com", &HashMap::new(), None, deadline())
            .await
            .expect_err("a terminal wall is still an error");
        match err {
            CrwError::RendererError(m) => assert!(m.contains("wall"), "got: {m}"),
            other => panic!("expected RendererError, got {other:?}"),
        }
        // Count the evaluates rather than the elapsed time: a wall-clock assert
        // on a loaded runner is a flake, and the request count proves the same
        // thing exactly. One evaluate means the loop was never entered.
        let evaluates = server
            .received_requests()
            .await
            .expect("recorded requests")
            .iter()
            .filter(|r| r.url.path() == "/tabs/t1/evaluate")
            .count();
        assert_eq!(evaluates, 1, "a terminal wall must not be polled");
    }

    #[tokio::test]
    async fn expired_deadline_short_circuits_without_http() {
        // No mocks mounted: if any HTTP call were made it would 404 and the
        // error message would differ from a Timeout.
        let server = MockServer::start().await;
        let r = renderer(&server.uri());
        let err = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                None,
                Deadline::from_request_ms(0),
            )
            .await
            .expect_err("expired deadline should error immediately");
        assert!(matches!(err, CrwError::Timeout(_)));
    }

    #[tokio::test]
    async fn is_available_true_when_health_ok() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;
        assert!(renderer(&server.uri()).is_available().await);
    }

    #[tokio::test]
    async fn is_available_false_when_health_503() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(
                ResponseTemplate::new(503).set_body_json(serde_json::json!({"ok": false})),
            )
            .mount(&server)
            .await;
        assert!(!renderer(&server.uri()).is_available().await);
    }

    #[test]
    fn wall_needles_match_and_clean_html_passes() {
        assert_eq!(looks_like_wall("Just a moment..."), Some("challenge"));
        assert_eq!(
            looks_like_wall("<script src=/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1>"),
            Some("challenge")
        );
        // The telemetry loader ships on pages that have already cleared, so it
        // must NOT read as a challenge: matching it would poll out the whole
        // ceiling and then discard a page camoufox had successfully rendered.
        assert_eq!(
            looks_like_wall(
                "<html><head><script src=/cdn-cgi/challenge-platform/scripts/jsd/main.js></script>\
                 </head><body><article>Real content served after the challenge cleared.</article>\
                 </body></html>"
            ),
            None
        );
        // The noscript line is part of the managed-challenge interstitial, so
        // a page carrying it next to a challenge marker is work in progress,
        // not a refusal. This is the repo's canonical Cloudflare capture, kept
        // verbatim from tests/egress_latch_no_latch_on_cf.rs.
        assert_eq!(
            looks_like_wall(concat!(
                "<html><body><h1>Just a moment...</h1>",
                "<script src=\"/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1\"></script>",
                "<div id=\"cf-please-wait\">Enable JavaScript and cookies to continue</div></body></html>"
            )),
            Some("challenge")
        );
        assert_eq!(
            looks_like_wall(
                "<html><body><h1>Verifying you are human</h1>\
                 <p>Enable JavaScript and cookies to continue</p></body></html>"
            ),
            Some("challenge")
        );
        // Alone it is a plain refusal.
        assert_eq!(
            looks_like_wall("<p>Please enable JavaScript and cookies to continue</p>"),
            Some("wall")
        );
        // Only inside noscript on an otherwise rendered page: not a refusal.
        assert_eq!(
            looks_like_wall(
                "<html><body><noscript>Enable JavaScript and cookies to continue</noscript>\
                 <article>Real content that rendered fine.</article></body></html>"
            ),
            None
        );
        // The terminal title outranks a challenge marker on the same page.
        assert_eq!(
            looks_like_wall(
                "<title>Attention Required! | Cloudflare</title>\
                 <script src=/cdn-cgi/challenge-platform/h/b/orchestrate/captcha/v1></script>"
            ),
            Some("wall")
        );
        assert_eq!(
            looks_like_wall("<div class=cf-challenge>"),
            Some("challenge")
        );
        assert_eq!(
            looks_like_wall("Verifying you are human"),
            Some("challenge")
        );
        assert_eq!(
            looks_like_wall("Checking your browser before access"),
            Some("challenge")
        );
        assert_eq!(
            looks_like_wall("<h1>Attention Required! | Cloudflare</h1>"),
            Some("wall")
        );
        assert_eq!(
            looks_like_wall("Please enable JavaScript and cookies to continue"),
            Some("wall")
        );
        assert_eq!(
            looks_like_wall("<html><body>normal page</body></html>"),
            None
        );
    }

    #[tokio::test]
    async fn cleanup_uses_fixed_timeout_not_request_deadline() {
        // A near-exhausted request deadline must NOT prevent cleanup from
        // running: cleanup has its own fixed budget. We give a tiny deadline so
        // the fetch fails fast (budget exhausted mid-sequence), then assert the
        // DELETE cleanup still fired exactly once.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tabs"))
            .respond_with(
                // Large delay (2s) vs the 5ms deadline gives a ~400x margin, so
                // the create-tab call deterministically times out before its
                // response arrives even under heavy CI load — no flakiness.
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(2))
                    .set_body_json(serde_json::json!({"tabId": "t1"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(r"^/sessions/.+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;

        let r = renderer(&server.uri());
        // 5ms deadline << 2s create-tab delay -> create_tab times out, but
        // cleanup (fixed 5s budget) must still fire.
        let err = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5),
            )
            .await
            .expect_err("tiny deadline should error");
        assert!(matches!(err, CrwError::Timeout(_)));
        // server drop verifies cleanup expect(1) despite the exhausted deadline
    }
}
