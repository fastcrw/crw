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
//! ## Sequence
//! 1. `POST {base}/tabs` `{userId, sessionKey, url}` → `{tabId, url, httpStatus}`
//!    (the `url` body navigates the new tab as it is created).
//! 2. `POST {base}/tabs/{tabId}/evaluate` `{userId, expression}` with
//!    `expression = "document.documentElement.outerHTML"` → `{ok, result}`.
//!    `result` is the fully JS-rendered DOM string — exactly what CRW's
//!    post-render pipeline (`only_main_content`, tag filters, markdown) needs.
//! 3. Cleanup, ALWAYS, even on error (see below).
//!
//! The endpoint contract above is from the camofox-browser `openapi.json`.
//!
//! ## One session per host
//! A sidecar session is one browser context, so it holds the cookies a solved
//! Cloudflare challenge leaves behind (`cf_clearance`). Requests to the same
//! host share a session (`userId` = `crw_` + a sha256 prefix of the
//! [`ClearanceCache::key`] scope), so the next page of that site skips the
//! challenge instead of solving it again. The key is hashed rather than
//! embedded because it can carry proxy credentials, and the `userId` ends up in
//! logs and in the sidecar's profile directory. Its proxy part only partitions
//! sessions: the sidecar's own proxy setting decides the egress.
//!
//! Each request has its own `sessionKey`, i.e. its own tab group, and cleanup
//! closes only that group (`DELETE /tabs/group/{sessionKey}`) while other
//! requests to the host are still in flight. The last request out deletes the
//! session (`DELETE /sessions/{userId}`, as before) unless the host needed a
//! challenge solve, in which case the session is left for the next page. The
//! sidecar closes an empty session within a minute; its persistence plugin
//! (on by default) saves the cookies on close and restores them for the same
//! `userId`, so clearance outlives the live context.
//!
//! A request runs on a one-off session, the old per-request behaviour, when
//! the host already has [`SHARED_SESSION_MAX_IN_FLIGHT`] requests in flight,
//! while the host's session is being torn down, or when the URL has no host.
//! A bot wall moves the host to a fresh `userId`, so a flagged cookie cannot
//! stick to it through the persisted profile.
//!
//! The first request to a host that is not known to be clear holds
//! [`clearance_cache`]'s single-flight lock, so a burst of pages waits for one
//! challenge solve instead of all hitting the challenge at once.

use async_trait::async_trait;
use crw_core::Deadline;
use crw_core::config::CAMOUFOX_DEFAULT_CHALLENGE_WAIT_MS;
use crw_core::error::{CrwError, CrwResult};
use crw_core::types::FetchResult;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::clearance::{ClearanceCache, clearance_cache};
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
/// Request-deadline time a caller's `waitFor` leaves for reading the DOM.
const WAIT_FOR_READ_RESERVE: Duration = Duration::from_secs(2);
/// With no `waitFor`, the most camofox-browser's readiness wait (network idle,
/// then the DOM settled) may take before the tab is read.
const SETTLE_MAX: Duration = Duration::from_secs(6);
/// What the poll can still spend after its ceiling before the caller sees the
/// result: one last evaluate at its floor plus the awaited session teardown.
/// A reserve handed to [`CamoufoxRenderer::with_challenge_reserve`] on behalf
/// of something that runs after this tier has to include it.
#[cfg(feature = "cloak")]
pub(crate) const POLL_HANDOFF_MS: u64 =
    MIN_POLL_EVALUATE_BUDGET.as_millis() as u64 + CLEANUP_TIMEOUT.as_millis() as u64;

/// Most requests that share one host session at a time. It is the sidecar's
/// default `MAX_CONCURRENT_PER_USER`: past it, the sidecar queues a session's
/// evaluate calls for up to 30s and then fails them. An operator who lowers
/// that setting gets queueing, not failures, because a request evaluates one
/// call at a time.
const SHARED_SESSION_MAX_IN_FLIGHT: u32 = 3;
/// How long a host counts as clear after a success, and how long a solved
/// challenge keeps its session alive. Cloudflare's `cf_clearance` lives about
/// 30 minutes; this is the same margin [`ClearanceCache`] uses.
const HOST_WARM_TTL: Duration = Duration::from_secs(25 * 60);
/// The user agent component of the session key. The sidecar owns its
/// fingerprint and ignores caller headers, so there is only one.
const SESSION_UA: &str = "camoufox";
/// Start of the error this tier raises for a bot wall or a challenge that did
/// not clear. Shared with [`Outcome`] so a wall is told apart from a timeout.
const WALL_ERROR_PREFIX: &str = "camoufox: bot ";
/// Allowance for a tab create, which commits the first navigation. Not a bound:
/// a create is capped only by the deadline.
const CREATE_ALLOWANCE: Duration = Duration::from_secs(5);
/// What one attempt spends before its challenge poll starts, given how long it
/// waits for the page (the caller's `waitFor`, or the settle wait): the
/// create, that wait, the read, and one poll evaluate.
fn before_poll(wait_for: Option<Duration>) -> Duration {
    CREATE_ALLOWANCE
        + wait_for.map_or(SETTLE_MAX, |w| w.max(SETTLE_MAX))
        + WAIT_FOR_READ_RESERVE
        + MIN_POLL_EVALUATE_BUDGET
}

/// How long a request may wait for another request's challenge solve. It must
/// still reach its own poll with more than the reserve left: below the reserve
/// the poll takes the whole remainder (see `run_sequence`), and with it the
/// floor a recovery arm after this tier needs.
fn lock_wait_budget(
    remaining: Duration,
    reserve: Duration,
    wait_for: Option<Duration>,
) -> Duration {
    remaining
        .saturating_sub(reserve)
        .saturating_sub(before_poll(wait_for))
}

/// What this process knows about one host's shared session.
#[derive(Debug, Default)]
struct HostSession {
    /// Requests currently running on the shared session.
    in_flight: u32,
    /// The last request out is deleting the session. Joiners go one-off until
    /// the delete returns, so it cannot close their tab.
    closing: bool,
    /// Bumped by a bot wall, so the next request gets a fresh `userId`. It is
    /// kept in memory only, like the rest of this entry: after a restart, or
    /// once the host has been idle past [`HOST_WARM_TTL`], the host starts at 0
    /// again, and a wall that is still there bumps it on the first page.
    generation: u32,
    /// Last success on this host.
    ok_at: Option<Instant>,
    /// Last success that had to get through a challenge first.
    solved_at: Option<Instant>,
    /// Last bot wall, which keeps the bumped generation around.
    walled_at: Option<Instant>,
}

impl HostSession {
    fn is_clear(&self, now: Instant) -> bool {
        fresh(self.ok_at, now)
    }

    fn worth_keeping(&self, now: Instant) -> bool {
        fresh(self.solved_at, now)
    }

    fn in_use(&self, now: Instant) -> bool {
        self.in_flight > 0
            || self.is_clear(now)
            || self.worth_keeping(now)
            || fresh(self.walled_at, now)
    }
}

fn fresh(at: Option<Instant>, now: Instant) -> bool {
    at.is_some_and(|t| now.saturating_duration_since(t) < HOST_WARM_TTL)
}

/// How a fetch ended, as far as the host's session is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Outcome {
    ok: bool,
    /// The page was reached through a challenge.
    solved: bool,
    /// The page stayed behind a bot wall or an uncleared challenge.
    wall: bool,
}

/// What the request's own cleanup has to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cleanup {
    /// Close this request's tab group; the session stays.
    Group,
    /// Delete the whole session.
    Session,
}

/// The sidecar `userId` a request runs under.
enum Lease<'a> {
    /// The host's shared session. The slot is released when this drops.
    Shared(Slot<'a>),
    /// A session of its own, deleted at the end.
    OneOff(String),
}

impl Lease<'_> {
    fn user_id(&self) -> &str {
        match self {
            Lease::Shared(slot) => &slot.user_id,
            Lease::OneOff(id) => id,
        }
    }
}

/// One request's place in a host's shared session. Dropping it, including
/// when the fetch future is cancelled, gives the place back, so a host can
/// never stay stuck at the share cap.
struct Slot<'a> {
    hosts: &'a Mutex<HashMap<String, HostSession>>,
    key: String,
    user_id: String,
    /// Set when this request is the last one out and is deleting the session.
    closing: bool,
    released: bool,
}

impl Slot<'_> {
    /// Record the outcome and decide the cleanup. A [`Cleanup::Group`] gives
    /// the place back at once; a [`Cleanup::Session`] keeps it, with the host
    /// marked closing, until the slot drops after the delete.
    fn finish(&mut self, outcome: Outcome, now: Instant) -> Cleanup {
        let mut hosts = lock(self.hosts);
        let host = hosts.entry(self.key.clone()).or_default();
        if outcome.ok {
            host.ok_at = Some(now);
            if outcome.solved {
                host.solved_at = Some(now);
            }
        } else {
            host.ok_at = None;
        }
        if outcome.wall {
            host.solved_at = None;
            host.walled_at = Some(now);
            host.generation = host.generation.wrapping_add(1);
        }
        if host.in_flight <= 1 && !host.worth_keeping(now) {
            host.closing = true;
            self.closing = true;
            return Cleanup::Session;
        }
        host.in_flight = host.in_flight.saturating_sub(1);
        self.released = true;
        Cleanup::Group
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let mut hosts = lock(self.hosts);
        if let Some(host) = hosts.get_mut(&self.key) {
            host.in_flight = host.in_flight.saturating_sub(1);
            if self.closing {
                host.closing = false;
            }
        }
    }
}

/// The host map is only ever held for a lookup, never across an `.await`, so a
/// poisoned lock can only mean a panic between two plain assignments.
fn lock(
    hosts: &Mutex<HashMap<String, HostSession>>,
) -> MutexGuard<'_, HashMap<String, HostSession>> {
    hosts.lock().unwrap_or_else(|e| e.into_inner())
}

/// The sidecar `userId` for a session key: `crw_` and the first 32 hex chars
/// of its sha256. Generation 0 hashes the key alone.
fn session_user_id(key: &str, generation: u32) -> String {
    let digest = if generation == 0 {
        Sha256::digest(key.as_bytes())
    } else {
        Sha256::digest(format!("{key}#{generation}").as_bytes())
    };
    format!("crw_{}", &hex::encode(digest)[..32])
}

/// The session key for `url`: [`ClearanceCache::key`] over its host, the
/// request's proxy (or `-`) and [`SESSION_UA`]. `None` when there is no host.
fn session_key(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let proxy = crate::REQUEST_PROXY
        .try_with(|p| p.as_ref().map(|e| e.raw().to_string()))
        .ok()
        .flatten();
    Some(ClearanceCache::key(host, proxy.as_deref(), SESSION_UA))
}

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
    /// Shared session state per host session key.
    hosts: Mutex<HashMap<String, HostSession>>,
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
            hosts: Mutex::new(HashMap::new()),
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

    /// Random opaque identifier (`<prefix>` + 32 hex chars), for a request's
    /// tab group and for a one-off session.
    fn rand_id(prefix: &str) -> String {
        let n: u128 = rand::random();
        format!("{prefix}{n:032x}")
    }

    /// Place the request on its host's shared session, or on a one-off session
    /// when the host is at the share cap, is being torn down, or `url` has no
    /// host. Prunes hosts nobody has used within [`HOST_WARM_TTL`].
    fn join(&self, url: &str, now: Instant) -> Lease<'_> {
        let Some(key) = session_key(url) else {
            return Lease::OneOff(Self::rand_id("crw_"));
        };
        let mut hosts = lock(&self.hosts);
        hosts.retain(|_, h| h.in_use(now));
        let host = hosts.entry(key.clone()).or_default();
        if host.closing || host.in_flight >= SHARED_SESSION_MAX_IN_FLIGHT {
            return Lease::OneOff(Self::rand_id("crw_"));
        }
        host.in_flight += 1;
        let user_id = session_user_id(&key, host.generation);
        Lease::Shared(Slot {
            hosts: &self.hosts,
            key,
            user_id,
            closing: false,
            released: false,
        })
    }

    fn host_is_clear(&self, key: &str) -> bool {
        lock(&self.hosts)
            .get(key)
            .is_some_and(|h| h.is_clear(Instant::now()))
    }

    /// Single flight for a host not known to be clear: the first request holds
    /// the host's solve lock across its fetch, so a burst of pages waits for
    /// one challenge solve. A waiter that finds the host clear once it gets the
    /// lock lets it go and runs in parallel. The wait leaves the challenge
    /// reserve alone, and a waiter that runs out of it goes ahead without the
    /// lock, as before, rather than failing with a timeout.
    async fn solve_turn(
        &self,
        key: &str,
        deadline: &Deadline,
        wait_for: Option<Duration>,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        if self.host_is_clear(key) {
            return None;
        }
        let room = lock_wait_budget(deadline.remaining(), self.challenge_reserve, wait_for);
        if room.is_zero() {
            return None;
        }
        let guard = tokio::time::timeout(room, clearance_cache().solve_lock(key).lock_owned())
            .await
            .ok()?;
        (!self.host_is_clear(key)).then_some(guard)
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

    /// Create a fresh tab navigated to `url`. Returns its `tabId` and the
    /// navigation's HTTP status, when the sidecar reports one. Some sidecar
    /// builds intermittently omit `tabId` under proxy-IP races on the first
    /// attempt, so we retry exactly once before failing.
    async fn create_tab(
        &self,
        url: &str,
        user_id: &str,
        session_key: &str,
        deadline: &Deadline,
    ) -> CrwResult<(String, Option<u16>)> {
        let body = serde_json::json!({
            "userId": user_id,
            "sessionKey": session_key,
            "url": url,
        });
        for attempt in 0..2u8 {
            let value = self.post_json("/tabs", &body, deadline).await?;
            if let Some(tab_id) = value.get("tabId").and_then(|t| t.as_str()) {
                let status = value
                    .get("httpStatus")
                    .and_then(|s| s.as_u64())
                    .and_then(|s| u16::try_from(s).ok());
                return Ok((tab_id.to_string(), status));
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
        self.evaluate_string(tab_id, user_id, OUTER_HTML_EXPR, deadline)
            .await
    }

    /// Evaluate a JavaScript expression whose result is a string.
    async fn evaluate_string(
        &self,
        tab_id: &str,
        user_id: &str,
        expression: &str,
        deadline: &Deadline,
    ) -> CrwResult<String> {
        let body = serde_json::json!({
            "userId": user_id,
            "expression": expression,
        });
        let value = self
            .post_json(&format!("/tabs/{tab_id}/evaluate"), &body, deadline)
            .await?;
        // Shape: { ok: bool, result: <any> }; every expression here yields a string.
        if value.get("ok").and_then(|o| o.as_bool()) == Some(false) {
            return Err(CrwError::RendererError(
                "camoufox: evaluate returned ok=false".into(),
            ));
        }
        match value.get("result").and_then(|r| r.as_str()) {
            Some(s) => Ok(s.to_string()),
            None => Err(CrwError::RendererError(
                "camoufox: evaluate result was not a string".into(),
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

    /// Best-effort close of one request's tab group, leaving the session (and
    /// its cookies) for the next request to the host. It also closes the tab
    /// of a create whose `tabId` never reached us, once that create got as far
    /// as navigating; one still building its page registers the tab later and
    /// is left to the sidecar's tab reaper. Same fixed budget and error
    /// handling as [`Self::destroy_session`].
    async fn close_group(&self, user_id: &str, session_key: &str) {
        // Both ids are a prefix and hex, so neither needs escaping.
        let url = format!(
            "{}/tabs/group/{}?userId={}",
            self.base_url, session_key, user_id
        );
        let fut = self.auth(self.client.delete(&url)).send();
        match tokio::time::timeout(CLEANUP_TIMEOUT, fut).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(
                renderer = %self.name, user_id, "camoufox tab cleanup failed: {e}"
            ),
            Err(_) => tracing::warn!(
                renderer = %self.name,
                user_id,
                "camoufox tab cleanup timed out after {}s (leaked server-side tab)",
                CLEANUP_TIMEOUT.as_secs()
            ),
        }
    }

    /// Create → evaluate. Returns the status, the DOM, where the page landed,
    /// and whether it was reached through a challenge. Cleanup is the caller's
    /// ([`PageFetcher::fetch`]), which runs it on every path.
    async fn run_sequence(
        &self,
        url: &str,
        user_id: &str,
        session_key: &str,
        wait_for: Option<Duration>,
        deadline: &Deadline,
    ) -> CrwResult<(u16, String, String, bool)> {
        let (tab_id, nav_status) = self.create_tab(url, user_id, session_key, deadline).await?;
        // The tab would otherwise be read as soon as navigation returns,
        // before most page scripts have run. The caller's `waitFor` is
        // honored as the CDP tiers honor it; without one, camofox-browser
        // waits for the page to settle. Either is capped so the read still
        // fits in the deadline.
        let room = deadline.remaining().saturating_sub(WAIT_FOR_READ_RESERVE);
        match wait_for {
            Some(wait) => tokio::time::sleep(wait.min(room)).await,
            None => {
                let settle = SETTLE_MAX.min(room);
                let body = serde_json::json!({
                    "userId": user_id,
                    "timeout": settle.as_millis() as u64,
                });
                // Best-effort: a page that never settles is read as it is.
                let _ = tokio::time::timeout(
                    settle,
                    self.post_json(&format!("/tabs/{tab_id}/wait"), &body, deadline),
                )
                .await;
            }
        }
        let mut html = self.evaluate_outer_html(&tab_id, user_id, deadline).await?;
        // A challenge served with a 404 that clears into the page is not missing.
        let challenged = looks_like_wall(&html) == Some("challenge");

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
        // A page still loading is work in progress too. On a busy machine the
        // settle above can run out while the document is still arriving, and
        // that shell (eBay's is its header over an empty #mainContent) would
        // ship as a thin but successful scrape.
        while match looks_like_wall(&html) {
            Some(kind) => kind == "challenge",
            None => crate::detector::looks_like_loading_placeholder(&html),
        } {
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
                "{WALL_ERROR_PREFIX}{kind} detected in rendered HTML"
            )));
        }
        // The sidecar reports the status of the tab's first navigation, not of
        // the page a challenge reloads into, so only a missing page (404, 410)
        // with no challenge in between is passed on. Anything else reads as 200.
        let status = nav_status
            .filter(|s| matches!(s, 404 | 410) && !challenged)
            .unwrap_or(200);
        // Where the page ended up, after any redirect, meta refresh or script
        // navigation. Read after the HTML, so a navigation in between shows up
        // here. Required: the sidecar follows redirects anywhere and this tier
        // has no other way to see where it landed (`crate::check_landing`).
        // Floored like the challenge poll: the poll may have spent the deadline,
        // and the HTML already in hand must not be lost to a zero budget.
        let href_deadline = Deadline::now_plus(deadline.remaining().max(MIN_POLL_EVALUATE_BUDGET));
        let landed = self
            .evaluate_string(&tab_id, user_id, "window.location.href", &href_deadline)
            .await?;
        // A challenge can also clear during the settle wait, before the first
        // read, and then only the navigation status shows it was there.
        let solved = challenged || nav_status.is_some_and(|s| matches!(s, 403 | 429 | 503));
        Ok((status, html, landed, solved))
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
    // Kasada's and Akamai's interstitials also run a script and reload into
    // the page when it passes.
    const CHALLENGES: &[&str] = &[
        "just a moment",
        "verifying you are human",
        "checking your browser before",
        "cf-challenge",
        "challenge-platform/h/",
        "kpsdk.start=kpsdk.now()",
        "kpsdk.scriptstart",
        "sec-if-cpt",
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
        wait_for_ms: Option<u64>,
        deadline: Deadline,
    ) -> CrwResult<FetchResult> {
        if deadline.expired() {
            return Err(CrwError::Timeout(deadline.requested_ms()));
        }
        let start = Instant::now();
        let session_key = Self::rand_id("task_");
        let wait_for = wait_for_ms.map(Duration::from_millis);
        let mut lease = self.join(url, start);
        let solver = match &lease {
            Lease::Shared(slot) => self.solve_turn(&slot.key, &deadline, wait_for).await,
            Lease::OneOff(_) => None,
        };
        let result = self
            .run_sequence(url, lease.user_id(), &session_key, wait_for, &deadline)
            .await;
        // Bound first, then cleaned up at statement level, so cleanup runs on
        // the Ok and the Err path alike before the result is returned.
        let cleanup = match &mut lease {
            Lease::Shared(slot) => slot.finish(
                Outcome {
                    ok: result.is_ok(),
                    solved: matches!(result, Ok((_, _, _, true))),
                    wall: matches!(&result, Err(CrwError::RendererError(m)) if m.starts_with(WALL_ERROR_PREFIX)),
                },
                Instant::now(),
            ),
            Lease::OneOff(_) => Cleanup::Session,
        };
        // Waiters re-check once the lock is free, and the outcome is recorded
        // by now, so they see a host that just cleared.
        drop(solver);
        match cleanup {
            Cleanup::Group => self.close_group(lease.user_id(), &session_key).await,
            Cleanup::Session => self.destroy_session(lease.user_id()).await,
        }
        drop(lease);
        let (status, html, landed, _) = result?;
        let final_url = Some(landed).filter(|h| h != url && url::Url::parse(h).is_ok());
        crate::check_landing(url, final_url.as_deref(), &deadline).await?;

        Ok(FetchResult {
            url: url.to_string(),
            final_url,
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
            budget_settled: false,
            deadline_exceeded: deadline.remaining().is_zero(),
            captured_responses: Vec::new(),
            // Camoufox is an HTTP sidecar (no CDP) — it never captures.
            screenshot: None,
            block: None,
            residential_bytes: None,
            actions: None,
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

    /// A missing page's status reaches the ladder, which takes a 404 as the
    /// answer. Any other navigation status reads as 200, since a challenge
    /// may have cleared after it.
    #[tokio::test]
    async fn missing_page_status_is_passed_on() {
        for (nav_status, want) in [(404, 404), (410, 410), (403, 200), (200, 200)] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/tabs"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tabId": "t1", "url": "https://example.com/gone", "httpStatus": nav_status
                })))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/tabs/t1/evaluate"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "ok": true,
                    "result": "<html><body><h1>Page not found</h1><p>The page you asked for is not here.</p></body></html>"
                })))
                .mount(&server)
                .await;
            mount_delete_session(&server).await;

            let res = renderer(&server.uri())
                .fetch(
                    "https://example.com/gone",
                    &HashMap::new(),
                    None,
                    deadline(),
                )
                .await
                .expect("a page either way");
            assert_eq!(res.status_code, want, "navigation status {nav_status}");
        }
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
            CrwError::RendererError(m) => assert!(m.contains("not a string")),
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

    /// A document still arriving when the settle ran out is read again.
    #[tokio::test]
    async fn page_still_loading_is_read_again() {
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
                "result": "<html><head><title>Item | eBay</title></head><body><div id=\"mainContent\"></div></body></html>"
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": "<html><head><title>Item | eBay</title></head><body><div id=\"mainContent\"><h1>real page</h1></div></body></html>"
            })))
            .mount(&server)
            .await;
        mount_delete_session(&server).await;

        let r = renderer(&server.uri()).with_challenge_wait(200);
        let res = r
            .fetch("https://example.com", &HashMap::new(), None, deadline())
            .await
            .expect("the page finished loading, so it must be returned");
        assert!(res.html.contains("real page"));
    }

    /// The point of the poll: a challenge that clears between evaluates now
    /// yields the real page instead of the interstitial.
    #[tokio::test]
    async fn challenge_that_clears_on_a_later_poll_is_returned_as_content() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tabs"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"tabId": "t1", "httpStatus": 404})),
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
        // Served with a 404, cleared into the page: not a missing page.
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
            looks_like_wall("<script>window.KPSDK={};KPSDK.start=KPSDK.now();</script>"),
            Some("challenge")
        );
        assert_eq!(
            looks_like_wall(r#"<div id="sec-if-cpt-container" style="display: none"></div>"#),
            Some("challenge")
        );
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

    // ── one session per host (#637) ────────────────────────────────────────

    const PAGE: &str = "<html><body>real content well over the empty threshold here</body></html>";

    /// A sidecar whose tab `t1` serves `PAGE`, its evaluate answered after
    /// `delay`, the create reporting `nav_status`.
    async fn sidecar(nav_status: u16, delay: Duration) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tabs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tabId": "t1", "httpStatus": nav_status
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(delay)
                    .set_body_json(serde_json::json!({"ok": true, "result": PAGE})),
            )
            .mount(&server)
            .await;
        server
    }

    /// Records when each `POST /tabs` arrived.
    #[derive(Clone, Default)]
    struct Arrivals(std::sync::Arc<Mutex<Vec<Instant>>>);

    impl wiremock::Respond for Arrivals {
        fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
            self.0.lock().unwrap().push(Instant::now());
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"tabId": "t1"}))
        }
    }

    impl Arrivals {
        fn gap(&self) -> Duration {
            let at = self.0.lock().unwrap();
            assert_eq!(at.len(), 2, "two creates");
            at[1].duration_since(at[0])
        }
    }

    /// Like [`sidecar`], recording create arrivals, every DELETE accepted.
    async fn timed_sidecar(delay: Duration) -> (MockServer, Arrivals) {
        let server = MockServer::start().await;
        let arrivals = Arrivals::default();
        Mock::given(method("POST"))
            .and(path("/tabs"))
            .respond_with(arrivals.clone())
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tabs/t1/evaluate"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(delay)
                    .set_body_json(serde_json::json!({"ok": true, "result": PAGE})),
            )
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;
        (server, arrivals)
    }

    async fn expect_deletes(server: &MockServer, sessions: u64, groups: u64) {
        Mock::given(method("DELETE"))
            .and(path_regex(r"^/sessions/crw_[0-9a-f]{32}$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .expect(sessions)
            .mount(server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(r"^/tabs/group/task_[0-9a-f]{32}$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .expect(groups)
            .mount(server)
            .await;
    }

    /// The `userId` of every create, in arrival order.
    async fn created_user_ids(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .expect("recorded requests")
            .iter()
            .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/tabs")
            .map(|r| {
                let body: serde_json::Value = serde_json::from_slice(&r.body).expect("json body");
                body["userId"].as_str().expect("userId").to_string()
            })
            .collect()
    }

    async fn fetch_ok(r: &CamoufoxRenderer, url: &str) {
        r.fetch(url, &HashMap::new(), None, deadline())
            .await
            .expect("fetch should succeed");
    }

    #[test]
    fn user_id_hashes_the_host_scope() {
        let key = session_key("https://WWW.Phoronix.com/news/x").expect("has a host");
        assert_eq!(key, "www.phoronix.com|-|camoufox");
        let id = session_user_id(&key, 0);
        assert_eq!(id.len(), 4 + 32);
        assert!(id.starts_with("crw_") && id[4..].bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!id.contains("phoronix"));
        assert_eq!(
            id,
            session_user_id(&session_key("https://www.phoronix.com/").unwrap(), 0)
        );
        assert_ne!(
            id,
            session_user_id(&session_key("https://phoronix.com/").unwrap(), 0)
        );
        assert_ne!(
            id,
            session_user_id(&key, 1),
            "a wall moves the host to a new id"
        );
        assert_eq!(session_key("data:text/html,hi"), None);
    }

    #[tokio::test]
    async fn request_proxy_partitions_the_session_without_leaking_into_the_id() {
        let proxy = std::sync::Arc::new(
            crw_core::ProxyEntry::parse("http://user:secret@px.example:8080").expect("proxy"),
        );
        let key = crate::REQUEST_PROXY
            .scope(Some(proxy), async { session_key("https://example.com/") })
            .await
            .expect("has a host");
        assert!(key.contains("secret"), "the key pins the proxy: {key}");
        let id = session_user_id(&key, 0);
        assert!(!id.contains("secret") && !id.contains("px.example"));
        assert_ne!(
            id,
            session_user_id(&session_key("https://example.com/").unwrap(), 0)
        );
    }

    #[tokio::test]
    async fn host_that_never_challenged_deletes_its_session_and_keeps_its_id() {
        let server = sidecar(200, Duration::ZERO).await;
        expect_deletes(&server, 2, 0).await;
        let r = renderer(&server.uri());
        fetch_ok(&r, "https://plain.example/a").await;
        fetch_ok(&r, "https://plain.example/b").await;
        let ids = created_user_ids(&server).await;
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], ids[1], "one host, one session id");
        let deleted: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method.as_str() == "DELETE")
            .map(|r| r.url.path().trim_start_matches("/sessions/").to_string())
            .collect();
        assert_eq!(deleted, vec![ids[0].clone(), ids[0].clone()]);
    }

    /// A page reached through a challenge leaves the session alive for the
    /// next page, which runs under the same id and closes only its own tab.
    #[tokio::test]
    async fn solved_challenge_keeps_the_session_for_the_next_page() {
        let server = sidecar(403, Duration::ZERO).await;
        expect_deletes(&server, 0, 2).await;
        let r = renderer(&server.uri());
        fetch_ok(&r, "https://guarded.example/1").await;
        fetch_ok(&r, "https://guarded.example/2").await;
        let ids = created_user_ids(&server).await;
        assert_eq!(ids[0], ids[1]);
        let group_closes = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().starts_with("/tabs/group/"))
            .filter(|r| r.url.query() == Some(format!("userId={}", ids[0]).as_str()))
            .count();
        assert_eq!(
            group_closes, 2,
            "each page closes its group under the shared id"
        );
    }

    /// While a sibling is still running, a request closes only its own tab:
    /// deleting the session would kill the sibling's page. The last one out
    /// deletes the session.
    #[tokio::test]
    async fn overlapping_requests_close_only_their_own_tab() {
        let server = sidecar(200, Duration::from_millis(400)).await;
        expect_deletes(&server, 1, 1).await;
        let r = renderer(&server.uri());
        // Mark the host clear first so both run in parallel, not single flight.
        lock(&r.hosts).insert(
            session_key("https://pair.example/").unwrap(),
            HostSession {
                ok_at: Some(Instant::now()),
                ..HostSession::default()
            },
        );
        tokio::join!(
            fetch_ok(&r, "https://pair.example/a"),
            fetch_ok(&r, "https://pair.example/b")
        );
    }

    #[tokio::test]
    async fn the_request_past_the_share_cap_runs_on_its_own_session() {
        let server = sidecar(200, Duration::from_millis(400)).await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;
        let r = renderer(&server.uri());
        lock(&r.hosts).insert(
            session_key("https://busy.example/").unwrap(),
            HostSession {
                ok_at: Some(Instant::now()),
                ..HostSession::default()
            },
        );
        tokio::join!(
            fetch_ok(&r, "https://busy.example/1"),
            fetch_ok(&r, "https://busy.example/2"),
            fetch_ok(&r, "https://busy.example/3"),
            fetch_ok(&r, "https://busy.example/4"),
        );
        let ids = created_user_ids(&server).await;
        let shared = session_user_id(&session_key("https://busy.example/").unwrap(), 0);
        assert_eq!(ids.iter().filter(|id| **id == shared).count(), 3);
        let own: Vec<&String> = ids.iter().filter(|id| **id != shared).collect();
        assert_eq!(own.len(), 1);
        let deletes = server.received_requests().await.unwrap();
        let session_deletes: Vec<&str> = deletes
            .iter()
            .filter(|r| r.method.as_str() == "DELETE" && r.url.path().starts_with("/sessions/"))
            .map(|r| r.url.path())
            .collect();
        assert!(session_deletes.contains(&format!("/sessions/{}", own[0]).as_str()));
        assert!(session_deletes.contains(&format!("/sessions/{shared}").as_str()));
    }

    #[test]
    fn closing_host_sends_joiners_to_their_own_session_until_the_delete_returns() {
        let r = renderer("http://127.0.0.1:1");
        let now = Instant::now();
        let url = "https://closing.example/";
        let key = session_key(url).unwrap();
        let ok = Outcome {
            ok: true,
            solved: false,
            wall: false,
        };

        let mut first = r.join(url, now);
        let mut second = r.join(url, now);
        let Lease::Shared(slot) = &mut first else {
            panic!("first shares")
        };
        assert_eq!(
            slot.finish(ok, now),
            Cleanup::Group,
            "a sibling is still running"
        );
        let Lease::Shared(slot) = &mut second else {
            panic!("second shares")
        };
        assert_eq!(slot.finish(ok, now), Cleanup::Session, "last one out");
        assert!(lock(&r.hosts)[&key].closing);
        assert!(
            matches!(r.join(url, now), Lease::OneOff(_)),
            "never join a closing session"
        );

        drop(first);
        drop(second);
        let hosts = lock(&r.hosts);
        assert!(!hosts[&key].closing);
        assert_eq!(hosts[&key].in_flight, 0);
    }

    #[test]
    fn a_cancelled_request_gives_its_place_back() {
        let r = renderer("http://127.0.0.1:1");
        let url = "https://cancel.example/";
        for _ in 0..(SHARED_SESSION_MAX_IN_FLIGHT + 2) {
            let lease = r.join(url, Instant::now());
            assert!(matches!(lease, Lease::Shared(_)), "the slot was released");
            drop(lease);
        }
        assert_eq!(lock(&r.hosts)[&session_key(url).unwrap()].in_flight, 0);
    }

    #[test]
    fn a_wall_moves_the_host_to_a_fresh_id() {
        let r = renderer("http://127.0.0.1:1");
        let url = "https://walled.example/";
        let now = Instant::now();
        let mut lease = r.join(url, now);
        let before = lease.user_id().to_string();
        let Lease::Shared(slot) = &mut lease else {
            panic!("shares")
        };
        let wall = Outcome {
            ok: false,
            solved: false,
            wall: true,
        };
        assert_eq!(slot.finish(wall, now), Cleanup::Session);
        drop(lease);
        // Another host's join prunes; the wall's bump must survive it.
        drop(r.join("https://elsewhere.example/", now));
        let after = r.join(url, now);
        assert_ne!(after.user_id(), before);
    }

    #[test]
    fn idle_hosts_are_pruned_and_a_recent_wall_is_kept() {
        let r = renderer("http://127.0.0.1:1");
        let old = Instant::now() - HOST_WARM_TTL - Duration::from_secs(1);
        let mut hosts = lock(&r.hosts);
        hosts.insert(
            "idle.example|-|camoufox".into(),
            HostSession {
                ok_at: Some(old),
                ..HostSession::default()
            },
        );
        hosts.insert(
            "walled-long-ago.example|-|camoufox".into(),
            HostSession {
                generation: 1,
                walled_at: Some(old),
                ..HostSession::default()
            },
        );
        hosts.insert(
            "walled.example|-|camoufox".into(),
            HostSession {
                generation: 1,
                walled_at: Some(Instant::now()),
                ..HostSession::default()
            },
        );
        drop(hosts);
        drop(r.join("https://other.example/", Instant::now()));
        let hosts = lock(&r.hosts);
        assert!(!hosts.contains_key("idle.example|-|camoufox"));
        assert!(!hosts.contains_key("walled-long-ago.example|-|camoufox"));
        assert_eq!(hosts["walled.example|-|camoufox"].generation, 1);
    }

    /// The first pages of a burst to a host not yet known to be clear run one
    /// at a time, so only one of them faces the challenge: the second create
    /// is only sent once the first request has done both of its reads.
    #[tokio::test]
    async fn a_cold_burst_runs_one_solve_at_a_time() {
        let delay = Duration::from_millis(400);
        let (server, arrivals) = timed_sidecar(delay).await;
        let r = renderer(&server.uri());
        tokio::join!(
            fetch_ok(&r, "https://cold-burst.example/1"),
            fetch_ok(&r, "https://cold-burst.example/2")
        );
        assert!(
            arrivals.gap() >= delay * 2,
            "serialised: {:?}",
            arrivals.gap()
        );
        drop(server);
    }

    /// Once the host is clear, the same burst runs in parallel.
    #[tokio::test]
    async fn a_clear_host_does_not_wait_for_a_solve() {
        let delay = Duration::from_millis(400);
        let (server, arrivals) = timed_sidecar(delay).await;
        let r = renderer(&server.uri());
        lock(&r.hosts).insert(
            session_key("https://warm-burst.example/").unwrap(),
            HostSession {
                ok_at: Some(Instant::now()),
                ..HostSession::default()
            },
        );
        tokio::join!(
            fetch_ok(&r, "https://warm-burst.example/1"),
            fetch_ok(&r, "https://warm-burst.example/2")
        );
        assert!(arrivals.gap() < delay, "parallel: {:?}", arrivals.gap());
        drop(server);
    }

    /// A waiter waits for the solver only as long as its own attempt can
    /// afford, then goes ahead without the lock instead of failing on the wait.
    #[tokio::test]
    async fn a_waiter_out_of_time_goes_ahead_without_the_lock() {
        // The solver's two reads take 3s; the waiter can afford 1s of waiting.
        let (server, arrivals) = timed_sidecar(Duration::from_millis(1500)).await;
        let r = renderer(&server.uri());
        let no_headers = HashMap::new();
        let solver = r.fetch(
            "https://slow-solve.example/1",
            &no_headers,
            None,
            deadline(),
        );
        let waiter = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let budget = (before_poll(None) + Duration::from_secs(1)).as_millis() as u64;
            r.fetch(
                "https://slow-solve.example/2",
                &no_headers,
                None,
                Deadline::from_request_ms(budget),
            )
            .await
        };
        let (solved, waited) = tokio::join!(solver, waiter);
        solved.expect("the solver finishes");
        waited.expect("the waiter finishes on its own");
        let gap = arrivals.gap();
        assert!(gap >= Duration::from_millis(900), "it waited: {gap:?}");
        assert!(
            gap < Duration::from_millis(2900),
            "it did not wait for the solve: {gap:?}"
        );
        drop(server);
    }

    /// After waiting, a request still reaches its poll with more than the
    /// challenge reserve left, so a recovery arm after this tier keeps its floor.
    #[test]
    fn a_lock_wait_leaves_the_reserve_and_one_attempt() {
        let reserve = Duration::from_millis(29_250);
        for wait_for in [
            None,
            Some(Duration::from_secs(2)),
            Some(Duration::from_secs(15)),
        ] {
            for remaining in [10, 30, 45, 60, 90].map(Duration::from_secs) {
                let wait = lock_wait_budget(remaining, reserve, wait_for);
                // Create, the page wait and the first read, at their allowances.
                let page_wait = wait_for.map_or(SETTLE_MAX, |w| w.max(SETTLE_MAX));
                let at_poll = remaining
                    .saturating_sub(wait)
                    .saturating_sub(CREATE_ALLOWANCE + page_wait + WAIT_FOR_READ_RESERVE);
                assert!(
                    wait.is_zero() || at_poll > reserve,
                    "{remaining:?} {wait_for:?}: waits {wait:?}, reaches the poll with {at_poll:?}"
                );
            }
        }
        // A long waitFor leaves less room to wait than the settle wait does.
        let sixty = Duration::from_secs(60);
        assert!(
            lock_wait_budget(sixty, reserve, Some(Duration::from_secs(15)))
                < lock_wait_budget(sixty, reserve, None)
        );
        assert!(lock_wait_budget(Duration::from_secs(10), Duration::ZERO, None).is_zero());
    }
}
