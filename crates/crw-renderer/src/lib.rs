//! HTTP and headless-browser rendering engine for the CRW web scraper.
//!
//! Provides a [`FallbackRenderer`] that fetches pages via plain HTTP and optionally
//! re-renders them through a CDP-based headless browser when SPA content is detected.
//!
//! - [`http_only`] — Simple HTTP fetcher using `reqwest`
//! - [`detector`] — Heuristic SPA shell detection (empty body, framework markers)
//! - `cdp` — Chrome DevTools Protocol renderer (LightPanda, Playwright, Chrome) *(requires `cdp` feature)*
//! - [`traits`] — [`PageFetcher`] trait for pluggable backends
//!
//! # Feature flags
//!
//! | Flag  | Description |
//! |-------|-------------|
//! | `cdp` | Enables CDP WebSocket rendering via `tokio-tungstenite` |
//!
//! # Example
//!
//! ```rust,no_run
//! use crw_core::config::RendererConfig;
//! use crw_renderer::FallbackRenderer;
//! use std::collections::HashMap;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use crw_core::config::StealthConfig;
//! let config = RendererConfig::default();
//! let stealth = StealthConfig::default();
//! let renderer = FallbackRenderer::new(&config, "crw/0.1", None, &stealth)?;
//! let deadline = crw_core::Deadline::from_request_ms(8000);
//! let result = renderer.fetch("https://example.com", &HashMap::new(), None, None, None, deadline).await?;
//! println!("status: {}", result.status_code);
//! # Ok(())
//! # }
//! ```

/// One lock for every test that mutates process-wide env vars in this
/// crate's unit-test binary (http_only's and impersonated's loopback escape
/// hatch flips). Without a SHARED lock, two modules flipping the same var
/// concurrently race each other into flaky failures.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub mod blocklist;
pub mod breaker;
#[cfg(feature = "auto-browser")]
pub mod browser;
#[cfg(feature = "cdp")]
pub mod browser_pool;
#[cfg(feature = "camoufox")]
pub mod camoufox;
#[cfg(feature = "cdp")]
pub mod cdp;
#[cfg(feature = "cdp")]
pub mod cdp_conn;
pub mod clearance;
#[cfg(feature = "cloak")]
pub mod cloak;
pub mod detector;
pub mod egress;
#[cfg(feature = "cdp")]
pub mod health_telemetry;
pub mod host_limiter;
pub mod http_only;
#[cfg(feature = "impersonated")]
pub mod impersonated;
pub mod locale;
pub mod preference;
pub mod traits;

use crate::breaker::{
    AttemptContext, BreakerOutcome, BreakerRegistry, Permit, ProbeGuard, classify_outcome,
};
use crate::preference::HostPreferences;
use crw_core::config::{BUILTIN_UA_POOL, RendererConfig, RendererMode, StealthConfig};
use crw_core::error::{CrwError, CrwResult};
use crw_core::metrics::metrics;
use crw_core::types::{
    FailoverErrorKind, FetchResult, RenderDecision, RendererKind, resolve_render_js,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use traits::PageFetcher;

tokio::task_local! {
    /// Per-request country code (ISO 3166-1 alpha-2, lowercase) for the
    /// chrome_proxy tier's CDP auth pump. Set by `FallbackRenderer::fetch`
    /// when a `ScrapeRequest.country` is present; read in `cdp.rs` while
    /// composing DataImpulse credentials. Task-local so child tasks
    /// spawned by the pool inherit it without trait-signature churn.
    pub static REQUEST_COUNTRY: Option<String>;
}

tokio::task_local! {
    /// Resolved proxy entry for the current request, picked from the active
    /// rotator by host. Set by the scrape/crawl entry points (via
    /// [`FallbackRenderer::pick_proxy`]); read in `cdp.rs` to drive the
    /// per-request Chrome `proxyServer` (a fresh proxied browser context) and
    /// the `Fetch.authRequired` pump. `None` = no proxy → existing behaviour.
    pub static REQUEST_PROXY: Option<Arc<crw_core::ProxyEntry>>;
}

/// Per-request screenshot capture parameters. Carried via a task-local rather
/// than the `PageFetcher::fetch` signature (mirrors [`REQUEST_PROXY`]) so the
/// trait + its ~30 call sites stay untouched. `Some` ⇒ capture a PNG via CDP
/// `Page.captureScreenshot` after the wait window; `None` ⇒ no screenshot.
#[derive(Debug, Clone, Copy)]
pub struct ScreenshotReq {
    /// Capture the full scrollable page (`captureBeyondViewport`) vs. just the
    /// current viewport.
    pub full_page: bool,
}

tokio::task_local! {
    /// Resolved screenshot request for the current scrape. Set by the
    /// scrape/crawl entry point ([`crw_crawl::single::scrape_url`]) when
    /// `formats` contains `Screenshot`; read in `cdp.rs` to drive
    /// `Page.captureScreenshot` and in [`FallbackRenderer::fetch`] to force the
    /// vanilla-Chrome CDP path. `None` = no screenshot → existing behaviour.
    pub static REQUEST_SCREENSHOT: Option<ScreenshotReq>;
}

/// Attach the browser-context pool to a CDP tier, when enabled and supported.
///
/// Every pooled tier gets its own pool, sized from the shared
/// `[renderer.chrome_pool]` block. Beyond amortizing the handshake, the pool is
/// what puts each render in its own browser context: the legacy path only
/// builds a context when the request carries its own proxy, so an unpooled tier
/// opens targets in the *default* context, which cannot be disposed and so
/// cannot be reaped when a render is cancelled.
///
/// Gated off on browserless v2 per plan §"Out of scope". The backend is set
/// explicitly in config; never URL-sniffed.
#[cfg(feature = "cdp")]
fn maybe_with_pool(
    renderer: cdp::CdpRenderer,
    config: &crw_core::config::RendererConfig,
    tier: &'static str,
) -> cdp::CdpRenderer {
    if !config.chrome_context_pool_enabled {
        return renderer;
    }
    match config.chrome_backend {
        crw_core::config::ChromeBackend::Vanilla => {
            let pcfg = &config.chrome_pool;
            let size = pcfg.size.unwrap_or_else(|| {
                let n = std::thread::available_parallelism()
                    .map(|p| p.get())
                    .unwrap_or(2);
                std::cmp::max(2, n / 2)
            });
            tracing::info!(tier, pool_size = size, "browser-context pool enabled");
            renderer.with_pool(browser_pool::PoolCfg {
                size,
                reserved_interactive_renders: pcfg.reserved_interactive_renders,
                recycle_after_navs: pcfg.recycle_after_navs,
                idle_timeout: std::time::Duration::from_secs(pcfg.idle_timeout_secs),
                health_check_after: std::time::Duration::from_secs(pcfg.health_check_secs),
                shutdown_drain: std::time::Duration::from_secs(pcfg.shutdown_drain_secs),
                close_target_timeout: std::time::Duration::from_secs(2),
                dispose_ctx_timeout: std::time::Duration::from_secs(1),
                create_ctx_timeout: std::time::Duration::from_secs(1),
            })
        }
        crw_core::config::ChromeBackend::Browserless => {
            tracing::warn!(
                tier,
                "chrome_context_pool_enabled = true but chrome_backend = browserless — \
                 pool unsupported on this backend in v1, falling back to legacy path"
            );
            renderer
        }
    }
}

/// Whether the named renderer tier can capture a screenshot.
///
/// Screenshot capture is CDP `Page.captureScreenshot` on vanilla Chrome
/// (`chrome`, `chrome_proxy`, `playwright`). LightPanda's CdpRenderer returns a
/// ~30-byte stub and Camoufox is an HTTP sidecar that doesn't speak CDP —
/// neither can capture.
///
/// An ALLOWLIST, so it fails closed: a tier added later is treated as unable to
/// capture until it is listed here. A denylist would silently advertise
/// `screenshot.supported: true` for a new non-CDP tier and then hand back an
/// empty capture.
///
/// SINGLE SOURCE OF TRUTH: used both by the request-time renderer filter in
/// `FallbackRenderer::fetch_with_js` and by
/// [`FallbackRenderer::supports_screenshot`] (which `/v1/capabilities` reports),
/// so the advertised capability and the runtime behaviour cannot drift apart.
pub fn renderer_can_screenshot(name: &str) -> bool {
    matches!(name, "chrome" | "chrome_proxy" | "playwright")
}

/// Whether a screenshot was requested for the current task (reads the
/// [`REQUEST_SCREENSHOT`] task-local). `false` when unset / outside a scope.
pub fn screenshot_requested() -> bool {
    REQUEST_SCREENSHOT
        .try_with(|s| s.is_some())
        .unwrap_or(false)
}

/// The resolved screenshot params for the current task, if any.
pub fn current_screenshot_req() -> Option<ScreenshotReq> {
    REQUEST_SCREENSHOT.try_with(|s| *s).ok().flatten()
}

/// Map a renderer's name string to the closed `RendererKind` enum.
/// Returns `None` for unknown names (e.g. "playwright" — treated as a
/// JS renderer but not tracked in metrics/preferences).
fn renderer_kind_for(name: &str) -> Option<RendererKind> {
    match name {
        "http" | "http_only_fallback" => Some(RendererKind::Http),
        "lightpanda" => Some(RendererKind::Lightpanda),
        "chrome" => Some(RendererKind::Chrome),
        "chrome_proxy" => Some(RendererKind::ChromeProxy),
        "camoufox" => Some(RendererKind::Camoufox),
        "cloak" => Some(RendererKind::Cloak),
        "impersonated-http" => Some(RendererKind::ImpersonatedHttp),
        _ => None,
    }
}

/// Classify a renderer-side error into a `FailoverErrorKind` for the
/// preference learner. Match on `CrwError` variants (not error strings),
/// so renaming or rewording the human-readable message can't silently
/// reclassify failures and over-promote hosts.
///
/// Only LightPanda-specific failures drive promotion (see
/// [`FailoverErrorKind::counts_for_promotion`]); transport / unreachable
/// errors stay in `NetworkError` so a flaky upstream doesn't push hosts
/// to Chrome.
fn classify_renderer_error(err: &CrwError) -> FailoverErrorKind {
    match err {
        CrwError::Timeout(_) => FailoverErrorKind::LightpandaTimeout,
        CrwError::TargetUnreachable(_) => FailoverErrorKind::NetworkError,
        CrwError::HttpError(_) => FailoverErrorKind::NetworkError,
        // RendererError covers WS disconnects, CDP frame errors, render
        // pipeline crashes — these are LightPanda-attributable.
        CrwError::RendererError(_) => FailoverErrorKind::LightpandaCrash,
        _ => FailoverErrorKind::Other,
    }
}

/// Build a per-tier timeout map from the renderer config. Used by the
/// breaker layer for pre-flight skip and clamp detection.
fn tier_timeouts_from(
    config: &RendererConfig,
) -> std::collections::HashMap<RendererKind, std::time::Duration> {
    let mut m = std::collections::HashMap::new();
    m.insert(
        RendererKind::Http,
        std::time::Duration::from_millis(config.http_timeout()),
    );
    m.insert(
        RendererKind::Lightpanda,
        std::time::Duration::from_millis(config.lightpanda_timeout()),
    );
    m.insert(
        RendererKind::Chrome,
        std::time::Duration::from_millis(config.chrome_timeout()),
    );
    m.insert(
        RendererKind::ChromeProxy,
        std::time::Duration::from_millis(config.chrome_proxy_timeout()),
    );
    // Unconditional: `camoufox_timeout()` exists regardless of feature. The map
    // entry is consulted only when a camoufox renderer is actually in the pool,
    // so an unused entry in lean builds is harmless and keeps this function
    // feature-free.
    m.insert(
        RendererKind::Camoufox,
        std::time::Duration::from_millis(config.camoufox_timeout()),
    );
    // Unconditional, same rationale as camoufox: `cloak_timeout()` exists in
    // every build; the entry is consulted only when a cloak renderer is in the
    // pool, so it is harmless dead capacity in lean.
    m.insert(
        RendererKind::Cloak,
        std::time::Duration::from_millis(config.cloak_timeout()),
    );
    // No `ImpersonatedHttp` entry: the only two readers of this map walk the
    // JS ladder, and the impersonated tier sits outside it by design. Its
    // budget lives in `FallbackRenderer::impersonated_timeout_ms`, the value
    // the per-proxy cache and the hop actually use.
    m
}

/// Credit cost per fetched page. Flat 1 for every renderer: the SaaS bills 1
/// credit per scrape regardless of renderer, and `data.credit_cost` is the
/// field docs tell users to audit their charge against — so it must equal that
/// charge. ponytail: per-renderer pricing removed; re-add a `match kind` here
/// (e.g. `Chrome => 2`) if a renderer ever needs to cost more than the base.
fn credit_for(_kind: RendererKind) -> u32 {
    1
}

/// Stamp `render_decision` and `credit_cost` for an HTTP-only result.
/// `requested_renderer` is taken into account: if the user explicitly
/// pinned `"http"` we mark it as `UserPinned`, otherwise `AutoDefault`.
fn stamp_http_decision(result: &mut FetchResult, requested_renderer: Option<&str>) {
    if result.render_decision.is_some() {
        return;
    }
    let kind = RendererKind::Http;
    result.credit_cost = credit_for(kind);
    result.render_decision = Some(match requested_renderer {
        Some("http") => RenderDecision::UserPinned { renderer: kind },
        _ => RenderDecision::AutoDefault { chosen: kind },
    });
    // Mirror the JS-renderer metric so dashboards see HTTP routing too.
    metrics()
        .render_route_decision_total
        .with_label_values(&[kind.as_str(), "success"])
        .inc();
}

/// Extract the host from a URL string, returning an empty string on failure.
fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_default()
}

/// Pick a user-agent: rotate from stealth pool when stealth is enabled.
fn pick_ua<'a>(default_ua: &'a str, stealth: &'a StealthConfig) -> String {
    if stealth.enabled {
        let pool: &[&str] = if stealth.user_agents.is_empty() {
            BUILTIN_UA_POOL
        } else {
            // Safe: user_agents is non-empty in this branch.
            return stealth.user_agents[rand::random_range(0..stealth.user_agents.len())].clone();
        };
        pool[rand::random_range(0..pool.len())].to_string()
    } else {
        default_ua.to_string()
    }
}

/// Pure classification of a JS-renderer result (no side-effects). Produced by
/// `FallbackRenderer::classify_js_attempt`; consumed by the serial loop and the
/// conditional hedge to apply the identical accept-gate.
#[allow(dead_code)] // full classification kept for completeness; hedge uses a subset
struct JsAttemptClass {
    text_len: usize,
    is_placeholder: bool,
    failed_render: Option<detector::FailedRenderReason>,
    is_bot_wall: bool,
    vendor_block: Option<&'static str>,
    is_status_blocked: bool,
    antibot: crw_extract::antibot::AntibotResult,
    antibot_blocked: bool,
    /// Egress-recoverable hard-block (drives the gated chrome_proxy recovery arm).
    hard_block: bool,
    /// A fingerprint-vendor wall chrome_proxy cannot clear (suppresses the arm).
    unrecoverable_wall: bool,
    /// Passes the success accept-gate (return as-is, don't escalate).
    acceptable: bool,
}

/// A fingerprint-vendor wall that chrome_proxy's default Chrome fingerprint
/// cannot clear (it needs the camoufox/cloak stealth tier): a Cloudflare managed
/// challenge, DataDome, PerimeterX, Kasada, Akamai, or Imperva. Firing the slow
/// residential tier on one just burns the deadline (the p90 regression the arm
/// was tuned against), so it suppresses the arm. Distinct from IP-reputation
/// blocks (429 / IP-ban 403 / generic bot walls / CF error-1020) that a
/// residential egress DOES recover — those still fire the arm. The `vendor_block`
/// "cloudflare" arm is deliberately excluded (CF is matched via `cf_challenge`)
/// so a CF error-1020 IP block is NOT wrongly suppressed.
/// See [`FallbackRenderer::has_recovery_tier`]. True when SOME tier can plausibly
/// clear an IP-reputation block: residential CDP egress, a stealth tier that the
/// auto path can actually reach, or a usable fallback HTTP proxy.
///
/// Every input is the REAL constructed thing, never a config flag or an env var:
///   * a `chrome_proxy` / `camoufox` entry present in `js_renderers` — camoufox
///     with `include_in_auto = false` is filtered out of the auto chain per
///     request, so it is excluded here too or a self-hoster who configured it
///     for pinned use only would silently lose the breaker's brake;
///   * `cloak_arm`, which is stored OUTSIDE `js_renderers` and would otherwise
///     read as "no recovery" while a perfectly good recovery tier exists;
///   * the concrete fetcher's `has_ratelimit_proxy()`, not the env var — a malformed
///     `CRW_HTTP_RATELIMIT_PROXY_URL` leaves the client `None`, and a typo must
///     not be mistaken for a recovery egress.
fn has_recovery_tier(
    config: &crw_core::config::RendererConfig,
    js_renderers: &[Arc<dyn PageFetcher>],
    cloak_arm_present: bool,
    http_fallback_proxy_ready: bool,
) -> bool {
    let auto_reachable = |name: &str| {
        js_renderers.iter().any(|r| r.name() == name)
            && match name {
                "camoufox" => config
                    .camoufox
                    .as_ref()
                    .is_some_and(|cf| cf.include_in_auto),
                _ => true,
            }
    };
    auto_reachable("chrome_proxy")
        || auto_reachable("camoufox")
        || cloak_arm_present
        || http_fallback_proxy_ready
}

/// Is this response an HTML document, or an unknown type we must assume is one?
///
/// Three callers now, one question. A browser render can only add to an HTML
/// document, the HTML structural heuristics in `antibot::classify` are only
/// meaningful on one (`crw_crawl::single::classify_block`), and `crw_extract`
/// uses it to decide whether running an HTML parser / HTML-to-markdown
/// converter over the body is even meaningful. Defined in `crw-core` (shared
/// by extract and renderer, which cannot depend on each other) and
/// re-exported here for existing callers.
pub use crw_core::is_html_like_content_type;

fn is_fingerprint_vendor_wall(
    cf_challenge: bool,
    vendor_block: Option<&str>,
    antibot_signal: crw_extract::antibot::AntibotSignal,
) -> bool {
    use crw_extract::antibot::AntibotSignal;
    cf_challenge
        || matches!(
            vendor_block,
            Some("datadome" | "perimeterx" | "kasada" | "akamai" | "imperva")
        )
        // `antibot::classify` recognises these vendors from visible text alone
        // (a PerimeterX/Imperva page without its SDK marker, or a CF page with a
        // single weak marker) that `looks_like_vendor_block` misses — yet those
        // feed `saw_hard_block` via `antibot.signal.is_blocked()`, so without
        // this arm the residential tier would fire on a wall it can't clear.
        // Sucuri / NetworkSecurity / GenericBlock / RateLimited /
        // StructuralFailure stay OUT (IP-reputation blocks a residential egress
        // recovers).
        || matches!(
            antibot_signal,
            AntibotSignal::Cloudflare
                | AntibotSignal::Datadome
                | AntibotSignal::PerimeterX
                | AntibotSignal::Akamai
                | AntibotSignal::Imperva
                | AntibotSignal::Kasada
        )
}

/// Result of the conditional hedge (race lightpanda+chrome).
enum HedgeOutcome {
    /// A tier passed the accept-gate — return as-is (terminal).
    Accepted(FetchResult),
    /// Both tiers thin/failed — best-thin result, whether a hard-block was seen
    /// (so the caller can fire the gated auto-egress recovery arm), and whether
    /// any attempt was a fingerprint-vendor wall (so the arm is suppressed even
    /// if the size-race stitch dropped that attempt's HTML). Mirrors the serial
    /// loop's `thin_result` + `saw_hard_block` + `saw_unrecoverable_wall`.
    Thin(FetchResult, bool, bool),
}

/// Did this renderer error come from failing to reach/navigate the ORIGIN, as
/// opposed to a fault on our side (CDP pool exhausted, browser discovery failed,
/// a pinned renderer that does not exist)?
///
/// Only used to decide whether an unreachable origin should outrank a JS-tier error
/// when both fail. Getting it wrong in the permissive direction (treating our fault as
/// the origin's) would blame the caller for our outage, so the match is deliberately
/// narrow.
fn is_origin_navigation_failure(e: &CrwError) -> bool {
    match e {
        CrwError::TargetUnreachable(_) => true,
        // Chrome/LightPanda report a failure to reach the origin as a navigation error
        // with a `net::ERR_*` code. Internal faults (pool exhausted, CDP discovery)
        // carry different messages and keep their own error.
        CrwError::RendererError(msg) => {
            let m = msg.to_ascii_lowercase();
            m.contains("navigation failed")
                || m.contains("net::err_")
                // "we could not confirm where this origin points" — the CDP
                // destination re-check collapses NXDOMAIN and a resolver
                // brown-out into one `Unresolved`. Same reasoning as the
                // `Timeout` arm below: absence of evidence, not evidence
                // against the HTTP tier's independent finding. Without this a
                // dead host exits as 500 `renderer_error` ("the page could not
                // be rendered") instead of 422 `target_unreachable`, which was
                // 10,107 requests over 14 days in prod.
                || m.contains("outbound destination check unavailable")
        }
        // A JS-tier timeout does not refute the HTTP tier's positive finding. Both
        // callers only consult this when the HTTP error was already
        // `TargetUnreachable`, i.e. two independent egresses failed to connect; a
        // browser that then also gets no answer is absence of evidence, not evidence
        // against. A host that blackholes SYNs hangs every tier, so chrome/lightpanda
        // report a plain timeout and never a `net::ERR_*`, which is why the arm above
        // alone never fired for this class. Safe because the pairing is required: an
        // `http=Timeout` (slow origin) or `HttpError` (our proxy) is not
        // `TargetUnreachable`, so a CDP-pool outage of ours can never be laundered
        // into a caller-blaming 422 on its own. It also does not matter if THIS
        // `js` timeout happens to be our own CDP connect (e.g. `cdp_conn.rs`'s
        // websocket handshake) rather than a hung navigation: the 422 verdict
        // rests on the HTTP tier's independent, already-verified
        // `TargetUnreachable` finding, not on why the browser failed. The JS
        // failure only ever needs to not contradict that finding.
        CrwError::Timeout(_) => true,
        _ => false,
    }
}

/// Is this failure the ORIGIN's fault, for breaker-scoping purposes only?
///
/// Deliberately narrower than [`is_origin_navigation_failure`], which exists to
/// decide error ATTRIBUTION and is generous about `net::ERR_*` on purpose. The
/// breaker asks a different question: does this failure say anything about the
/// TIER's own health? Chromium reports a proxy tunnel that will not open, and a
/// box that lost its network, with the same `net::ERR_` shape as a dead origin,
/// but those are ours and must keep reaching the global window or nothing ever
/// concludes the tier is sick. `cdp.rs::is_proxy_tunnel_error` already draws
/// this exact line for its country-retry, with the same reasoning.
///
/// `ERR_NAME_NOT_RESOLVED` stays on the origin side. A resolver brown-out would
/// produce it on every host, but so does a dead domain on one host, and the
/// customer workloads that drive this code are full of the latter. The HTTP
/// tier sees the same failure independently and reports `TargetUnreachable`, so
/// a genuine resolver fault is not invisible.
/// A ladder result that is nothing but a wall loses to a usable fallback body.
///
/// While the wall verdict was an error this preference was automatic: the ladder
/// "failed", and the fallback arm substituted the HTTP body whenever that body
/// was not itself a wall. Now that the verdict travels as data the preference
/// has to be stated, or a wall every tier already rejected would displace a page
/// we can actually serve. That is a recall regression, and recall is the one
/// thing this change may not cost.
///
/// Demoting re-enters the existing substitution arm unchanged, so the fallback
/// keeps its warning, its timing and its metric. `may_substitute` is false for a
/// screenshot request or a hard renderer pin, where there is no substitution to
/// prefer and the caller is better served by the verdict itself.
fn demote_wall_to_ladder_failure(
    js: CrwResult<FetchResult>,
    fallback: &FetchResult,
    may_substitute: bool,
) -> CrwResult<FetchResult> {
    match js {
        Ok(r)
            if may_substitute
                && r.block.is_some()
                && !detector::looks_like_generic_bot_wall(&fallback.html, fallback.truncated) =>
        {
            Err(CrwError::RendererError(
                r.block
                    .map(|b| b.reason)
                    .unwrap_or_else(|| "anti-bot wall".to_string()),
            ))
        }
        other => other,
    }
}

/// The verdict for a fallback body that is itself an anti-bot wall.
///
/// Both fallback arms in [`Renderer::fetch`] reach this conclusion about a
/// different variable, and both used to express it as
/// `Err(CrwError::HttpError(..))`, which `crw-server` maps to 502. A target
/// refusing us is not our gateway failing, so the verdict now travels on the
/// `FetchResult` and `crw_crawl::single` stamps it at the shared scrape choke.
///
/// `generic_block` is load-bearing rather than cosmetic: `ScrapeData::http_error()`
/// yields to any NON-structural block, so this vendor is what routes a wall
/// served with a 4xx into the `anti_bot` branch of `routes/scrape.rs` instead of
/// its `http_error` branch.
fn unclearable_wall_verdict() -> crw_core::types::BlockOutcome {
    crw_core::types::BlockOutcome {
        vendor: "generic_block".to_string(),
        reason: "anti-bot wall that no renderer tier could clear".to_string(),
    }
}

/// Did the cloak tier reject a body on CONTENT grounds rather than fail?
///
/// `cloak.rs` answers `Err` for two different things. A transport failure
/// reaching the sidecar is a genuine tier fault and must keep tripping the
/// global breaker. But a body it read and rejected, a still-challenged page or
/// the sidecar's empty fail-open, is a verdict about the page, and a customer
/// batch of dead origins produces a stream of them.
///
/// Without this split, the length floor added to `cloak.rs`'s accept arm would
/// have MOVED the dead-origin case off the `Ok`-but-thin path, which is
/// host-scoped, onto the `Err` path, which was not, and roughly 50 dead hosts
/// would take the cloak tier out globally. That is the exact class this change
/// exists to remove, so it would have been self-defeating.
fn is_cloak_content_verdict(e: &CrwError) -> bool {
    matches!(e, CrwError::RendererError(m)
        if m.starts_with("cloak: still challenged")
            || m.starts_with("cloak: empty or unusably thin body"))
}

fn is_origin_fault_for_breaker(e: &CrwError) -> bool {
    if let CrwError::RendererError(m) = e {
        let u = m.to_ascii_uppercase();
        if u.contains("ERR_TUNNEL_CONNECTION_FAILED")
            || u.contains("ERR_PROXY_CONNECTION_FAILED")
            || u.contains("ERR_NETWORK_CHANGED")
            || u.contains("ERR_INTERNET_DISCONNECTED")
        {
            return false;
        }
    }
    is_origin_navigation_failure(e)
}

/// Prefix of the `warning` set when a JS escalation failed and the HTTP body was
/// returned in its place. Public because it is BOTH the caller-facing
/// explanation and the signal `crw_crawl::single` reads to skip a second
/// escalation round that would re-run a ladder this request already exhausted.
/// A shared constant so the producer and that consumer cannot drift.
pub const JS_ESCALATION_FAILED: &str = "js_escalation_failed:";

/// Soft-block / soft-error status codes where the body often contains real
/// content despite the status header. Sources:
///   - UA/header-based bot filters: 401, 403, 405, 406, 412
///   - Rate limits: 429
///   - Geo gates: 451
///   - Origin overload: 503
///   - "Not found" SPAs that 404 the route but render content via JS
///     hydration: 404, 410
///   - Origin error that still serves a usable page: 500
///
/// Firecrawl-comparison (April 2026 bench): the JS render path recovered
/// content in ~25/99 such cases that HTTP alone could not. Shared by the auto
/// and forced-JS arms of `fetch_inner`, which must agree on what counts as a
/// body worth warning about. Two further copies of this list still live inline
/// in `fetch_with_js` — worth folding in, but not while this change is in
/// flight.
fn is_soft_block_status(status_code: u16) -> bool {
    matches!(
        status_code,
        401 | 403 | 404 | 405 | 406 | 410 | 412 | 429 | 451 | 500 | 503
    )
}

/// Hard-block status set: egress-recoverable blocks only, NOT the softer
/// 404/405/406/410/412/451/500 shapes that often carry real bodies. Single
/// owner for the set the serial ladder derives `saw_hard_block` /
/// `site_blocked` from (previously spelled inline at each site) and the
/// impersonated hop's trigger.
fn is_hard_block_status(status_code: u16) -> bool {
    matches!(status_code, 401 | 403 | 429 | 503) || (520..=530).contains(&status_code)
}

/// Cap on the warm per-proxy fetcher caches (see
/// `cached_per_proxy_fetcher`): bound growth under arbitrary BYOP proxies,
/// where config pools are small but the space of raw proxy URLs is not.
const PROXY_CLIENT_CACHE_CAP: usize = 512;

/// Why the impersonated hop fired; label for logs and the route-decision
/// metric. Kept as an enum (not a bool) so the Failover reason mapping cannot
/// conflate a wall with a transport failure.
#[cfg(feature = "impersonated")]
#[derive(Clone, Copy)]
enum ImpersonatedTrigger {
    /// Body/header wall on the plain HTTP response (the Amazon shape).
    Wall,
    /// The plain HTTP fetch itself failed at the transport layer
    /// (fingerprint-shaped TLS rejection).
    Transport,
}

#[cfg(feature = "impersonated")]
impl ImpersonatedTrigger {
    fn as_str(self) -> &'static str {
        match self {
            ImpersonatedTrigger::Wall => "wall",
            ImpersonatedTrigger::Transport => "transport",
        }
    }
}

/// Minimum remaining request budget for a network attempt to be worth making.
/// Below this a CDP tier cannot complete its handshake and returns a fabricated
/// `Timeout after Nms` (single-digit N) while still consuming a pool slot.
/// Guards the main ladder loop, the hedge dispatch, the breaker leak-through arm,
/// and the HTTP tier's proxy retry (`http_only`).
pub const MIN_TIER_BUDGET: Duration = Duration::from_millis(500);

/// Fresh render budget the `chrome_proxy` auto-egress recovery arm gets when it
/// fires, instead of the shared request deadline. The HTTP/LightPanda/Chrome
/// ladder routinely burns a ~15s scrape deadline down to ~2s before the arm, far
/// below what a residential connect and nav need — so gating the arm on
/// `deadline.remaining()` (any floor) kept it permanently inert on real scrapes
/// (prod-measured: remaining ~2s). The arm's fetch is dispatched with a fresh
/// `Deadline::now_plus(this)` so recovery is actually attempted; only
/// hard-blocked scrapes (which would otherwise fail) pay the extra time, and the
/// SaaS→engine fetch tolerates up to 120s (`crw-client.ts TIMEOUT_MS`).
///
/// Sized from a live measurement (2026-09-08, `www.carfax.ca` behind Azure Front
/// Door): a residential Chrome render of the page takes 12.3 to 16.3 s end to
/// end, so the previous 12_000 ms let the arm finish on 2 of 11 attempts while a
/// pinned `chrome_proxy` with a larger budget cleared the wall 6 of 6 times.
/// 20 s covers the measured worst case with headroom. Only the hard-blocked
/// tail pays for it: the arm never runs on a scrape the ladder already served.
///
/// ponytail: the EFFECTIVE render budget is `min(this, nav_budget)` (cdp.rs
/// `nav_budget = self.nav_budget.min(deadline.remaining())`), which is why the
/// chrome_proxy tier is built with `chrome_nav_budget_ms.max(this)` below.
/// Raising this alone does nothing; raising `chrome_nav_budget_ms` would also
/// slow the direct chrome tier on the hot path, so the max is applied to the
/// proxy tier only.
pub(crate) const CHROME_PROXY_ARM_BUDGET_MS: u64 = 20_000;

// Concurrency permits for the chrome_proxy auto-egress recovery arm are sized to
// `config.chrome_proxy_pool_size()` at construction (see `chrome_proxy_arm_sem`) —
// falls back to `pool_size` when unset, but can be raised independently so a burst
// of hard-blocks doesn't compete with the main chrome/lightpanda tiers' pool. The
// residential chrome_proxy pool blocking-acquires a small (~pool_size) slot set; without a
// non-blocking load-shed a burst of datacenter-blocked URLs (batch/crawl) would
// queue every page on it for up to the SaaS 120s timeout, collapsing throughput
// for co-tenant traffic. The arm `try_acquire_owned`s a permit and skips recovery
// (returns the block) when none is free — best-effort recovery that never queues.

/// Concurrency permits for the cloak recovery arm — sized to the sidecar's
/// cold-solve browser cap (`CLOAK_CONCURRENCY`, default 4) so the engine sheds
/// excess with a fast clean block instead of queuing headed Turnstile solves
/// that hold the request budget. This is the historical default; the effective
/// value is `config.cloak_pool_size()`, so an operator can raise the tier's
/// box-wide throughput ceiling (it is ~0.8 pages/s at 4 permits) together with
/// the sidecar's own concurrency and memory. `pub const` so it never trips
/// `dead_code` in a lean build (referenced only under `#[cfg(feature="cloak")]`).
#[cfg(feature = "cloak")]
pub const CLOAK_SEM_PERMITS: usize = crw_core::config::CLOAK_DEFAULT_POOL_SIZE;

/// Composite renderer that tries multiple backends in order.
pub struct FallbackRenderer {
    http: Arc<dyn PageFetcher>,
    js_renderers: Vec<Arc<dyn PageFetcher>>,
    /// Global default for `render_js` when a request doesn't specify one.
    render_js_default: Option<bool>,
    /// Phase 0 (latency-qn): emit per-fetch structured timing for bench runs.
    latency_breakdown: bool,
    /// Phase 2 (latency-qn): gate chrome_proxy as a hard-block-only recovery arm
    /// (removed from the normal ladder) instead of an always-on tier.
    auto_egress_escalation: bool,
    /// Give the post-ladder cloak CF-recovery arm a fresh, decoupled budget
    /// (`CLOAK_ARM_RECOVER_BUDGET_MS`) instead of skipping it when the shared
    /// deadline is below `CLOAK_ARM_FLOOR_MS`. Unconditional field (like
    /// `auto_egress_escalation`) — inert in a lean (no-`cloak`) build since
    /// `route_to_cloak` folds to `false` there. Default off.
    cloak_recover_on_cf: bool,
    /// Let the cloak arm fire on a Cloudflare challenge the auto-egress arm
    /// uncovered, not only on one the ladder itself saw. See
    /// `RendererConfig::cloak_after_egress`. Default off; inert in a lean
    /// (no-`cloak`) build, where the flag that reads it folds to `false`.
    cloak_after_egress: bool,
    /// latency-qn: conditional hedge — race lightpanda+chrome concurrently.
    chrome_hedge: bool,
    /// Headroom gate for the hedge: bounds concurrent hedges to pool_size/2 so the
    /// 2-contexts-per-request hedge can never deadlock the context pool. Acquired
    /// with `try_acquire` (no permit → serial fallback; blocking would defeat the
    /// latency win).
    hedge_sem: Arc<tokio::sync::Semaphore>,
    /// Load-shed for the chrome_proxy auto-egress recovery arm — non-blocking
    /// `try_acquire_owned` so a burst of datacenter-blocked URLs never queues on
    /// the residential pool (see [`CHROME_PROXY_ARM_BUDGET_MS`]). Sized to
    /// `pool_size` so at most a poolful of arms run and none blocks.
    chrome_proxy_arm_sem: Arc<tokio::sync::Semaphore>,
    /// Is there any tier that could actually clear an IP-reputation block —
    /// residential CDP egress, the stealth tier, or a fallback HTTP proxy?
    ///
    /// Gates the `SiteBlocked` breaker classification. Ignoring a site block in
    /// the failure window only pays off when something downstream can recover
    /// the page. Where nothing can (the common self-host build: no
    /// `chrome_proxy`, no cloak, no `CRW_HTTP_RATELIMIT_PROXY_URL`), every tier
    /// egresses from the same banned IP, so suppressing the breaker would make a
    /// permanently blocked host re-walk the whole serial ladder on every request
    /// — measured ~3-6s/page against ~0.5s once the breaker opens, i.e. a 6-12x
    /// slowdown on a crawl, buying exactly zero recall. There, the breaker keeps
    /// its brake and behaviour is unchanged.
    has_recovery_tier: bool,
    /// Per-host renderer preference learning (auto-mode only).
    preferences: Arc<HostPreferences>,
    /// Per-host + global circuit breakers per renderer.
    breakers: Arc<BreakerRegistry>,
    /// Per-tier configured timeouts (Duration). Used by the breaker layer
    /// for pre-flight deadline-skip and clamp detection in
    /// `AttemptContext::capture`.
    tier_timeouts: std::collections::HashMap<RendererKind, std::time::Duration>,
    /// Process-wide per-eTLD+1 rate (req/sec). `0.0` disables the interval
    /// floor; the concurrency cap below still applies. Configured via
    /// [`Self::with_host_limits`].
    requests_per_second: f64,
    /// Process-wide per-eTLD+1 in-flight cap for batch/crawl. `1` enforces strict
    /// politeness. Interactive gets `per_host_interactive_reserve` extra slots.
    per_host_max_concurrent: u32,
    /// Extra per-host in-flight slots reserved for interactive traffic (the A
    /// reserved lane). Total per-host in-flight is bounded by
    /// `per_host_max_concurrent + this`.
    per_host_interactive_reserve: u32,
    /// Anti-bot classifier policy. Drives the in-loop `classify()` call that
    /// decides whether a 200-status block page is a soft failure (escalate
    /// toward `chrome_proxy`) or a genuine success.
    antibot: crw_core::config::AntibotConfig,
    /// Active proxy rotator. Drives the HTTP fetcher pool and (with the `cdp`
    /// feature) per-request CDP `proxyServer` selection. `None` = no proxy
    /// configured → direct connections, byte-identical to prior behavior.
    proxy_rotator: Option<Arc<crw_core::ProxyRotator>>,
    /// Saved HTTP-fetcher construction inputs so a per-request proxied client
    /// can be built on demand (when `REQUEST_PROXY` is set) without re-picking.
    http_ua: String,
    http_inject_stealth: bool,
    http_timeout_ms: u64,
    /// Warm per-proxy HTTP fetchers keyed by `ProxyEntry::raw()`, so repeated
    /// requests to the same proxy reuse a connection pool instead of rebuilding
    /// a client each time. Bounded — cleared past a cap to avoid unbounded
    /// growth under arbitrary BYOP proxies.
    proxy_client_cache: std::sync::Mutex<std::collections::HashMap<String, Arc<dyn PageFetcher>>>,
    /// Browser-context pool handles for graceful drain on shutdown, one per
    /// pooled CDP tier (`chrome`, `chrome_proxy`). Empty when the pool is
    /// disabled or no CDP tier is configured.
    #[cfg(feature = "cdp")]
    chrome_pools: Vec<Arc<browser_pool::BrowserContextPool<cdp_conn::CdpConnection>>>,
    /// Whether the (constructed) camoufox tier participates in the auto ladder
    /// for this instance's mode. Drives the non-pinned pool filter in
    /// `fetch_with_js`: when false, a configured camoufox renderer is reachable
    /// only by an explicit `renderer = "camoufox"` pin, never the auto chain.
    #[cfg(feature = "camoufox")]
    camoufox_in_auto: bool,
    /// Cloak Turnstile-solver recovery arm, held OUT of the normal ladder and
    /// fired only on a detected CF challenge. `None` when unconfigured. Gated so
    /// a lean build has no such field (byte-identical).
    #[cfg(feature = "cloak")]
    cloak_arm: Option<Arc<dyn PageFetcher>>,
    /// Concurrency shed for the cloak arm — sized to the sidecar's cold-solve
    /// browser cap so the engine returns a fast clean block instead of queuing
    /// headed-Chromium solves. Acquired non-blocking (`try_acquire_owned`).
    #[cfg(feature = "cloak")]
    cloak_sem: Arc<tokio::sync::Semaphore>,
    /// Chrome-impersonating HTTP tier (wreq): the auto-chain hop between the
    /// plain HTTP tier and the browser ladder, and the pin target for
    /// `renderer = "impersonated-http"`. cfg-gated like `cloak_arm` so the
    /// lean build is byte-identical. `None` when the config disables it.
    #[cfg(feature = "impersonated")]
    impersonated: Option<Arc<dyn PageFetcher>>,
    /// Warm per-proxy impersonated clients, mirroring `proxy_client_cache`
    /// (same 512 bound): a set REQUEST_PROXY is honored by the hop and the pin
    /// alike, never bypassed. wreq reads no env proxies implicitly, so nothing
    /// implicit is wired either way.
    #[cfg(feature = "impersonated")]
    impersonated_client_cache:
        std::sync::Mutex<std::collections::HashMap<String, Arc<dyn PageFetcher>>>,
    /// Saved impersonated-tier timeout for the per-proxy cache.
    #[cfg(feature = "impersonated")]
    impersonated_timeout_ms: u64,
}

impl std::fmt::Debug for FallbackRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FallbackRenderer")
            .field("http", &self.http.name())
            .field(
                "js_renderers",
                &self
                    .js_renderers
                    .iter()
                    .map(|r| r.name())
                    .collect::<Vec<_>>(),
            )
            .field("render_js_default", &self.render_js_default)
            .finish()
    }
}

/// How much of the request deadline the camoufox challenge poll must leave for
/// the cloak recovery arm, or `None` when that arm cannot fire after it.
///
/// The arm exists whenever a cloak endpoint is configured (it is a recovery
/// arm, not a ladder tier, so its own `include_in_auto` does not decide this),
/// and it is gated on `CLOAK_ARM_FLOOR_MS` of remaining deadline only while
/// `cloak_recover_on_cf` is off. With that flag on it fires on its own fresh
/// budget, so reserving for it would only shorten the poll for nothing.
///
/// It also only fires when an earlier tier left a Cloudflare challenge body
/// behind, and camoufox reports a challenge as an error, never as a body. So
/// the reserve pays off in exactly one shape: `mode = "auto"` with camoufox in
/// the ladder, where chrome ran first. A camoufox pin or `mode = "camoufox"`
/// runs nothing before it, the arm is unreachable, and holding time back would
/// only cut the poll short. A pin issued on a deployment that also has
/// camoufox in auto still carries the reserve; that is a per-instance ceiling
/// worth knowing, not worth plumbing.
///
/// The floor is what the arm checks AFTER this tier has returned, so the
/// reserve also covers the last poll evaluate and the awaited session
/// teardown that sit between the end of the poll and that check. The tier
/// honours the reserve only while more than the reserve remains; with less,
/// it prefers its own poll to an arm that may not fire.
#[cfg(all(feature = "camoufox", feature = "cloak"))]
fn camoufox_challenge_reserve_ms(config: &RendererConfig) -> Option<u64> {
    let cloak_configured = config
        .cloak
        .as_ref()
        .is_some_and(|c| !c.base_url.trim().is_empty());
    let camoufox_after_a_ladder =
        matches!(config.mode, RendererMode::Auto) && config.camoufox_in_ladder();
    (camoufox_after_a_ladder && cloak_configured && !config.cloak_recover_on_cf)
        .then_some(crw_core::config::CLOAK_ARM_FLOOR_MS + camoufox::POLL_HANDOFF_MS)
}

impl FallbackRenderer {
    pub fn new(
        config: &RendererConfig,
        user_agent: &str,
        proxy: Option<&str>,
        stealth: &StealthConfig,
    ) -> CrwResult<Self> {
        let effective_ua = pick_ua(user_agent, stealth);
        let inject_headers = stealth.enabled && stealth.inject_headers;
        let http_timeout_ms = config.http_timeout();
        // Fail closed: a malformed single `proxy` (e.g. CLI `--proxy htp://...`)
        // is a hard error, never a silent direct connection (real-IP leak).
        if let Some(p) = proxy {
            crw_core::ProxyEntry::parse(p).map_err(CrwError::ConfigError)?;
        }
        let http_concrete = http_only::HttpFetcher::with_timeout(
            &effective_ua,
            proxy,
            inject_headers,
            std::time::Duration::from_millis(http_timeout_ms),
        );
        // Read off the CONCRETE fetcher: once coerced to `Arc<dyn PageFetcher>` the
        // proxy-availability question is no longer askable, and asking the env var
        // instead would call a malformed URL a working recovery egress.
        let http_fallback_proxy_ready = http_concrete.has_ratelimit_proxy();
        let http = Arc::new(http_concrete) as Arc<dyn PageFetcher>;

        // Chrome-impersonation HTTP tier (wreq). Constructed BEFORE the
        // mode=none early return below: it is an HTTP tier, so `mode = "none"`
        // ("no JS") keeps it; only the `impersonated.enabled = false` kill
        // switch or a feature-less build leaves it out. Build failure is a
        // hard error, never a silent fall-through to a non-impersonating
        // client (see `ImpersonatedFetcher::new`).
        #[cfg(feature = "impersonated")]
        let impersonated: Option<Arc<dyn PageFetcher>> = if config.impersonated_in_chain() {
            let tier = impersonated::ImpersonatedFetcher::new(std::time::Duration::from_millis(
                config.impersonated_timeout(),
            ))?;
            tracing::info!(
                timeout_ms = config.impersonated_timeout(),
                "impersonated-http tier enabled"
            );
            Some(Arc::new(tier) as Arc<dyn PageFetcher>)
        } else {
            None
        };

        // A pinned backend (Lightpanda/Chrome/Playwright) must have CDP compiled in
        // AND its matching endpoint configured. `Auto` and `None` remain functional
        // without CDP — they just won't spawn any JS renderer.
        #[cfg(not(feature = "cdp"))]
        if matches!(
            config.mode,
            RendererMode::Lightpanda | RendererMode::Chrome | RendererMode::Playwright
        ) {
            return Err(CrwError::ConfigError(format!(
                "renderer.mode = {:?} requires the 'cdp' feature, but this build was \
                 compiled without it. Rebuild with --features cdp or set mode = \"auto\"/\"none\".",
                config.mode
            )));
        }

        // Camoufox is REST, not CDP — it requires the `camoufox` feature
        // independently of `cdp`. Separate top-level guard (never nested in the
        // cdp block above) so a camoufox-less build rejects the pin cleanly.
        #[cfg(not(feature = "camoufox"))]
        if matches!(config.mode, RendererMode::Camoufox) {
            return Err(CrwError::ConfigError(
                "renderer.mode = \"camoufox\" requires the 'camoufox' feature, but this build \
                 was compiled without it. Rebuild with --features camoufox or set mode = \
                 \"auto\"/\"none\"."
                    .into(),
            ));
        }

        #[cfg(not(feature = "cloak"))]
        if matches!(config.mode, RendererMode::Cloak) {
            return Err(CrwError::ConfigError(
                "renderer.mode = \"cloak\" requires the 'cloak' feature, but this build \
                 was compiled without it. Rebuild with --features cloak or set mode = \
                 \"auto\"/\"none\"."
                    .into(),
            ));
        }

        #[allow(unused_mut)]
        let mut js_renderers: Vec<Arc<dyn PageFetcher>> = Vec::new();

        if matches!(config.mode, RendererMode::None) {
            if config.render_js_default == Some(true) {
                tracing::warn!(
                    "render_js_default=true has no effect with mode=none; \
                     requests will fall back to HTTP via http_only_fallback"
                );
            }
            return Ok(Self {
                http,
                js_renderers,
                render_js_default: config.render_js_default,
                latency_breakdown: config.latency_breakdown,
                auto_egress_escalation: config.auto_egress_escalation,
                cloak_recover_on_cf: config.cloak_recover_on_cf,
                cloak_after_egress: config.cloak_after_egress,
                chrome_hedge: config.chrome_hedge,
                hedge_sem: Arc::new(tokio::sync::Semaphore::new((config.pool_size / 2).max(1))),
                chrome_proxy_arm_sem: Arc::new(tokio::sync::Semaphore::new(
                    config.chrome_proxy_pool_size(),
                )),
                // `mode = none` builds no JS tier at all, so the only possible
                // recovery is the HTTP fallback proxy.
                has_recovery_tier: http_fallback_proxy_ready,
                preferences: Arc::new(HostPreferences::with_defaults()),
                breakers: Arc::new(BreakerRegistry::with_defaults()),
                tier_timeouts: tier_timeouts_from(config),
                requests_per_second: 0.0,
                per_host_max_concurrent: 1,
                per_host_interactive_reserve: 1,
                antibot: config.antibot.clone(),
                proxy_rotator: None,
                http_ua: effective_ua.clone(),
                http_inject_stealth: inject_headers,
                http_timeout_ms,
                proxy_client_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
                #[cfg(feature = "cdp")]
                chrome_pools: Vec::new(),
                // mode=none constructs no renderers, so camoufox is never in
                // the (empty) ladder.
                #[cfg(feature = "camoufox")]
                camoufox_in_auto: false,
                // mode=none never recovers — no cloak arm.
                #[cfg(feature = "cloak")]
                cloak_arm: None,
                #[cfg(feature = "cloak")]
                cloak_sem: Arc::new(tokio::sync::Semaphore::new(CLOAK_SEM_PERMITS)),
                #[cfg(feature = "impersonated")]
                impersonated,
                #[cfg(feature = "impersonated")]
                impersonated_client_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
                #[cfg(feature = "impersonated")]
                impersonated_timeout_ms: config.impersonated_timeout(),
            });
        }

        #[cfg(feature = "cdp")]
        let mut chrome_pools: Vec<
            Arc<browser_pool::BrowserContextPool<cdp_conn::CdpConnection>>,
        > = Vec::new();

        #[cfg(feature = "cdp")]
        {
            let want = |m: RendererMode| -> bool {
                matches!(config.mode, RendererMode::Auto) || config.mode == m
            };

            if want(RendererMode::Lightpanda) {
                if let Some(lp) = &config.lightpanda {
                    js_renderers.push(Arc::new(
                        cdp::CdpRenderer::new(
                            "lightpanda",
                            &lp.ws_url,
                            config.lightpanda_timeout(),
                            config.pool_size,
                        )
                        .with_user_agent(&effective_ua),
                    ));
                } else if matches!(config.mode, RendererMode::Lightpanda) {
                    return Err(CrwError::ConfigError(
                        "renderer.mode = \"lightpanda\" but [renderer.lightpanda] ws_url is not \
                         configured"
                            .into(),
                    ));
                }
            }
            if want(RendererMode::Playwright) {
                if let Some(pw) = &config.playwright {
                    // Playwright is treated as a "chrome-equivalent" tier —
                    // same timeout budget, same kind of work.
                    js_renderers.push(Arc::new(
                        cdp::CdpRenderer::new(
                            "playwright",
                            &pw.ws_url,
                            config.chrome_timeout(),
                            config.pool_size,
                        )
                        .with_user_agent(&effective_ua),
                    ));
                } else if matches!(config.mode, RendererMode::Playwright) {
                    return Err(CrwError::ConfigError(
                        "renderer.mode = \"playwright\" but [renderer.playwright] ws_url is not \
                         configured"
                            .into(),
                    ));
                }
            }
            if want(RendererMode::Chrome) {
                if let Some(ch) = &config.chrome {
                    let blocklist = blocklist::Blocklist::defaults()
                        .with_stylesheets(config.chrome_intercept_stylesheets);
                    let mut renderer = cdp::CdpRenderer::new(
                        "chrome",
                        &ch.ws_url,
                        config.chrome_timeout(),
                        config.pool_size,
                    )
                    .with_user_agent(&effective_ua)
                    .with_nav_budget(config.chrome_nav_budget_ms)
                    .with_challenge_retries(
                        config
                            .chrome_challenge_max_retries
                            .unwrap_or(cdp::CHALLENGE_MAX_RETRIES),
                    )
                    .with_spa_selector_max(
                        config
                            .chrome_spa_selector_max_ms
                            .unwrap_or(cdp::SPA_SELECTOR_MAX_MS),
                    )
                    .with_fast_ready(config.chrome_fast_ready)
                    .with_interception(
                        config.chrome_intercept_resources,
                        blocklist,
                        config.chrome_host_intercept_disable.clone(),
                    );

                    renderer = maybe_with_pool(renderer, config, "chrome");
                    chrome_pools.extend(renderer.pool());
                    js_renderers.push(Arc::new(renderer));
                } else if matches!(config.mode, RendererMode::Chrome) {
                    return Err(CrwError::ConfigError(
                        "renderer.mode = \"chrome\" but [renderer.chrome] ws_url is not configured"
                            .into(),
                    ));
                }
                // Residential-proxy Chrome tier: opt-in 4th renderer. Pushed
                // after `chrome` so the existing in-request fallback loop
                // (`for renderer in renderers` in fetch_with_js) tries Chrome
                // direct first and falls through to chrome_proxy on failure.
                // Skipped when [renderer.chrome_proxy] is unset OR when
                // `ws_url` is empty (docker-compose passes empty env vars
                // even when --profile proxy is inactive).
                if let Some(cp) = config
                    .chrome_proxy
                    .as_ref()
                    .filter(|c| !c.ws_url.trim().is_empty())
                {
                    let blocklist = blocklist::Blocklist::defaults()
                        .with_stylesheets(config.chrome_intercept_stylesheets);
                    let mut renderer = cdp::CdpRenderer::new(
                        "chrome_proxy",
                        &cp.ws_url,
                        config.chrome_proxy_timeout(),
                        config.chrome_proxy_pool_size(),
                    )
                    .with_user_agent(&effective_ua)
                    .with_nav_budget(config.chrome_nav_budget_ms.max(CHROME_PROXY_ARM_BUDGET_MS))
                    .with_challenge_retries(
                        config
                            .chrome_challenge_max_retries
                            .unwrap_or(cdp::CHALLENGE_MAX_RETRIES),
                    )
                    .with_spa_selector_max(
                        config
                            .chrome_spa_selector_max_ms
                            .unwrap_or(cdp::SPA_SELECTOR_MAX_MS),
                    )
                    .with_fast_ready(config.chrome_fast_ready)
                    .with_interception(
                        config.chrome_intercept_resources,
                        blocklist,
                        config.chrome_host_intercept_disable.clone(),
                    );
                    // Wire DataImpulse base creds when configured. The renderer
                    // composes `{base_user}__cr.{country}` per request and replies
                    // to Chrome's `Fetch.authRequired` via CDP — replacing the
                    // removed gost forwarder.
                    if let (Some(u), Some(p)) = (&config.proxy_base_user, &config.proxy_base_pass) {
                        renderer = renderer.with_proxy_auth_base(
                            u.clone(),
                            p.clone(),
                            config.proxy_default_country.clone(),
                        );
                    }
                    tracing::info!(
                        ws_url = %cp.ws_url,
                        proxy_auth = config.proxy_base_user.is_some(),
                        default_country = ?config.proxy_default_country,
                        "chrome_proxy tier enabled"
                    );
                    // Same browser-context pool as the chrome tier. Without it
                    // this tier takes the legacy path, and because its egress
                    // proxy comes from the container's own `--proxy-server`
                    // (not a per-request one), it never builds a browser
                    // context — so its targets land in the *default* context,
                    // which cannot be disposed and therefore cannot be reaped
                    // when a render is cancelled. Prod, 2026-07-29: chrome held
                    // 0 stray targets while chrome_proxy held 42.
                    renderer = maybe_with_pool(renderer, config, "chrome_proxy");
                    chrome_pools.extend(renderer.pool());
                    js_renderers.push(Arc::new(renderer));
                }
            }
        }

        // Camoufox REST tier — a TOP-LEVEL block, NOT nested in the cdp guard
        // above (camoufox is REST, not CDP). The renderer is constructed
        // whenever an endpoint is configured, so an explicit per-request
        // `renderer = "camoufox"` pin can always reach it. Whether it
        // participates in the *auto* (non-pinned) chain is decided at request
        // time in `fetch_with_js` via `camoufox_in_auto` — a configured
        // endpoint with `include_in_auto = false` stays out of the auto ladder.
        #[cfg(feature = "camoufox")]
        {
            if let Some(cf) = config
                .camoufox
                .as_ref()
                .filter(|c| !c.base_url.trim().is_empty())
            {
                let camoufox_tier = camoufox::CamoufoxRenderer::new(
                    "camoufox",
                    &cf.base_url,
                    &cf.api_key,
                    config.camoufox_timeout(),
                )
                .with_challenge_wait(config.camoufox_challenge_wait());
                #[cfg(feature = "cloak")]
                let camoufox_tier = match camoufox_challenge_reserve_ms(config) {
                    Some(reserve) => camoufox_tier.with_challenge_reserve(reserve),
                    None => camoufox_tier,
                };
                js_renderers.push(Arc::new(camoufox_tier) as Arc<dyn PageFetcher>);
                tracing::info!(
                    base_url = %cf.base_url,
                    include_in_auto = cf.include_in_auto,
                    "camoufox tier enabled"
                );
            } else if matches!(config.mode, RendererMode::Camoufox) {
                return Err(CrwError::ConfigError(
                    "renderer.mode = \"camoufox\" but [renderer.camoufox] base_url is not configured"
                        .into(),
                ));
            }
        }

        // Cloak Turnstile-solver recovery tier — held OUT of `js_renderers` (it
        // is a CF-challenge recovery arm, not a ladder tier). Constructed with
        // the DataImpulse base creds + default country threaded from config, so
        // it can mint per-request sticky-sessid proxy URLs itself.
        #[cfg(feature = "cloak")]
        let cloak_arm: Option<Arc<dyn PageFetcher>> = {
            if let Some(ck) = config
                .cloak
                .as_ref()
                .filter(|c| !c.base_url.trim().is_empty())
            {
                let proxy_base = config
                    .proxy_base_user
                    .clone()
                    .zip(config.proxy_base_pass.clone());
                tracing::info!(
                    base_url = %ck.base_url,
                    proxy_auth = proxy_base.is_some(),
                    "cloak recovery tier enabled"
                );
                Some(Arc::new(cloak::CloakRenderer::new(
                    "cloak",
                    &ck.base_url,
                    &ck.api_key,
                    config.cloak_timeout(),
                    proxy_base,
                    config.proxy_default_country.clone(),
                    config.cloak_proxy_host.clone(),
                )) as Arc<dyn PageFetcher>)
            } else if matches!(config.mode, RendererMode::Cloak) {
                return Err(CrwError::ConfigError(
                    "renderer.mode = \"cloak\" but [renderer.cloak] base_url is not configured"
                        .into(),
                ));
            } else {
                None
            }
        };

        // Spawn the process-wide CDP telemetry sampler. Idempotent —
        // OnceLock guarantees a single task across all FallbackRenderer
        // instances. No-op on the `mode = none` early-return path above.
        #[cfg(feature = "cdp")]
        health_telemetry::spawn_once();

        if config.render_js_default == Some(true) && js_renderers.is_empty() {
            tracing::warn!(
                "render_js_default=true but no JS renderer is available; \
                 requests will fall back to HTTP via http_only_fallback"
            );
        }

        // The cloak arm is feature-gated and lives outside `js_renderers`, so its
        // presence has to be reduced to a bool here — a lean build has no such
        // binding at all.
        #[cfg(feature = "cloak")]
        let cloak_arm_present = cloak_arm.is_some();
        #[cfg(not(feature = "cloak"))]
        let cloak_arm_present = false;
        let recovery_tier_available = has_recovery_tier(
            config,
            &js_renderers,
            cloak_arm_present,
            http_fallback_proxy_ready,
        );
        Ok(Self {
            http,
            js_renderers,
            render_js_default: config.render_js_default,
            latency_breakdown: config.latency_breakdown,
            auto_egress_escalation: config.auto_egress_escalation,
            cloak_recover_on_cf: config.cloak_recover_on_cf,
            cloak_after_egress: config.cloak_after_egress,
            chrome_hedge: config.chrome_hedge,
            hedge_sem: Arc::new(tokio::sync::Semaphore::new((config.pool_size / 2).max(1))),
            chrome_proxy_arm_sem: Arc::new(tokio::sync::Semaphore::new(
                config.chrome_proxy_pool_size(),
            )),
            has_recovery_tier: recovery_tier_available,
            preferences: Arc::new(HostPreferences::with_defaults()),
            breakers: Arc::new(BreakerRegistry::with_defaults()),
            tier_timeouts: tier_timeouts_from(config),
            requests_per_second: 0.0,
            per_host_max_concurrent: 1,
            per_host_interactive_reserve: 1,
            antibot: config.antibot.clone(),
            proxy_rotator: None,
            http_ua: effective_ua.clone(),
            http_inject_stealth: inject_headers,
            http_timeout_ms,
            proxy_client_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            #[cfg(feature = "cdp")]
            chrome_pools,
            // Single source of truth for the opt-in policy: true only when
            // mode=camoufox (pinned) or mode=auto + include_in_auto. A
            // configured-but-not-opted-in endpoint stays out of the auto chain.
            #[cfg(feature = "camoufox")]
            camoufox_in_auto: config.camoufox_in_ladder(),
            #[cfg(feature = "cloak")]
            cloak_arm,
            #[cfg(feature = "cloak")]
            cloak_sem: Arc::new(tokio::sync::Semaphore::new(config.cloak_pool_size())),
            #[cfg(feature = "impersonated")]
            impersonated,
            #[cfg(feature = "impersonated")]
            impersonated_client_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            #[cfg(feature = "impersonated")]
            impersonated_timeout_ms: config.impersonated_timeout(),
        })
    }

    /// Attach the config proxy rotator. Retained so scrape/crawl entry points
    /// can resolve a per-request proxy (via [`Self::pick_proxy_for_url`]) into
    /// the [`REQUEST_PROXY`] task-local; the HTTP and CDP paths then both consume
    /// that single resolved entry — no second pick. `None` is a no-op. Builder
    /// style so `new()`'s signature stays stable.
    pub fn with_proxy_rotator(
        mut self,
        rotator: Option<Arc<crw_core::ProxyRotator>>,
    ) -> CrwResult<Self> {
        self.proxy_rotator = rotator;
        Ok(self)
    }

    /// The HTTP fetcher to use for the current request. When `REQUEST_PROXY` is
    /// set (resolved once by the caller, honoring BYOP > config precedence),
    /// build a client bound to THAT exact proxy so the HTTP path egresses
    /// through the same proxy the CDP path uses. Hard-fails on a bad proxy
    /// (never a silent direct connection). When unset, use the shared
    /// (no-proxy or single-proxy) fetcher from `new()`.
    fn http_fetcher_for_request(&self) -> CrwResult<Arc<dyn PageFetcher>> {
        let Some(entry) = REQUEST_PROXY.try_with(|p| p.clone()).ok().flatten() else {
            return Ok(self.http.clone());
        };
        Self::cached_per_proxy_fetcher(&self.proxy_client_cache, &entry, || {
            Ok(Arc::new(http_only::HttpFetcher::with_proxy(
                &self.http_ua,
                entry.raw(),
                self.http_inject_stealth,
                std::time::Duration::from_millis(self.http_timeout_ms),
            )?) as Arc<dyn PageFetcher>)
        })
    }

    /// Warm per-proxy fetcher cache shared by the two HTTP-shaped tiers: reuse
    /// a client previously bound to the same `REQUEST_PROXY` entry, else build
    /// one via `build` (fail-closed) and remember it. The build runs OUTSIDE
    /// the lock; the insert re-locks, so concurrent misses may build twice and
    /// last-write-wins, which is harmless for a connection-pool cache.
    fn cached_per_proxy_fetcher(
        cache: &std::sync::Mutex<std::collections::HashMap<String, Arc<dyn PageFetcher>>>,
        entry: &crw_core::ProxyEntry,
        build: impl FnOnce() -> CrwResult<Arc<dyn PageFetcher>>,
    ) -> CrwResult<Arc<dyn PageFetcher>> {
        if let Some(f) = cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(entry.raw())
            .cloned()
        {
            return Ok(f);
        }
        let fetcher = build()?;
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        // Bound growth under arbitrary BYOP proxies (config pools are small).
        if cache.len() >= PROXY_CLIENT_CACHE_CAP {
            cache.clear();
        }
        cache.insert(entry.raw().to_string(), fetcher.clone());
        Ok(fetcher)
    }

    /// REQUEST_PROXY-aware impersonated fetcher lookup, mirroring
    /// [`Self::http_fetcher_for_request`]: shared tier when no proxy is set,
    /// warm per-proxy client built with `ImpersonatedFetcher::with_proxy`
    /// (fail-closed) when one is. When no REQUEST_PROXY is set the shared
    /// client falls back to wreq's system-proxy matcher, which reads
    /// `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY` exactly as reqwest
    /// does for the plain tier. That parity is deliberate: a self-host behind
    /// a corporate proxy must not have this one tier bypass it.
    #[cfg(feature = "impersonated")]
    fn impersonated_fetcher_for_request(&self) -> CrwResult<Arc<dyn PageFetcher>> {
        // Tier-present check FIRST, before the proxy branch: a config-disabled
        // tier must stay disabled even when REQUEST_PROXY is set, or the pin
        // would build a per-proxy client and fetch past the kill switch.
        let Some(shared) = self.impersonated.as_ref() else {
            return Err(CrwError::RendererError(
                "the impersonated-http tier is not available in this build or config".into(),
            ));
        };
        let Some(entry) = REQUEST_PROXY.try_with(|p| p.clone()).ok().flatten() else {
            return Ok(shared.clone());
        };
        Self::cached_per_proxy_fetcher(&self.impersonated_client_cache, &entry, || {
            Ok(Arc::new(impersonated::ImpersonatedFetcher::with_proxy(
                entry.raw(),
                std::time::Duration::from_millis(self.impersonated_timeout_ms),
            )?) as Arc<dyn PageFetcher>)
        })
    }

    /// Did the impersonated tier come back holding a WALL? This is the pin's
    /// verdict, and the first half of the hop's.
    ///
    /// NOT `classify_js_attempt(..).acceptable`: that predicate also fails on
    /// a body under [`Self::MIN_RENDERED_TEXT_LEN`] and on the soft
    /// 404/410/500 statuses, so a pinned crawl would report every dead link as
    /// "the response carries an anti-bot block", a false verdict on the wire.
    ///
    /// Nor the full `hard_block`: that includes
    /// [`crw_extract::antibot::AntibotSignal::StructuralFailure`], which means
    /// "thin or empty body", not "a vendor said no". It fires on every PDF
    /// (this arm runs ahead of both PDF short-circuits, so `html` is empty by
    /// construction) and on any sparse page. A hard BROWSER pin does not error
    /// on those either: it returns the body with an actionable hint and errors
    /// only on a real wall. This mirrors that contract, so the two pin kinds
    /// answer the same shape the same way.
    #[cfg(feature = "impersonated")]
    fn impersonation_blocked(&self, result: &FetchResult) -> bool {
        use crw_extract::antibot::AntibotSignal;
        if matches!(
            result.warning.as_deref(),
            Some("cloudflare_mitigated") | Some("waf_challenge")
        ) {
            return true;
        }
        let class = self.classify_js_attempt(result);
        is_hard_block_status(result.status_code)
            || class.is_bot_wall
            || class.vendor_block.is_some()
            || detector::looks_like_cloudflare_challenge(&result.html)
            || (class.antibot.signal.is_blocked()
                && class.antibot.signal != AntibotSignal::StructuralFailure)
    }

    /// Accept predicate for the AUTO-CHAIN hop: may this response end the
    /// chain here, with no browser tier ever running?
    ///
    /// Stricter than [`Self::impersonation_blocked`] on purpose, and stricter
    /// than `acceptable` alone. `acceptable` was calibrated for POST-render
    /// output from a browser tier; the hop feeds it PRE-render HTML, so on its
    /// own it accepts any body with 50+ characters of visible text. A
    /// TLS-gated SPA answers the impersonated client with its real server
    /// HTML, which is a shell plus a nav and a cookie banner: comfortably past
    /// 50 characters, carrying none of the content the page is about. Today
    /// that URL reaches Chrome (the plain tier's wall triggers the ladder);
    /// accepting it here would end the chain on a shell and bill it as a
    /// success.
    ///
    /// So the hop additionally applies the auto arm's OWN JS-escalation
    /// triggers to the hop body: if `needs_js_rendering` or the
    /// thin-plus-executable-JS pair would have escalated this HTML, the hop is
    /// a miss and the ladder runs exactly as it does today. The hop can
    /// therefore only ever end a chain the ladder would also have ended.
    #[cfg(feature = "impersonated")]
    fn impersonation_accepted(&self, result: &FetchResult) -> bool {
        if self.impersonation_blocked(result) || !self.classify_js_attempt(result).acceptable {
            return false;
        }
        !detector::needs_js_rendering(&result.html)
            && !(detector::looks_like_thin_html(&result.html)
                && detector::warrants_browser_retry(&result.html))
    }

    /// `antibot::classify` behind the configured gate: the real classifier
    /// when `[renderer.antibot]` is enabled, `none()` otherwise, so callers do
    /// not each spell the conditional.
    fn antibot_result(&self, status_code: u16, html: &str) -> crw_extract::antibot::AntibotResult {
        if self.antibot.enabled {
            crw_extract::antibot::classify(Some(status_code), html)
        } else {
            crw_extract::antibot::AntibotResult::none()
        }
    }

    /// One Chrome-impersonated retry between the plain-HTTP tier and the
    /// browser ladder. The caller fires it ONLY on wall-shaped triggers
    /// (block body/headers or a hard status) or a transport error, never on
    /// SPA/thin/empty shapes, and never when `is_fingerprint_vendor_wall`
    /// says the wall needs JS. Returns the accepted result, or `None` to
    /// continue exactly as today (ladder, then today's fallbacks).
    #[cfg(feature = "impersonated")]
    async fn try_impersonated_hop(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        deadline: crw_core::Deadline,
        trigger: ImpersonatedTrigger,
    ) -> Option<FetchResult> {
        self.impersonated.as_ref()?;
        // Budget split, mirroring the cloak-first arm's
        // `CLOAK_FIRST_LADDER_RESERVE_MS`: the hop runs BEFORE the ladder on
        // the SHARED request deadline, so without a cap a slow or futile hop
        // (a tarpitting wall host is exactly this tier's population) drains
        // the budget the browser tiers needed and turns a request that
        // succeeds today into a Timeout. That is a scrape-success debit, and
        // the success line is not negotiable. Half is the split: the hop is
        // one HTTP request against the ladder's several, so half is generous
        // to the hop and still leaves the ladder a real attempt.
        //
        // A reserve is only owed when there IS a ladder to protect. An
        // HTTP-only deployment has nothing to fall through to, so it hands
        // the hop the whole remaining budget (this is the deployment the
        // tier helps most: it gets the wall fix with zero browsers).
        let remaining = deadline.remaining();
        if remaining < MIN_TIER_BUDGET {
            return None;
        }
        let deadline = if self.js_renderers.is_empty() {
            deadline
        } else {
            let share = remaining / 2;
            if share < MIN_TIER_BUDGET {
                return None;
            }
            crw_core::Deadline::now_plus(share)
        };
        // Every metric site in this function shares this label source, so a
        // typo'd literal cannot fork the series.
        let kind = RendererKind::ImpersonatedHttp;
        let fetcher = match self.impersonated_fetcher_for_request() {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    url,
                    trigger = trigger.as_str(),
                    "impersonated hop skipped: {e}"
                );
                metrics()
                    .render_route_decision_total
                    .with_label_values(&[kind.as_str(), "error"])
                    .inc();
                return None;
            }
        };
        match fetcher.fetch(url, headers, None, deadline).await {
            Ok(mut r) => {
                if self.impersonation_accepted(&r) {
                    r.credit_cost = credit_for(kind);
                    r.render_decision = Some(RenderDecision::Failover {
                        chain: vec![RendererKind::Http, kind],
                        reason: match trigger {
                            // Wall-shaped trigger maps to VendorBlock, the
                            // ladder's convention for an anti-bot verdict;
                            // `counts_for_promotion()` correctly excludes it.
                            ImpersonatedTrigger::Wall => FailoverErrorKind::VendorBlock,
                            ImpersonatedTrigger::Transport => FailoverErrorKind::NetworkError,
                        },
                    });
                    metrics()
                        .render_route_decision_total
                        .with_label_values(&[kind.as_str(), "success"])
                        .inc();
                    Some(r)
                } else {
                    tracing::info!(
                        url,
                        trigger = trigger.as_str(),
                        status_code = r.status_code,
                        "impersonated hop still blocked; continuing to the ladder"
                    );
                    metrics()
                        .render_route_decision_total
                        .with_label_values(&[kind.as_str(), "blocked"])
                        .inc();
                    None
                }
            }
            Err(e) => {
                tracing::info!(
                    url,
                    trigger = trigger.as_str(),
                    "impersonated hop failed: {e}"
                );
                metrics()
                    .render_route_decision_total
                    .with_label_values(&[kind.as_str(), "error"])
                    .inc();
                None
            }
        }
    }

    /// Pick a proxy from the configured rotator for `host` (honoring the
    /// rotation strategy). `None` when no proxy is configured. Scrape/crawl
    /// entry points call this and scope the result into the [`REQUEST_PROXY`]
    /// task-local so the CDP/JS path egresses through the chosen proxy.
    pub fn pick_proxy(&self, host: Option<&str>) -> Option<Arc<crw_core::ProxyEntry>> {
        self.proxy_rotator
            .as_ref()
            .map(|r| Arc::new(r.pick(host).clone()))
    }

    /// True when a JS renderer (chrome / lightpanda / chrome_proxy) is wired in,
    /// so a `render_js` request can actually execute a page. The sitemap
    /// escalation arm uses this to skip pointless re-fetches of a challenged
    /// sitemap when no renderer could clear the wall anyway.
    pub fn js_capable(&self) -> bool {
        !self.js_renderers.is_empty() || self.auto_egress_escalation
    }

    /// Like [`Self::pick_proxy`] but derives the host key from a URL using the
    /// same normalization the HTTP fetcher and host limiter use — so the CDP
    /// `proxyServer` and the HTTP client land on the SAME sticky proxy.
    pub fn pick_proxy_for_url(&self, url: &str) -> Option<Arc<crw_core::ProxyEntry>> {
        self.proxy_rotator.as_ref()?;
        let host = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(crate::preference::normalize_host));
        self.pick_proxy(host.as_deref())
    }

    /// Drain every chrome browser-context pool. Idempotent and a no-op when
    /// the pool is disabled. Call from the server's SIGTERM handler after
    /// the HTTP server has finished serving in-flight requests.
    #[cfg(feature = "cdp")]
    pub async fn shutdown_chrome_pool(&self, drain: std::time::Duration) {
        for pool in self.chrome_pools.clone() {
            tracing::info!(
                drain_secs = drain.as_secs(),
                "draining chrome browser-context pool"
            );
            pool.shutdown(drain).await;
        }
    }

    /// No-op when the `cdp` feature is disabled — keeps caller code simple.
    #[cfg(not(feature = "cdp"))]
    pub async fn shutdown_chrome_pool(&self, _drain: std::time::Duration) {}

    /// Configure the process-wide per-host limiter (eTLD+1 keyed). Call once
    /// at startup with values from `CrawlerConfig`. Defaults: rps=0.0 (no
    /// interval floor), per-host cap=1 (strict politeness).
    pub fn with_host_limits(
        mut self,
        requests_per_second: f64,
        per_host_max_concurrent: u32,
        per_host_interactive_reserve: u32,
    ) -> Self {
        self.requests_per_second = requests_per_second;
        self.per_host_max_concurrent = per_host_max_concurrent;
        self.per_host_interactive_reserve = per_host_interactive_reserve;
        self
    }

    /// Access the host preferences cache (for admin endpoints, tests).
    pub fn preferences(&self) -> Arc<HostPreferences> {
        Arc::clone(&self.preferences)
    }

    /// Access the breaker registry (for tests).
    pub fn breakers(&self) -> Arc<BreakerRegistry> {
        Arc::clone(&self.breakers)
    }

    /// Names of the configured JS renderers in fallback order.
    /// Used for startup logs and tests — does not leak internal types.
    /// Is some tier configured that could actually clear an IP-reputation block?
    /// Exposed for the integration test that pins the self-host trade-off; see
    /// the field docs.
    pub fn has_recovery_tier(&self) -> bool {
        self.has_recovery_tier
    }

    pub fn js_renderer_names(&self) -> Vec<&str> {
        self.js_renderers.iter().map(|r| r.name()).collect()
    }

    /// Does the impersonated tier exist in THIS instance (feature + config)?
    /// The pin-validation surfaces ask this; `js_renderer_names()` cannot
    /// answer it, because the tier lives outside `js_renderers` by design.
    /// Const false in a lean build.
    pub fn has_impersonated_tier(&self) -> bool {
        #[cfg(feature = "impersonated")]
        {
            self.impersonated.is_some()
        }
        #[cfg(not(feature = "impersonated"))]
        {
            false
        }
    }

    /// The renderer names a request may pin on this instance: the JS ladder
    /// plus, when present, the impersonated HTTP tier prepended. The single
    /// vocabulary both pin-validation surfaces (crw-server `state.rs`,
    /// crw-crawl `single.rs`) check against, so their 400 messages list the
    /// real capability set uniformly instead of a special-case branch.
    pub fn available_renderer_names(&self) -> Vec<&str> {
        // No with_capacity(len + 1): the vec holds at most a handful of names,
        // and an explicit size derived from `js_renderers.len()` trips CodeQL's
        // uncontrolled-allocation query (the length is operator config, not
        // request input, but the plain push/extend needs no hint at this size).
        let mut names = Vec::new();
        if self.has_impersonated_tier() {
            names.push("impersonated-http");
        }
        names.extend(self.js_renderer_names());
        names
    }

    /// The JS tiers the auto chain may enter on its own, in construction order.
    ///
    /// Drops the two tiers a name-pin would otherwise smuggle past a gate
    /// `fetch_with_js` applies:
    ///
    /// * camoufox held out by `include_in_auto = false`: an internally chosen
    ///   escalation must not override a deliberate opt-out;
    /// * chrome_proxy under `auto_egress_escalation`, where the chain lifts it
    ///   out of the ladder and holds it as a hard-block-gated, load-shed
    ///   recovery arm. Pinning it by name skips that gate and puts a paid
    ///   residential render on every page instead of the blocked ones.
    ///
    /// This is construction order, not the per-request order: `fetch_with_js`
    /// reorders for a host the preference learner has promoted, and filters for
    /// a screenshot. Callers that need one tier should express which one they
    /// want rather than trusting position alone.
    pub fn auto_ladder_names(&self) -> Vec<&str> {
        self.js_renderers
            .iter()
            .map(|r| r.name())
            .filter(|name| *name != "chrome_proxy" || !self.auto_egress_escalation)
            .filter(|name| {
                #[cfg(feature = "camoufox")]
                {
                    *name != "camoufox" || self.camoufox_in_auto
                }
                #[cfg(not(feature = "camoufox"))]
                {
                    let _ = name;
                    true
                }
            })
            .collect()
    }

    /// Which tier a post-LightPanda escalation should aim at, or `None` when
    /// this pool has nothing above lightpanda and the escalation should be
    /// skipped rather than dispatched.
    ///
    /// chrome first, so a pool that holds it behaves exactly as the old
    /// hardcoded target did, including for a host the preference learner has
    /// promoted to chrome. Otherwise the next tier in [`Self::auto_ladder_names`],
    /// which is what makes the escalation work on a deployment that runs no
    /// Chrome sidecar instead of failing on a pin the pool cannot satisfy.
    ///
    /// playwright is never chosen: it has no `RendererKind`, so a render on it
    /// carries no breaker, no `renderDecision` and no `creditCost`. It stays
    /// reachable through the auto chain and an explicit pin, as before.
    pub fn lightpanda_escalation_target(&self) -> Option<&str> {
        let ladder = self.auto_ladder_names();
        ladder
            .iter()
            .copied()
            .find(|name| *name == "chrome")
            .or_else(|| {
                ladder
                    .iter()
                    .copied()
                    .find(|name| *name != "lightpanda" && *name != "playwright")
            })
    }

    /// Whether this instance can actually capture a screenshot: at least one
    /// constructed JS renderer speaks CDP `Page.captureScreenshot`. Both the
    /// build features (no CDP feature ⇒ no tier is constructable) and the
    /// operator config (no `ws_url` ⇒ no tier) are reflected, because
    /// `js_renderers` is only populated when both hold.
    ///
    /// Shares [`renderer_can_screenshot`] with the request-time filter in
    /// [`Self::fetch_with_js`], so `/v1/capabilities` and the runtime can never
    /// disagree about which tiers can capture.
    pub fn supports_screenshot(&self) -> bool {
        self.js_renderer_names()
            .iter()
            .any(|name| renderer_can_screenshot(name))
    }

    /// Fetch a URL with smart mode: HTTP first, then JS if needed.
    ///
    /// When `render_js` is `None` (auto-detect), the renderer also escalates to
    /// JS rendering if the HTTP response looks like an anti-bot challenge page
    /// (Cloudflare "Just a moment...", etc.). The CDP renderer has built-in
    /// challenge retry logic that waits for non-interactive JS challenges to
    /// auto-resolve.
    pub async fn fetch(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        render_js: Option<bool>,
        wait_for_ms: Option<u64>,
        requested_renderer: Option<&str>,
        deadline: crw_core::Deadline,
    ) -> CrwResult<FetchResult> {
        self.fetch_hinted(
            url,
            headers,
            render_js,
            wait_for_ms,
            requested_renderer,
            false,
            deadline,
        )
        .await
    }

    /// `fetch` plus the server-injected `force_cloak` routing hint. Kept separate
    /// so the ~dozen existing `fetch` callers (crawl, discovery, tests) stay
    /// unchanged and pass `force_cloak = false` implicitly; only the scrape path
    /// that carries a per-request verdict opts in.
    #[allow(clippy::too_many_arguments)]
    pub async fn fetch_hinted(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        render_js: Option<bool>,
        wait_for_ms: Option<u64>,
        requested_renderer: Option<&str>,
        force_cloak: bool,
        deadline: crw_core::Deadline,
    ) -> CrwResult<FetchResult> {
        // Phase 0 (latency-qn): time the whole fetch and emit a structured
        // breakdown event so bench runs can attribute p90 to a tier. The flag
        // is off by default, so the only cost on the hot path is one cheap
        // `Instant::now()` + a branch. The accepted tier is `rendered_with`,
        // which already distinguishes the HTTP fast-path from each JS renderer.
        if !self.latency_breakdown {
            return self
                .fetch_inner(
                    url,
                    headers,
                    render_js,
                    wait_for_ms,
                    requested_renderer,
                    force_cloak,
                    deadline,
                )
                .await;
        }
        let t0 = std::time::Instant::now();
        let out = self
            .fetch_inner(
                url,
                headers,
                render_js,
                wait_for_ms,
                requested_renderer,
                force_cloak,
                deadline,
            )
            .await;
        let total_ms = t0.elapsed().as_millis() as u64;
        match &out {
            Ok(r) => tracing::info!(
                target: "latency_breakdown",
                url,
                total_ms,
                rendered_with = r.rendered_with.as_deref().unwrap_or("unknown"),
                content_len = r.html.len(),
                "scrape latency breakdown"
            ),
            Err(e) => tracing::info!(
                target: "latency_breakdown",
                url,
                total_ms,
                error = %e,
                "scrape latency breakdown (error)"
            ),
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    async fn fetch_inner(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        render_js: Option<bool>,
        wait_for_ms: Option<u64>,
        requested_renderer: Option<&str>,
        force_cloak: bool,
        deadline: crw_core::Deadline,
    ) -> CrwResult<FetchResult> {
        // Per-eTLD+1 rate-limit + concurrency cap. Held across the entire
        // fetch (including any escalation to a JS renderer) so a host that
        // rate-limits HTTP doesn't get hammered by Chrome on retry.
        let host_key = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(crate::preference::normalize_host));
        let _host_permit = if let Some(key) = host_key.as_deref() {
            let remaining = deadline.remaining();
            if remaining.is_zero() {
                return Err(CrwError::Timeout(deadline.requested_ms()));
            }
            match tokio::time::timeout(
                remaining,
                crate::host_limiter::acquire(
                    key,
                    self.requests_per_second,
                    self.per_host_max_concurrent as usize,
                    self.per_host_interactive_reserve as usize,
                ),
            )
            .await
            {
                Ok((permit, sleep)) => {
                    if !sleep.is_zero() {
                        let budget = deadline.remaining();
                        if sleep > budget {
                            return Err(CrwError::Timeout(sleep.as_millis().max(1) as u64));
                        }
                        tokio::time::sleep(sleep).await;
                    }
                    // Reserved per-host lane permit (interactive gets a dedicated
                    // slot, batch a bounded one). Held for the whole fetch by
                    // binding it to `_host_permit`.
                    Some(permit)
                }
                Err(_) => {
                    return Err(CrwError::Timeout(deadline.requested_ms()));
                }
            }
        } else {
            None
        };

        let mut effective = resolve_render_js(render_js, self.render_js_default);
        // A screenshot is captured via CDP — it can only happen on the JS/CDP
        // path. Force `render_js = Some(true)` so the `Some(false)` / auto
        // (`None`) branches below don't return an HTTP-only result that never
        // reaches `fetch_with_js` (where the capture occurs). The HTTP-only,
        // camoufox and lightpanda renderers are also filtered out downstream.
        if effective != Some(true) && screenshot_requested() {
            effective = Some(true);
        }
        tracing::debug!(
            url,
            request_render_js = ?render_js,
            default_render_js = ?self.render_js_default,
            effective_render_js = ?effective,
            requested_renderer,
            "FallbackRenderer::fetch dispatching"
        );
        // Cloak-first routing hint: for a domain the SaaS registry learned is
        // Cloudflare-managed, try the cloak Turnstile arm BEFORE the ladder with
        // the FULL deadline, so the ~39s cold solve fits in-band on request #1
        // instead of the ladder (lightpanda->chrome, which cannot clear CF)
        // burning the budget first. Reuses the recovery arm's floor+breaker+sem
        // guards verbatim; on ANY miss (floor not cleared / breaker open / no
        // permit / thin / challenge / error) it falls through to the normal
        // dispatch below, so a mis-flag can only cost time, never content (the
        // recall invariant). Screenshot requests skip it (cloak replays HTTP and
        // has no DOM to capture). `cloak_attempted` is threaded to
        // `fetch_with_js` so the post-ladder recovery arm is suppressed and one
        // page never burns two `cloak_sem` permits.
        let cloak_attempted: bool = {
            #[cfg(feature = "cloak")]
            {
                let mut attempted = false;
                // Eligible only for auto/unpinned requests that allow JS: a hard
                // renderer pin or `renderJs:false` is an explicit caller contract
                // ("no silent fallback" / "HTTP only") that cloak-first must not
                // silently break. Screenshots also skip it (cloak replays HTTP and
                // has no DOM to capture).
                let cloak_first_eligible = force_cloak
                    && !screenshot_requested()
                    && effective != Some(false)
                    && !matches!(requested_renderer, Some(name) if name != "auto");
                if cloak_first_eligible && let Some(arm) = &self.cloak_arm {
                    let host = host_of(url);
                    let kind = RendererKind::Cloak;
                    let floor =
                        std::time::Duration::from_millis(crw_core::config::CLOAK_ARM_FLOOR_MS);
                    // Reserve budget for the ladder: cloak-first runs on the SHARED
                    // deadline, so a slow/failed solve on a mis-flagged non-CF domain
                    // would otherwise starve the ladder into a Timeout (recall
                    // regression). Fire only with room for a full solve AND the
                    // reserve, and cap the cloak call at `remaining - reserve`.
                    let reserve = std::time::Duration::from_millis(
                        crw_core::config::CLOAK_FIRST_LADDER_RESERVE_MS,
                    );
                    let permit = if deadline.remaining() >= floor + reserve
                        && !self.breakers.host_for(&host, kind).await.is_open()
                    {
                        self.cloak_sem.clone().try_acquire_owned().ok()
                    } else {
                        None
                    };
                    if let Some(_permit) = permit {
                        attempted = true;
                        let cloak_deadline = crw_core::Deadline::now_plus(
                            deadline.remaining().saturating_sub(reserve),
                        );
                        let entry = self.pick_proxy_for_url(url);
                        let attempt = REQUEST_PROXY
                            .scope(entry, arm.fetch(url, headers, wait_for_ms, cloak_deadline))
                            .await;
                        match attempt {
                            Ok(mut r) => {
                                // `arm_ok` = the recovery arm's exact content gate;
                                // it drives the breaker outcome so a body that only
                                // the stricter ship gate rejects cannot skew the
                                // shared Cloak breaker toward open (which would
                                // suppress BOTH cloak paths for the host).
                                let arm_ok = html_body_text_len(&r.html)
                                    >= Self::MIN_RENDERED_TEXT_LEN
                                    && detector::looks_like_failed_render(&r.html).is_none()
                                    && !detector::looks_like_loading_placeholder(&r.html)
                                    && !detector::looks_like_cloudflare_challenge(&r.html)
                                    // See the recovery arms: CF alone is not the
                                    // only wall this tier can fail to clear.
                                    && !detector::looks_like_generic_bot_wall(&r.html, r.truncated)
                                    && detector::looks_like_vendor_block(&r.html).is_none();
                                // Ship only a fully-rendered body: a curl_cffi shell
                                // that passes `arm_ok` but is thin still falls
                                // through to chrome (RED-2).
                                let ship_ok = arm_ok && !detector::looks_like_thin_html(&r.html);
                                if !host.is_empty() {
                                    // A tier that returned an HTTP response we
                                    // could inspect is not broken; the judgement
                                    // below is about the PAGE, so it belongs to
                                    // the page's host. `record_scoped_outcome`
                                    // exists for exactly this and its doc says so,
                                    // but only the leak-through arm used it: the
                                    // other call sites wrote content verdicts into
                                    // the global window, where `min_calls: 50` is
                                    // reachable by one busy domain. Measured: after
                                    // the correct SiteBlocked suppression only ~214
                                    // lightpanda outcomes reached any window in 6h,
                                    // and github.com alone contributed 38 of them,
                                    // all failures. Success still records globally,
                                    // so the ratio keeps an honest denominator.
                                    if arm_ok {
                                        self.breakers
                                            .record_outcome(&host, kind, BreakerOutcome::Success)
                                            .await;
                                    } else {
                                        self.breakers
                                            .record_scoped_outcome(
                                                &host,
                                                kind,
                                                None,
                                                Some(BreakerOutcome::RenderError),
                                            )
                                            .await;
                                    }
                                }
                                if self.latency_breakdown {
                                    tracing::info!(
                                        target: "latency_breakdown",
                                        url, tier = "cloak", ok = ship_ok, consumed = ship_ok,
                                        "cloak-first fired"
                                    );
                                }
                                if ship_ok {
                                    // Stamp the routing metadata + flat page credit
                                    // the recovery-arm accept path also stamps
                                    // (cloak.rs emits credit_cost:0 / decision:None).
                                    r.credit_cost = credit_for(kind);
                                    r.render_decision = Some(RenderDecision::Failover {
                                        chain: vec![kind],
                                        reason: FailoverErrorKind::Other,
                                    });
                                    return Ok(r);
                                }
                                // thin -> fall through to the ladder.
                            }
                            Err(_e) => {
                                if !host.is_empty() {
                                    // A body the sidecar returned and we rejected
                                    // is a page verdict, not a dead tier.
                                    let global = (!is_cloak_content_verdict(&_e))
                                        .then_some(BreakerOutcome::ConnectionError);
                                    self.breakers
                                        .record_scoped_outcome(
                                            &host,
                                            kind,
                                            global,
                                            Some(BreakerOutcome::ConnectionError),
                                        )
                                        .await;
                                }
                                if self.latency_breakdown {
                                    tracing::info!(
                                        target: "latency_breakdown",
                                        url, tier = "cloak", error = %_e,
                                        "cloak-first fired (error)"
                                    );
                                }
                                // error -> fall through to the ladder.
                            }
                        }
                    }
                }
                attempted
            }
            #[cfg(not(feature = "cloak"))]
            {
                let _ = force_cloak;
                false
            }
        };

        // Early pin arm for the impersonated HTTP tier: it must run BEFORE the
        // `match effective` dispatch so an impersonated pin works whatever
        // `renderJs` resolution says (the pin chooses a transport, not a JS
        // render), and it never reaches `fetch_with_js`'s name filter.
        // REQUEST_PROXY is honored via the warm per-proxy cache.
        //
        // The pin still runs the WALL verdict: a pinned result that carries a
        // block surfaces as an error (the hard-pin "failures must surface"
        // contract), never as a billed `userPinned` success, and the
        // route-decision metric records "blocked", not "success". It does NOT
        // run the auto-chain hop's accept gate: the pin has no ladder to fall
        // through to, so a PDF, a 404 or a thin page must come back as the
        // result it is, exactly as a hard browser pin returns its body.
        if requested_renderer == Some("impersonated-http") {
            #[cfg(feature = "impersonated")]
            {
                let kind = RendererKind::ImpersonatedHttp;
                // Counted here because this arm returns ahead of
                // `fetch_with_js`, which owns the metric for every other pin.
                metrics()
                    .user_pin_total
                    .with_label_values(&[kind.as_str()])
                    .inc();
                let fetcher = match self.impersonated_fetcher_for_request() {
                    Ok(f) => f,
                    Err(e) => {
                        metrics()
                            .render_route_decision_total
                            .with_label_values(&[kind.as_str(), "error"])
                            .inc();
                        return Err(e);
                    }
                };
                let r = match fetcher.fetch(url, headers, wait_for_ms, deadline).await {
                    Ok(r) => r,
                    Err(e) => {
                        metrics()
                            .render_route_decision_total
                            .with_label_values(&[kind.as_str(), "error"])
                            .inc();
                        return Err(e);
                    }
                };
                if self.impersonation_blocked(&r) {
                    tracing::warn!(
                        url,
                        status_code = r.status_code,
                        "pinned impersonated-http result looks blocked; surfacing as an error"
                    );
                    metrics()
                        .render_route_decision_total
                        .with_label_values(&[kind.as_str(), "blocked"])
                        .inc();
                    return Err(CrwError::RendererError(
                        "impersonated-http: the response carries an anti-bot block \
                         (vendor wall class); the pinned tier refuses to bill it as a success"
                            .into(),
                    ));
                }
                let mut r = r;
                // Same courtesy the hard BROWSER pin extends when its single
                // tier comes back thin: the pin had no failover to run, so say
                // so instead of returning sparse markdown under `success:true`
                // with no explanation.
                if r.content_type.as_deref() != Some("application/pdf")
                    && !self.classify_js_attempt(&r).acceptable
                {
                    r.warnings.push(format!(
                        "Pinned renderer '{}' returned little or no content. It executes no \
                         JavaScript, so a client-rendered page will look empty here. Retry with \
                         renderer=\"chrome\" or omit the renderer field for auto-failover.",
                        kind.as_str(),
                    ));
                }
                r.credit_cost = credit_for(kind);
                r.render_decision = Some(RenderDecision::UserPinned { renderer: kind });
                metrics()
                    .render_route_decision_total
                    .with_label_values(&[kind.as_str(), "success"])
                    .inc();
                return Ok(r);
            }
            #[cfg(not(feature = "impersonated"))]
            {
                return Err(CrwError::RendererError(
                    "renderer \"impersonated-http\" requires the 'impersonated' feature, \
                     but this build was compiled without it"
                        .into(),
                ));
            }
        }

        // A non-"auto" pinned renderer is a hard pin — failures must surface.
        let is_hard_pinned = matches!(requested_renderer, Some(name) if name != "auto");
        match effective {
            Some(false) => {
                let mut r = self
                    .http_fetcher_for_request()?
                    .fetch(url, headers, None, deadline)
                    .await?;
                stamp_http_decision(&mut r, requested_renderer);
                Ok(r)
            }
            Some(true) => {
                // Fetch via HTTP first to check content type — PDFs can't be JS-rendered.
                // An HTTP-layer failure is not terminal here either: the auto arm
                // escalates on it (bench: 10/147 false "unreachable" + 5/147
                // "http_502" recover via a real Chromium navigation), and a
                // caller who explicitly asked for JS must not get LESS recall
                // than one who asked for nothing. Without this, `renderJs:true`
                // — and every screenshot request, which :1242 forces down this
                // arm — never reached the browser on an origin that rejects
                // reqwest's TLS fingerprint.
                let mut http_result = match self
                    .http_fetcher_for_request()?
                    .fetch(url, headers, None, deadline)
                    .await
                {
                    Ok(r) => r,
                    // `UnsupportedContentType` is excluded on purpose: it means
                    // the body is not a web page at all (a .docx ZIP, an image),
                    // which no renderer can fix. Escalating one costs a full
                    // lightpanda -> chrome -> camoufox climb (measured ~23s and a
                    // Camoufox session) and still fails, with the precise content
                    // type lost behind the ladder's generic "no usable content".
                    Err(e)
                        if !self.js_renderers.is_empty()
                            && !matches!(e, CrwError::UnsupportedContentType(_)) =>
                    {
                        tracing::info!(
                            url,
                            error = %e,
                            "HTTP fetch failed, escalating to JS renderer"
                        );
                        return self
                            .fetch_with_js(
                                url,
                                headers,
                                wait_for_ms,
                                requested_renderer,
                                cloak_attempted,
                                deadline,
                            )
                            .await
                            .map_err(|js_err| {
                                tracing::warn!("Both HTTP and JS failed: http={e}, js={js_err}");
                                // Same attribution rule as the auto arm: a dead
                                // origin is the caller's target (422), not our
                                // renderer breaking (500).
                                match (&e, &js_err) {
                                    (CrwError::TargetUnreachable(_), js)
                                        if is_origin_navigation_failure(js) =>
                                    {
                                        e
                                    }
                                    _ => js_err,
                                }
                            });
                    }
                    Err(e) => return Err(e),
                };
                if http_result.content_type.as_deref() == Some("application/pdf") {
                    // A PDF has no rendered DOM to capture. A screenshot request
                    // on a PDF returns the parsed document with no `screenshot`
                    // field (ponytail: honest null — PDFs genuinely can't be
                    // screenshotted; not worth a warning the PDF parse path drops).
                    stamp_http_decision(&mut http_result, requested_renderer);
                    return Ok(http_result);
                }

                if self.js_renderers.is_empty() {
                    // A screenshot needs CDP — there is no HTTP fallback that can
                    // satisfy it. Fail closed rather than return a 200 with a null
                    // screenshot the caller explicitly asked for.
                    if screenshot_requested() {
                        return Err(CrwError::RendererError(
                            "a screenshot was requested but no JS renderer is available; \
                             configure a chrome/chrome_proxy tier"
                                .into(),
                        ));
                    }
                    tracing::warn!(
                        url,
                        "JS rendering requested but no renderer available — falling back to HTTP"
                    );
                    let mut result = http_result;
                    result.rendered_with = Some("http_only_fallback".to_string());
                    result.warning = Some("JS rendering was requested but no renderer is available. Content was fetched via HTTP only.".to_string());
                    result.warnings.push(
                        "JS rendering requested but no renderer available; HTTP fallback used"
                            .into(),
                    );
                    stamp_http_decision(&mut result, requested_renderer);
                    Ok(result)
                } else {
                    // The HTTP body was already fetched above for the
                    // content-type check, so when the JS ladder fails there is a
                    // valid document sitting in hand — returning `Err` and a 504
                    // instead of that body is a straight recall loss, and the
                    // auto arm below has never done it.
                    //
                    // Unlike auto, the fallback here is ALWAYS announced. Auto
                    // can swap silently because the caller expressed no
                    // preference; a `renderJs:true` caller asked for a browser
                    // and must be able to tell they did not get one, both to
                    // debug and because the request is billed either way.
                    let is_auth_blocked = is_soft_block_status(http_result.status_code);
                    let started_at = std::time::Instant::now();
                    let js_outcome = self
                        .fetch_with_js(
                            url,
                            headers,
                            wait_for_ms,
                            requested_renderer,
                            cloak_attempted,
                            deadline,
                        )
                        .await;
                    // A ladder result that is nothing but a wall must not
                    // displace a usable HTTP body. This is the preference the
                    // old error carried implicitly; see the helper.
                    let js_outcome = demote_wall_to_ladder_failure(
                        js_outcome,
                        &http_result,
                        !screenshot_requested() && !is_hard_pinned,
                    );
                    match js_outcome {
                        Ok(js_result) => Ok(js_result),
                        // A capture has no HTTP substitute (the caller asked for
                        // pixels), and an explicit renderer pin is a caller
                        // contract that forbids silent substitution. Both fail
                        // closed, matching the no-renderer arm directly above.
                        Err(e) if screenshot_requested() || is_hard_pinned => Err(e),
                        Err(e) => {
                            // The HTTP shell substitutes for a failed ladder so
                            // the caller gets content instead of nothing. But
                            // when that shell IS the wall the ladder just
                            // refused to clear, returning it as content hands
                            // back exactly what every tier rejected.
                            //
                            // That used to `Err(e)` out of the guard above, which
                            // was right about the body and wrong about the
                            // status: `CrwError::HttpError` maps to 502, so a
                            // target refusing us was reported as our own gateway
                            // failing. The caller saw "Bad Gateway", the SaaS
                            // booked `upstream_5xx`, and the watchdog paged for a
                            // platform incident that was not happening.
                            //
                            // Carry the verdict instead and keep every other
                            // decision on this path untouched: the shell still
                            // travels, and `crw_crawl::single` stamps it at the
                            // shared choke, so the request ends as
                            // `success: false` + `anti_bot` with no credit, like
                            // every other wall. A shell that is not a wall still
                            // substitutes exactly as before.
                            if detector::looks_like_generic_bot_wall(
                                &http_result.html,
                                http_result.truncated,
                            ) {
                                http_result.block = Some(unclearable_wall_verdict());
                            }
                            if is_auth_blocked {
                                tracing::error!(
                                    url,
                                    status_code = http_result.status_code,
                                    "JS escalation failed for soft-block status; surfacing HTTP shell with warning: {e}"
                                );
                            } else {
                                tracing::warn!(
                                    "JS rendering failed, falling back to HTTP result: {e}"
                                );
                            }
                            let warning = format!("{JS_ESCALATION_FAILED} {e}");
                            http_result.warning = Some(match http_result.warning.take() {
                                Some(prev) => format!("{warning}; {prev}"),
                                None => warning,
                            });
                            // `elapsed_ms` came from the HTTP fetch alone, so it
                            // would report a few hundred ms for a request that
                            // spent the whole deadline in the ladder.
                            http_result.elapsed_ms = http_result
                                .elapsed_ms
                                .saturating_add(started_at.elapsed().as_millis() as u64);
                            // `stamp_http_decision` below records this as a plain
                            // `http`/`success` route, which is what an operator
                            // would read as "no browser was needed". Emit the
                            // real story first so forced-JS fallbacks are
                            // separable from ordinary HTTP traffic; without it
                            // the whole failure class is invisible in metrics.
                            metrics()
                                .render_route_decision_total
                                .with_label_values(&[
                                    RendererKind::Http.as_str(),
                                    "jsLadderExhausted",
                                ])
                                .inc();
                            stamp_http_decision(&mut http_result, requested_renderer);
                            Ok(http_result)
                        }
                    }
                }
            }
            None => {
                // In auto mode, an HTTP-layer failure (TargetUnreachable, body
                // decode mid-stream, oversize response, transient network) is
                // not terminal: if a JS renderer is available, escalate. Many
                // sites that reject reqwest's TLS/UA fingerprint succeed via a
                // real Chromium navigation. Bench analysis: 10/147 false
                // "unreachable" + 5/147 "http_502" map to this branch.
                let fetched = self
                    .http_fetcher_for_request()?
                    .fetch(url, headers, None, deadline)
                    .await;
                // Chrome-impersonation hop on a TRANSPORT failure
                // (fingerprint-shaped TLS rejection: the origin talks to
                // browsers, not to reqwest), BEFORE the JS escalation below.
                // When it returns None the match that follows is
                // byte-identical to the pre-hop ladder. Suppressed for
                // `UnsupportedContentType` for the same reason the escalation
                // arm excludes it: no client turns a .docx into a page.
                // `TargetUnreachable` (DNS / refused / blackholed connect) and
                // the >50MB size-cap verdict are also excluded: a Chrome TLS
                // fingerprint cannot fix either, and retrying would double the
                // connect timeout or re-download the whole oversize body under
                // a second client (possibly through a paid proxy).
                // `Timeout` is excluded for the same reason: a stalled or
                // tarpitting origin is slow to every client, so a second
                // request under a Chrome fingerprint cannot make it fast. It
                // would only spend the ladder's remaining budget re-learning
                // that the origin is slow.
                let fingerprint_shaped = |e: &CrwError| {
                    !matches!(e, CrwError::UnsupportedContentType(_))
                        && !matches!(e, CrwError::TargetUnreachable(_))
                        && !matches!(e, CrwError::Timeout(_))
                        && !matches!(e, CrwError::HttpError(m) if m.starts_with("Response too large"))
                };
                let fetched = match fetched {
                    Err(e) if fingerprint_shaped(&e) => {
                        #[cfg(feature = "impersonated")]
                        if let Some(r) = self
                            .try_impersonated_hop(
                                url,
                                headers,
                                deadline,
                                ImpersonatedTrigger::Transport,
                            )
                            .await
                        {
                            return Ok(r);
                        }
                        Err(e)
                    }
                    other => other,
                };
                let mut result = match fetched {
                    Ok(r) => r,
                    // `UnsupportedContentType` is excluded on purpose: it means
                    // the body is not a web page at all (a .docx ZIP, an image),
                    // which no renderer can fix. Escalating one costs a full
                    // lightpanda -> chrome -> camoufox climb (measured ~23s and a
                    // Camoufox session) and still fails, with the precise content
                    // type lost behind the ladder's generic "no usable content".
                    Err(e)
                        if !self.js_renderers.is_empty()
                            && !matches!(e, CrwError::UnsupportedContentType(_)) =>
                    {
                        tracing::info!(
                            url,
                            error = %e,
                            "HTTP fetch failed, escalating to JS renderer"
                        );
                        return self
                            .fetch_with_js(
                                url,
                                headers,
                                wait_for_ms,
                                requested_renderer,
                                cloak_attempted,
                                deadline,
                            )
                            .await
                            .map_err(|js_err| {
                                tracing::warn!("Both HTTP and JS failed: http={e}, js={js_err}");
                                // When the HTTP tier could not reach the origin AND the JS tier
                                // failed navigating to that same origin, the origin is the root
                                // cause: surface TargetUnreachable (422 — the caller handed us a
                                // dead target) instead of the JS tier's RendererError, which
                                // falls through to a 500 and reads as "our server broke".
                                //
                                // A JS failure can also be OUR fault (pool exhausted, CDP
                                // discovery failed, pinned renderer missing). Those keep their
                                // own error, or we would blame the caller for our outage.
                                match (&e, &js_err) {
                                    (CrwError::TargetUnreachable(_), js)
                                        if is_origin_navigation_failure(js) =>
                                    {
                                        e
                                    }
                                    _ => js_err,
                                }
                            });
                    }
                    Err(e) => return Err(e),
                };

                // PDFs don't need JS rendering — return immediately.
                if result.content_type.as_deref() == Some("application/pdf") {
                    stamp_http_decision(&mut result, requested_renderer);
                    return Ok(result);
                }

                let needs_js = detector::needs_js_rendering(&result.html);
                // Either header-announced vendor challenge (`cf-mitigated` or
                // `x-amzn-waf-action`). Independent of status and body, so it
                // catches the AWS-WAF shape that carries NO body to inspect:
                // HTTP 202 + content-length 0, which every body detector misses.
                //
                // The AWS half is additionally gated on `!is_hard_pinned`. The
                // pinned path surfaces a JS failure as an error instead of falling
                // back to the HTTP body (`Err(e) if is_hard_pinned`), so letting
                // the new signal escalate a pinned request would convert today's
                // `Ok`-with-an-empty-202 into a 5xx — the same debit the
                // `!is_hard_pinned` term on `is_empty_2xx` exists to prevent, one
                // line over. `cloudflare_mitigated` keeps its pre-existing
                // behaviour untouched; this change adds no new pinned failure.
                let challenge_header_signal = match result.warning.as_deref() {
                    Some("cloudflare_mitigated") => true,
                    Some("waf_challenge") => !is_hard_pinned,
                    _ => false,
                };
                let is_generic_bot_wall =
                    detector::looks_like_generic_bot_wall(&result.html, result.truncated);
                let is_cf_challenge = detector::looks_like_cloudflare_challenge(&result.html);
                let is_blocked = challenge_header_signal || is_cf_challenge || is_generic_bot_wall;
                let is_auth_blocked = is_soft_block_status(result.status_code);
                // Post-fetch thin-content trigger: HTTP returned 2xx but the
                // body has effectively no extractable text. Catches sites whose
                // SPA marker we don't recognize (no `id="root"`, no
                // `__next_data__`) yet still return a near-empty HTML shell.
                // Bench analysis showed 23/147 failures fall in this bucket
                // (seattletimes, espn, ionos, huduser, …).
                // Escalate a thin 2xx body ONLY when a browser would plausibly
                // reveal more (executable JS, or a meta-refresh redirect). A
                // script-less static doc (e.g. example.com) is already complete,
                // so a headless render just adds seconds for nothing. The
                // recognized-shell sites this bucket targets (seattletimes, espn,
                // …) all ship script bundles, so they still escalate.
                let is_2xx = (200..300).contains(&result.status_code);
                let is_thin_content = is_2xx
                    && detector::looks_like_thin_html(&result.html)
                    && detector::warrants_browser_retry(&result.html);
                // A 2xx with a literally empty body carries no content by
                // definition, and `warrants_browser_retry` structurally cannot
                // fire on it (there is no markup to find a script tag in), so the
                // thin-content path above misses it entirely and the empty
                // response is returned to the caller as a success. Observed on
                // AWS-WAF hosts that answer 202 + content-length 0.
                //
                // Narrow on purpose:
                //   * 204/205/206 legitimately carry no (full) body;
                //   * non-HTML content types (empty JSON, plain text, images,
                //     archives) gain nothing from a browser — and this matters
                //     because the HTTP tier decodes EVERY non-PDF response as
                //     HTML, so without the check they would all escalate;
                //   * a hard-pinned renderer surfaces JS failures as an error
                //     rather than falling back to the HTTP body, so escalating
                //     here would turn today's empty-but-Ok into a hard 5xx.
                //   * PDFs already returned earlier.
                let is_empty_2xx = is_2xx
                    && !is_hard_pinned
                    && !matches!(result.status_code, 204..=206)
                    && is_html_like_content_type(result.content_type.as_deref())
                    && result.html.trim().is_empty();

                // Chrome-impersonation hop (feature-gated): between the plain
                // HTTP tier and the browser ladder, on WALL-shaped triggers
                // only. Tier presence is the FIRST conjunct so a config-disabled
                // tier pays none of the vendor-wall scans below. Never fires on
                // needs_js / thin / empty shapes (impersonation cannot execute
                // JS) and never when the wall is a fingerprint-vendor wall
                // (Cloudflare managed challenge, DataDome, PerimeterX, Kasada,
                // Imperva: those need JS no HTTP client can fake, so firing only
                // burns budget before the ladder). Independent of js_renderers
                // emptiness: an HTTP-only deployment gets the Amazon fix with
                // zero browsers.
                #[cfg(feature = "impersonated")]
                if self.impersonated.is_some()
                    && (is_blocked || is_hard_block_status(result.status_code))
                    // `challenge_header_signal` joins the body-derived CF
                    // check here: `cf-mitigated` / `x-amzn-waf-action` announce
                    // a vendor challenge that needs JS, and the hop's own accept
                    // gate rejects exactly those two markers, so firing on them
                    // is a round trip that is rejected by construction.
                    && !is_fingerprint_vendor_wall(
                        is_cf_challenge || challenge_header_signal,
                        detector::looks_like_vendor_block(&result.html),
                        self.antibot_result(result.status_code, &result.html).signal,
                    )
                    && let Some(r) = self
                        .try_impersonated_hop(url, headers, deadline, ImpersonatedTrigger::Wall)
                        .await
                {
                    return Ok(r);
                }

                if !self.js_renderers.is_empty()
                    && (needs_js
                        || is_blocked
                        || is_auth_blocked
                        || is_thin_content
                        || is_empty_2xx)
                {
                    if is_auth_blocked {
                        tracing::info!(
                            url,
                            status_code = result.status_code,
                            "HTTP {} received, escalating to JS renderer",
                            result.status_code
                        );
                    } else if is_blocked {
                        tracing::info!(
                            url,
                            "Anti-bot challenge detected in HTTP response, escalating to JS renderer"
                        );
                        if is_generic_bot_wall {
                            tracing::info!(
                                url,
                                "Generic anti-bot interstitial detected, escalating to JS renderer"
                            );
                        }
                    } else if needs_js {
                        tracing::info!(url, "SPA shell detected, retrying with JS renderer");
                    } else {
                        tracing::info!(
                            url,
                            html_len = result.html.len(),
                            "HTTP 2xx but body is thin, escalating to JS renderer"
                        );
                    }
                    let js_outcome = self
                        .fetch_with_js(
                            url,
                            headers,
                            wait_for_ms,
                            requested_renderer,
                            cloak_attempted,
                            deadline,
                        )
                        .await;
                    // Same preference as the sibling arm above.
                    let js_outcome =
                        demote_wall_to_ladder_failure(js_outcome, &result, !is_hard_pinned);
                    match js_outcome {
                        Ok(js_result) => Ok(js_result),
                        // Pinned: the caller's contract forbids substitution.
                        Err(e) if is_hard_pinned => Err(e),
                        Err(e) => {
                            // The shell is the very thing the ladder rejected, so
                            // it is not a fallback. See the sibling arm above for
                            // why that verdict now travels as data instead of as
                            // a `CrwError::HttpError` the server turns into 502.
                            if detector::looks_like_generic_bot_wall(&result.html, result.truncated)
                            {
                                result.block = Some(unclearable_wall_verdict());
                            }
                            // For `is_auth_blocked` (4xx/5xx soft-block status codes), the
                            // HTTP body is almost certainly an error shell — falling back
                            // to it silently misleads the caller. For `needs_js` /
                            // `is_blocked` / `is_thin_content`, the HTTP body still has
                            // *some* useful content, so the fallback itself stays silent
                            // and only the log level differs.
                            //
                            // The warning tag does NOT differ. `JS_ESCALATION_FAILED` is
                            // how `crw_crawl::single` learns the ladder is spent
                            // (`js_ladder_exhausted`); tagging only the soft-block arm let
                            // a body-detected block on a plain 200 — the canonical
                            // Turnstile-over-200 shape — re-run the ENTIRE ladder against
                            // a site that had just failed it, doubling the wall clock for
                            // a result that cannot differ. The sibling `render_js:true`
                            // arm above has always tagged unconditionally; this matches it.
                            if is_auth_blocked {
                                tracing::error!(
                                    url,
                                    status_code = result.status_code,
                                    "JS escalation failed for soft-block status; surfacing HTTP shell with warning: {e}"
                                );
                            } else {
                                tracing::warn!(
                                    "JS rendering failed, falling back to HTTP result: {e}"
                                );
                            }
                            let warning = format!("{JS_ESCALATION_FAILED} {e}");
                            result.warning = Some(match result.warning.take() {
                                Some(prev) => format!("{warning}; {prev}"),
                                None => warning,
                            });
                            stamp_http_decision(&mut result, requested_renderer);
                            Ok(result)
                        }
                    }
                } else {
                    stamp_http_decision(&mut result, requested_renderer);
                    Ok(result)
                }
            }
        }
    }

    /// Minimum body text length for a JS-rendered result to be considered
    /// successful. If the rendered page has less visible text than this, the
    /// next renderer in the chain is tried.
    const MIN_RENDERED_TEXT_LEN: usize = 50;

    /// Pure classification of a JS-renderer result: the accept-gate + thin/block
    /// signals, NO side-effects. Shared by the serial escalation loop and the
    /// conditional hedge so both apply the identical accept criteria (the red
    /// line: hedge must be provably ≡ serial on success/recall).
    fn classify_js_attempt(&self, result: &FetchResult) -> JsAttemptClass {
        let text_len = html_body_text_len(&result.html);
        let is_placeholder = detector::looks_like_loading_placeholder(&result.html);
        let failed_render = detector::looks_like_failed_render(&result.html);
        let is_bot_wall = detector::looks_like_generic_bot_wall(&result.html, result.truncated);
        let vendor_block = detector::looks_like_vendor_block(&result.html);
        // Size-independent Cloudflare interstitial check: modern managed
        // challenges are 100-300KB with the challenge marker deep in the body,
        // which the size-capped `vendor_block`/`bot_wall`/`antibot` detectors all
        // miss. Without this a CF challenge slips through as "thin" (not a hard
        // block), so the chrome_proxy egress-recovery arm never fires.
        let cf_challenge = detector::looks_like_cloudflare_challenge(&result.html);
        let is_status_blocked = matches!(
            result.status_code,
            401 | 403 | 404 | 405 | 406 | 410 | 412 | 429 | 451 | 500 | 503
        );
        let antibot = self.antibot_result(result.status_code, &result.html);
        let antibot_blocked = self.antibot.escalate_in_failover && antibot.signal.is_blocked();
        // Egress-recoverable hard-block subset (drives the gated chrome_proxy arm).
        let hard_block = is_hard_block_status(result.status_code)
            || is_bot_wall
            || vendor_block.is_some()
            || cf_challenge
            || antibot.signal.is_blocked();
        let acceptable = text_len >= Self::MIN_RENDERED_TEXT_LEN
            && !is_placeholder
            && failed_render.is_none()
            && !is_bot_wall
            && vendor_block.is_none()
            && !cf_challenge
            && !is_status_blocked
            && !antibot_blocked;
        let unrecoverable_wall =
            is_fingerprint_vendor_wall(cf_challenge, vendor_block, antibot.signal);
        JsAttemptClass {
            text_len,
            is_placeholder,
            failed_render,
            is_bot_wall,
            vendor_block,
            is_status_blocked,
            antibot,
            antibot_blocked,
            hard_block,
            unrecoverable_wall,
            acceptable,
        }
    }

    /// Conditional hedge: race lightpanda + chrome CONCURRENTLY (chrome's render
    /// clock starts immediately instead of after lightpanda fails) and take the
    /// best result by tier priority. Returns `None` if a breaker was open (caller
    /// falls back to serial). Success/recall ≡ serial:
    ///   * Rule A: among gate-passing results, lightpanda wins (serial accepts
    ///     lightpanda when it passes, never seeing chrome); a faster-arriving
    ///     chrome only wins if lightpanda is NOT acceptable.
    ///   * Rule B: if neither passes, return the richest-HTML thin (== serial's
    ///     thin stitch).
    ///   * Rule C: record breaker/preference side-effects only for tiers that
    ///     actually COMPLETED (the cancelled loser — dropped on the other's
    ///     accept — records nothing); its in-flight render is reaped by the
    ///     PoolGuard Drop reaper.
    #[allow(clippy::too_many_arguments)] // url/headers/wait/deadline/host mirror fetch_with_js
    async fn try_hedge(
        &self,
        lp: &Arc<dyn PageFetcher>,
        chrome: &Arc<dyn PageFetcher>,
        url: &str,
        headers: &HashMap<String, String>,
        wait_for_ms: Option<u64>,
        deadline: crw_core::Deadline,
        host: &str,
    ) -> CrwResult<Option<HedgeOutcome>> {
        // Breaker gates (mirror serial). If either tier's breaker is open, bail to
        // serial so its skip/leak-through handling applies.
        let (lp_permit, lp_guard) = self
            .breakers
            .acquire_with_guard(host, RendererKind::Lightpanda)
            .await;
        if lp_permit == Permit::Rejected {
            drop(lp_guard);
            return Ok(None);
        }
        let (ch_permit, ch_guard) = self
            .breakers
            .acquire_with_guard(host, RendererKind::Chrome)
            .await;
        if ch_permit == Permit::Rejected {
            drop(lp_guard);
            drop(ch_guard);
            return Ok(None);
        }
        // Both `acquire_with_guard` calls above await, so the budget may have drained
        // since the caller's floor check. Re-check before dispatching, or the floor is
        // only advisory here. Bail to the serial loop, which applies its own floor and
        // records the skip.
        if deadline.remaining() < MIN_TIER_BUDGET {
            drop(lp_guard);
            drop(ch_guard);
            return Ok(None);
        }

        let mut lp_guard = Some(lp_guard);
        let mut ch_guard = Some(ch_guard);

        // Race both on the CURRENT task (select!, not spawn) so REQUEST_PROXY /
        // REQUEST_COUNTRY task-locals propagate into each fetch.
        let lp_fut = lp.fetch(url, headers, wait_for_ms, deadline);
        let chrome_fut = chrome.fetch(url, headers, wait_for_ms, deadline);
        tokio::pin!(lp_fut, chrome_fut);
        let (mut lp_done, mut ch_done) = (false, false);
        let mut lp_res: Option<CrwResult<FetchResult>> = None;
        let mut ch_res: Option<CrwResult<FetchResult>> = None;
        while !(lp_done && ch_done) {
            tokio::select! {
                biased;
                r = &mut lp_fut, if !lp_done => {
                    lp_done = true;
                    let accept = matches!(&r, Ok(res) if self.classify_js_attempt(res).acceptable);
                    lp_res = Some(r);
                    // Rule A: lightpanda authoritative — accept now, drop chrome.
                    if accept {
                        break;
                    }
                }
                r = &mut chrome_fut, if !ch_done => {
                    ch_done = true;
                    let ch_accept = matches!(&r, Ok(res) if self.classify_js_attempt(res).acceptable);
                    ch_res = Some(r);
                    // chrome may finish first; only accept it early once lightpanda
                    // is known NOT acceptable (else wait for lightpanda — Rule A).
                    if lp_done {
                        let lp_accept = matches!(&lp_res, Some(Ok(res)) if self.classify_js_attempt(res).acceptable);
                        if !lp_accept && ch_accept {
                            break;
                        }
                    }
                }
            }
        }
        // The still-pending future (if any) drops at scope end → PoolGuard reaper.

        // Finalize. Record side-effects only for COMPLETED tiers (Some result).
        let lp_accept =
            matches!(&lp_res, Some(Ok(res)) if self.classify_js_attempt(res).acceptable);
        let ch_accept =
            matches!(&ch_res, Some(Ok(res)) if self.classify_js_attempt(res).acceptable);

        // Rule A: lightpanda wins if acceptable.
        if lp_accept {
            let mut r = lp_res.unwrap().unwrap();
            self.record_hedge_success(host, RendererKind::Lightpanda, &r, &mut lp_guard)
                .await;
            // chrome cancelled or thin → no record (Rule C).
            r.credit_cost = credit_for(RendererKind::Lightpanda);
            r.render_decision = Some(RenderDecision::AutoDefault {
                chosen: RendererKind::Lightpanda,
            });
            return Ok(Some(HedgeOutcome::Accepted(r)));
        }
        // lightpanda completed thin → record it (serial would have).
        let mut saw_hard_block = false;
        let mut saw_unrecoverable_wall = false;
        if let Some(Ok(res)) = &lp_res {
            let cls = self.classify_js_attempt(res);
            saw_hard_block |= cls.hard_block;
            saw_unrecoverable_wall |= cls.unrecoverable_wall;
            self.record_hedge_thin(host, RendererKind::Lightpanda, &cls, &mut lp_guard)
                .await;
        }
        if ch_accept {
            let mut r = ch_res.unwrap().unwrap();
            self.record_hedge_success(host, RendererKind::Chrome, &r, &mut ch_guard)
                .await;
            r.credit_cost = credit_for(RendererKind::Chrome);
            r.render_decision = Some(RenderDecision::Failover {
                chain: vec![RendererKind::Lightpanda, RendererKind::Chrome],
                reason: FailoverErrorKind::Other,
            });
            return Ok(Some(HedgeOutcome::Accepted(r)));
        }
        // chrome completed thin → record it.
        if let Some(Ok(res)) = &ch_res {
            let cls = self.classify_js_attempt(res);
            saw_hard_block |= cls.hard_block;
            saw_unrecoverable_wall |= cls.unrecoverable_wall;
            self.record_hedge_thin(host, RendererKind::Chrome, &cls, &mut ch_guard)
                .await;
        }

        // Log the errored legs on the path where NEITHER leg was accepted. Both
        // accept branches above return before reaching here, so a leg that
        // errored while the other won is still not logged; that case is covered
        // by neither this nor the serial loop, and is left alone deliberately
        // because it does not affect the request's outcome.
        //
        // This function emitted no tracing at all, which is why a hedge that
        // lost both legs was invisible: 12h of production held 190 budget-skip
        // errors against only 104 `JS renderer failed` lines, so most failures
        // had no tier logged anywhere. The serial loop cannot cover for it. By
        // the time control returns, the hedge has spent the shared deadline and
        // every serial tier is skipped on the budget floor.
        for (kind, res) in [
            (RendererKind::Lightpanda, &lp_res),
            (RendererKind::Chrome, &ch_res),
        ] {
            if let Some(Err(e)) = res {
                // WARN to match the serial loop's `JS renderer failed`, which is
                // the identical event. At INFO an operator filtering on WARN
                // would see serial tier failures and not hedge ones, which is
                // the same blind spot one level up.
                tracing::warn!(renderer = kind.as_str(), "hedge leg failed: {e}");
            }
        }

        // Rule B: best-thin = richest HTML among completed Ok results.
        let thin = [lp_res, ch_res]
            .into_iter()
            .flatten()
            .filter_map(|r| r.ok())
            .max_by_key(|r| r.html.len());
        match thin {
            Some(r) => Ok(Some(HedgeOutcome::Thin(
                r,
                saw_hard_block,
                saw_unrecoverable_wall,
            ))),
            // Both tiers errored. The caller falls back to the serial loop,
            // which will skip every tier on the spent budget and report
            // `Timeout(requested_ms)`. That status is preserved deliberately;
            // the legs' real causes are on the `warn!` lines above.
            None => Ok(None),
        }
    }

    /// Record a hedge winner's success side-effects (breaker + preference + guard).
    async fn record_hedge_success(
        &self,
        host: &str,
        k: RendererKind,
        result: &FetchResult,
        guard: &mut Option<ProbeGuard>,
    ) {
        if !host.is_empty() {
            let outcome = if result.truncated {
                BreakerOutcome::Truncated
            } else {
                BreakerOutcome::Success
            };
            self.breakers.record_outcome(host, k, outcome).await;
            self.preferences.record_success(host).await;
        }
        if let Some(g) = guard.take() {
            g.disarm();
        }
    }

    /// Record a hedge thin/blocked tier's failure side-effects.
    async fn record_hedge_thin(
        &self,
        host: &str,
        k: RendererKind,
        cls: &JsAttemptClass,
        guard: &mut Option<ProbeGuard>,
    ) {
        if !host.is_empty() {
            // Identical rule to the serial loop (`classify_outcome`'s
            // `site_blocked`): the hedge must be provably equivalent to serial,
            // and `cls.hard_block` is the same expression the serial arm derives.
            let outcome = if cls.hard_block && self.has_recovery_tier {
                BreakerOutcome::SiteBlocked
            } else {
                BreakerOutcome::RenderError
            };
            // Host-scoped: this whole function is the thin/blocked verdict, i.e.
            // a judgement about the page rather than about the tier. Same rule
            // the serial thin arm now applies, which is what keeps the hedge
            // provably equivalent to serial.
            self.breakers
                .record_scoped_outcome(host, k, None, Some(outcome))
                .await;
            if k == RendererKind::Lightpanda {
                let err_kind = if cls.is_status_blocked || cls.is_bot_wall || cls.antibot_blocked {
                    FailoverErrorKind::AntibotBlock
                } else {
                    FailoverErrorKind::PlaceholderContent
                };
                let _ = self.preferences.record_failure(host, &err_kind).await;
            }
        }
        // Thin attempt: leave the probe guard armed. Its Drop calls
        // `cancel_probe`, and `record_scoped_outcome` above already cancelled the
        // global probe for the tier it was handed `None` for, so the global side
        // is cancelled twice. Safe (`saturating_sub`, and `lazy_evaluate` clamps
        // `admitted` to `max_probes`) but not the no-op this comment used to
        // claim: the worst case is a half-open tier admitting one extra probe.
        let _ = guard;
    }

    async fn fetch_with_js(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        wait_for_ms: Option<u64>,
        requested_renderer: Option<&str>,
        // True when the pre-ladder cloak-first hint already fired a cloak attempt
        // this request; suppresses the post-ladder recovery arm so one page never
        // burns two `cloak_sem` permits.
        cloak_attempted: bool,
        deadline: crw_core::Deadline,
    ) -> CrwResult<FetchResult> {
        let host = host_of(url);
        let is_user_pinned = matches!(requested_renderer, Some(name) if name != "auto");
        if let Some(pinned) = requested_renderer
            && let Some(kind) = renderer_kind_for(pinned)
        {
            metrics()
                .user_pin_total
                .with_label_values(&[kind.as_str()])
                .inc();
        }

        // Filter the JS pool down to a hard-pinned renderer when one was named.
        // "auto" or `None` means "use the configured chain".
        //
        // A pinned request (`Some(name)` where name != "auto") is matched by
        // exact name and BYPASSES the camoufox auto-exclusion — an explicit
        // `renderer = "camoufox"` pin always reaches the (constructed) tier even
        // when `include_in_auto = false`. The exclusion applies ONLY to the
        // non-pinned auto chain.
        let mut renderers: Vec<&Arc<dyn PageFetcher>> = match requested_renderer {
            Some(name) if name != "auto" => self
                .js_renderers
                .iter()
                .filter(|r| r.name() == name)
                .collect(),
            _ => {
                #[cfg(feature = "camoufox")]
                {
                    let in_auto = self.camoufox_in_auto;
                    self.js_renderers
                        .iter()
                        .filter(|r| in_auto || r.name() != "camoufox")
                        .collect()
                }
                #[cfg(not(feature = "camoufox"))]
                {
                    self.js_renderers.iter().collect()
                }
            }
        };

        // LightPanda has no upstream-proxy support: when a proxy is active for
        // this request, drop it so the rotated/sticky egress IP is honored
        // (vanilla Chrome applies it via a per-context `proxyServer`). Fail
        // CLOSED — if filtering leaves no proxy-capable JS renderer, return a
        // hard error rather than silently navigating direct through LightPanda
        // and leaking the host's real IP.
        let proxy_active = REQUEST_PROXY.try_with(|p| p.is_some()).unwrap_or(false);
        if proxy_active {
            renderers.retain(|r| r.name() != "lightpanda");
            if renderers.is_empty() {
                return Err(CrwError::RendererError(
                    "a proxy is required for this request but the only available JS \
                     renderer (lightpanda) cannot route through a proxy; configure a \
                     chrome/chrome_proxy tier to use proxies with JS rendering"
                        .into(),
                ));
            }
        }

        // Drop the tiers that cannot capture (see `renderer_can_screenshot` —
        // the same predicate `/v1/capabilities` reports) and fail CLOSED if that
        // empties the chain, rather than returning a screenshot-less result the
        // caller asked for (mirrors the proxy retain above). Applies even to a
        // hard pin: pinning camoufox/lightpanda + requesting a screenshot is
        // unsatisfiable.
        if screenshot_requested() {
            renderers.retain(|r| renderer_can_screenshot(r.name()));
            if renderers.is_empty() {
                return Err(CrwError::RendererError(
                    "a screenshot was requested but no CDP-capable Chrome renderer is \
                     available; lightpanda and camoufox cannot capture screenshots — \
                     configure a chrome/chrome_proxy tier"
                        .into(),
                ));
            }
        }
        // Phase 2 (latency-qn): gated auto-egress. Pull chrome_proxy OUT of the
        // normal ladder and hold it as a hard-block-only recovery arm fired ONCE
        // after the ladder (below), with a reserved deadline budget. A naive
        // always-on chrome_proxy ladder tier is net-negative (bench: success
        // −2pp, p90 +69%) because the slow residential tier burns the deadline
        // on every escalation; gating it to genuine hard-blocks keeps the
        // recovery without the regression. Only in auto mode and when the
        // request isn't already proxied (that path wants chrome_proxy in-ladder).
        //
        // NOT when a screenshot was requested: the retain above already dropped
        // every tier that cannot capture, so on an instance whose only capable
        // tier is chrome_proxy this hold-out would empty the ladder and fail a
        // capture that `/v1/capabilities` correctly advertised as supported.
        // Keep it in-ladder for a capture (same as the already-proxied path) and
        // let the latency gating apply to the ordinary, screenshot-less traffic
        // it was measured on.
        let auto_egress_arm: Option<Arc<dyn PageFetcher>> = if self.auto_egress_escalation
            && !is_user_pinned
            && !proxy_active
            && !screenshot_requested()
        {
            let arm = self
                .js_renderers
                .iter()
                .find(|r| r.name() == "chrome_proxy")
                .cloned();
            renderers.retain(|r| r.name() != "chrome_proxy");
            arm
        } else {
            None
        };

        // Auto mode: if this host has been promoted, try Chrome first.
        if !is_user_pinned
            && let Some(RendererKind::Chrome) = self.preferences.preferred(&host).await
        {
            // 3-tier rank: chrome first, then the residential chrome_proxy,
            // then everything lighter. A stable binary key would yield
            // `[chrome, lightpanda, chrome_proxy]` — escalating a chrome
            // block to lightpanda (same WAF, lighter fingerprint) before
            // ever reaching the residential tier.
            renderers.sort_by_key(|r| match r.name() {
                "chrome" => 0,
                "chrome_proxy" => 1,
                _ => 2,
            });
            tracing::debug!(host = %host, "host promoted to chrome by preference learner");
        }

        if renderers.is_empty() {
            let available = self.js_renderer_names();
            return Err(CrwError::RendererError(format!(
                "requested renderer '{}' not in pool [{}]",
                requested_renderer.unwrap_or("auto"),
                available.join(", ")
            )));
        }

        // Track the chain we attempted so we can populate
        // `RenderDecision::Failover` when nothing succeeded outright.
        let mut chain: Vec<RendererKind> = Vec::new();
        let mut breaker_skipped: Vec<RendererKind> = Vec::new();
        let mut last_error = None;
        let mut last_failover_reason: Option<FailoverErrorKind> = None;
        let mut thin_result: Option<FetchResult> = None;
        // Phase 2: did any ladder attempt end in a hard block (egress-recoverable
        // subset: 401/403/429/503/520-530 or a bot-wall/vendor/antibot wall)?
        // Drives the gated chrome_proxy recovery arm below. Excludes
        // 404/410/412/451/500 (a different egress IP won't fix those).
        let mut saw_hard_block = false;
        // Was any attempt a fingerprint-vendor wall chrome_proxy can't clear?
        // Tracked at detection time (not re-detected from the stitched
        // `thin_result`, whose largest-HTML keeper can drop a small vendor shell
        // in a multi-hop chain) so the arm is reliably suppressed on those.
        let mut saw_unrecoverable_wall = false;
        // Snapshot for the leak-through fallback below. The main loop
        // consumes `renderers`; we keep a parallel reference list so a
        // single skipped renderer can still get a shot when its host
        // breaker is closed.
        let renderers_snapshot: Vec<&Arc<dyn PageFetcher>> = renderers.clone();

        // latency-qn conditional hedge: when lightpanda is first (cheap-first, not
        // promoted to chrome) and chrome is present, race them concurrently so
        // chrome's render clock starts immediately instead of after lightpanda
        // fails. Headroom-gated (try_acquire) so it can't deadlock the pool; on no
        // permit / open breaker / both-errored it falls through to the serial loop.
        let mut hedge_done = false;
        if self.chrome_hedge
            && !is_user_pinned
            && !proxy_active
            // Same degenerate-budget guard as the serial loop: the hedge dispatches
            // both CDP tiers directly, bypassing that check. Prod runs with
            // CRW_CHROME_HEDGE=true, so this is load-bearing, not defensive.
            && deadline.remaining() >= MIN_TIER_BUDGET
            && renderers.first().map(|r| r.name()) == Some("lightpanda")
            && renderers.iter().any(|r| r.name() == "chrome")
            && let Ok(_permit) = self.hedge_sem.clone().try_acquire_owned()
        {
            let lp = renderers
                .iter()
                .find(|r| r.name() == "lightpanda")
                .expect("checked above");
            let chrome = renderers
                .iter()
                .find(|r| r.name() == "chrome")
                .expect("checked above");
            match self
                .try_hedge(lp, chrome, url, headers, wait_for_ms, deadline, &host)
                .await
            {
                Ok(Some(HedgeOutcome::Accepted(r))) => return Ok(r),
                Ok(Some(HedgeOutcome::Thin(r, hb, uw))) => {
                    thin_result = Some(r);
                    saw_hard_block |= hb;
                    saw_unrecoverable_wall |= uw;
                    chain.push(RendererKind::Lightpanda);
                    chain.push(RendererKind::Chrome);
                    hedge_done = true;
                }
                // breaker open / both-errored → fall back to the serial loop.
                Ok(None) => {}
                Err(e) => last_error = Some(e),
            }
        }

        for renderer in renderers {
            if hedge_done {
                break;
            }
            let kind = renderer_kind_for(renderer.name());

            // Skip empty hosts: don't pollute breaker/preference caches
            // with the "" key when URL parsing failed.
            let trackable = kind.filter(|_| !host.is_empty());

            // A tier-side skip on a *partial* budget stays removed (86dd10f): letting
            // chrome attempt with a partial-DOM budget beats aborting pre-flight on
            // legitimately-slow tail URLs, and classify_outcome ignores DeadlineClamped
            // so the breaker isn't poisoned. What is reinstated here is narrower: a
            // *degenerate* budget. A CDP attempt cannot even finish its handshake in
            // single-digit milliseconds, so it returns a fabricated `Timeout after 5ms`
            // that pollutes logs and burns a pool slot. Measured in prod: 432 of 536
            // escalations ran with <50ms of budget. Skip those, and only those.
            //
            // Note this is skip-*without*-attempting, distinct from the post-hoc
            // DeadlineClamped classification, which still only applies to tiers that
            // were actually invoked.
            let remaining = deadline.remaining();
            if remaining < MIN_TIER_BUDGET {
                tracing::info!(
                    renderer = renderer.name(),
                    remaining_ms = remaining.as_millis() as u64,
                    "budget below minimum tier budget, skipping renderer"
                );
                if let Some(k) = kind {
                    // Deliberately NOT `breaker_skipped`: that vec means "the circuit
                    // breaker rejected this tier" and gates the leak-through arm.
                    metrics()
                        .render_route_decision_total
                        .with_label_values(&[k.as_str(), "budgetSkipped"])
                        .inc();
                }
                // Preserve the status code a starved request returns today. The tier we
                // are skipping would have been invoked with `remaining`, timed out, and
                // written `CrwError::Timeout` here — overwriting any earlier error, as
                // every other `last_error` assignment in this function does. Assign
                // unconditionally for the same reason: `get_or_insert_with` would let an
                // earlier tier's `RendererError` survive and map to 500 instead of 504.
                //
                // Report the REQUESTED budget, not `remaining`. `remaining` is by
                // definition below MIN_TIER_BUDGET on this branch, and in production it
                // is ~0, so it rendered as the literal `Timeout after 1ms`: 190 times
                // in 12h against 104 tiers that actually ran. That number described no
                // real event, because the preceding CDP tier clamps to `deadline.remaining()`
                // with no reserve, so it had already spent the budget (observed clamps:
                // 19239ms, 23348ms, 27932ms, 29335ms). A caller given 30s was told its
                // request timed out after 1ms, which is what `Deadline::requested_ms`
                // exists to prevent.
                last_error = Some(CrwError::Timeout(deadline.requested_ms()));
                continue;
            }

            // Consult breaker for tracked renderers. Untracked names (e.g.
            // "playwright") bypass the breaker for now.
            let mut probe_guard: Option<ProbeGuard> = None;
            if let Some(k) = trackable {
                let (permit, guard) = self.breakers.acquire_with_guard(&host, k).await;
                if permit == Permit::Rejected {
                    tracing::info!(
                        renderer = renderer.name(),
                        host = %host,
                        "circuit breaker open, skipping renderer"
                    );
                    metrics()
                        .render_route_decision_total
                        .with_label_values(&[k.as_str(), "breakerSkipped"])
                        .inc();
                    breaker_skipped.push(k);
                    drop(guard); // not Probe — drop is a no-op
                    continue;
                }
                probe_guard = Some(guard);
            }

            // `acquire_with_guard` awaits, so the budget may have drained while we
            // waited for a breaker permit. Re-check before dispatching, or the floor
            // above is only advisory. Dropping `probe_guard` here cancels the probe
            // (see `ProbeGuard::drop`), so the breaker is left as we found it.
            let remaining = deadline.remaining();
            if remaining < MIN_TIER_BUDGET {
                tracing::info!(
                    renderer = renderer.name(),
                    remaining_ms = remaining.as_millis() as u64,
                    "budget drained while acquiring breaker permit, skipping renderer"
                );
                if let Some(k) = kind {
                    metrics()
                        .render_route_decision_total
                        .with_label_values(&[k.as_str(), "budgetSkipped"])
                        .inc();
                }
                // Same reasoning as the floor above: report the requested budget,
                // never the sub-minimum remainder.
                last_error = Some(CrwError::Timeout(deadline.requested_ms()));
                continue;
            }

            if let Some(k) = kind {
                chain.push(k);
            }

            // Capture pre-call context so post-await classification is
            // race-free against deadline drift.
            let attempt_ctx = {
                let remaining = deadline.remaining();
                let tier_budget = kind
                    .and_then(|k| self.tier_timeouts.get(&k).copied())
                    .unwrap_or(remaining);
                AttemptContext::capture(remaining, tier_budget)
            };
            // Phase 1 (latency-qn): per-attempt timing. The whole-fetch wrapper
            // only records total + accepted tier; this records each tier's
            // wall time + outcome so a bench run can tell whether the p90 tail
            // is stacked failed-tier time (a hedge would cut it) or the final
            // accepted render itself (a hedge would NOT). Feeds the Phase 1.5
            // kill-gate. Off in prod (gated by `latency_breakdown`).
            let attempt_start = std::time::Instant::now();
            let attempt_outcome = renderer.fetch(url, headers, wait_for_ms, deadline).await;
            if self.latency_breakdown {
                let attempt_ms = attempt_start.elapsed().as_millis() as u64;
                let tier = renderer.name();
                match &attempt_outcome {
                    Ok(r) => tracing::info!(
                        target: "latency_breakdown",
                        url, tier, attempt_ms,
                        status = r.status_code,
                        html_len = r.html.len(),
                        "hedge attempt"
                    ),
                    Err(e) => tracing::info!(
                        target: "latency_breakdown",
                        url, tier, attempt_ms,
                        error = %e,
                        "hedge attempt (error)"
                    ),
                }
            }
            match attempt_outcome {
                Ok(mut result) => {
                    let text_len = html_body_text_len(&result.html);
                    let is_placeholder = detector::looks_like_loading_placeholder(&result.html);
                    let failed_render = detector::looks_like_failed_render(&result.html);
                    let is_bot_wall =
                        detector::looks_like_generic_bot_wall(&result.html, result.truncated);
                    let vendor_block = detector::looks_like_vendor_block(&result.html);
                    // Size-independent Cloudflare interstitial check (see
                    // `classify_js_attempt`): large managed-challenge pages evade
                    // the size-capped detectors above, so a CF challenge would
                    // otherwise slip through as "thin" and never arm chrome_proxy.
                    let cf_challenge = detector::looks_like_cloudflare_challenge(&result.html);
                    // Mirrors the HTTP-tier escalation set (lib.rs:658). A JS
                    // renderer can return 200 with bot HTML or 403 with content
                    // — without this check, both slip through as "valid".
                    let is_status_blocked = matches!(
                        result.status_code,
                        401 | 403 | 404 | 405 | 406 | 410 | 412 | 429 | 451 | 500 | 503
                    );
                    // The comprehensive 3-tier antibot classifier. The
                    // `detector` heuristics above only know a fixed phrase
                    // list + 8 named vendors; `classify()` additionally
                    // recognises Reddit-class WAF pages ("blocked by network
                    // security") served with HTTP 200 that otherwise slip
                    // through as success. Always runs for telemetry when
                    // `enabled`; only forces escalation when
                    // `escalate_in_failover` is on (the kill switch).
                    let antibot = if self.antibot.enabled {
                        crw_extract::antibot::classify(Some(result.status_code), &result.html)
                    } else {
                        crw_extract::antibot::AntibotResult::none()
                    };
                    let antibot_blocked =
                        self.antibot.escalate_in_failover && antibot.signal.is_blocked();
                    // Phase 2: track hard-block (egress-recoverable) outcomes for
                    // the gated chrome_proxy arm. Hard-block status subset only
                    // (not 404/410/412/451/500) + interstitial walls.
                    if is_fingerprint_vendor_wall(cf_challenge, vendor_block, antibot.signal) {
                        saw_unrecoverable_wall = true;
                    }
                    if is_hard_block_status(result.status_code)
                        || is_bot_wall
                        || vendor_block.is_some()
                        || cf_challenge
                        || antibot.signal.is_blocked()
                    {
                        saw_hard_block = true;
                    }
                    if text_len >= Self::MIN_RENDERED_TEXT_LEN
                        && !is_placeholder
                        && failed_render.is_none()
                        && !is_bot_wall
                        && vendor_block.is_none()
                        && !cf_challenge
                        && !is_status_blocked
                        && !antibot_blocked
                    {
                        // Capture the promotion state BEFORE record_success
                        // clears the latch — otherwise AutoPromoted decisions
                        // race against the success path and downgrade to AutoDefault.
                        let was_promoted = matches!(
                            self.preferences.preferred(&host).await,
                            Some(RendererKind::Chrome)
                        );
                        if let Some(k) = trackable {
                            // Treat truncated-but-valid as Truncated (ignored
                            // by default per BreakerConfig.count_truncated_as_failure).
                            let outcome = if result.truncated {
                                BreakerOutcome::Truncated
                            } else {
                                BreakerOutcome::Success
                            };
                            self.breakers.record_outcome(&host, k, outcome).await;
                            self.preferences.record_success(&host).await;
                            metrics()
                                .render_route_decision_total
                                .with_label_values(&[k.as_str(), "success"])
                                .inc();
                            metrics()
                                .host_preferences_size
                                .set(self.preferences.size() as i64);
                        }
                        if let Some(g) = probe_guard.take() {
                            g.disarm();
                        }
                        // Populate routing metadata + per-renderer credit.
                        if let Some(k) = kind {
                            result.credit_cost = credit_for(k);
                            result.render_decision = Some(if is_user_pinned {
                                RenderDecision::UserPinned { renderer: k }
                            } else if !breaker_skipped.is_empty() {
                                RenderDecision::BreakerSkipped {
                                    skipped: breaker_skipped[0],
                                    chosen: k,
                                }
                            } else if chain.len() > 1 {
                                RenderDecision::Failover {
                                    chain: chain.clone(),
                                    reason: last_failover_reason
                                        .clone()
                                        .unwrap_or(FailoverErrorKind::Other),
                                }
                            } else if was_promoted && k == RendererKind::Chrome {
                                RenderDecision::AutoPromoted {
                                    chosen: k,
                                    from: RendererKind::Lightpanda,
                                    reason: "host preference learner".into(),
                                }
                            } else {
                                RenderDecision::AutoDefault { chosen: k }
                            });
                        }
                        return Ok(result);
                    }
                    // Treat thin/placeholder/failed as a soft failure for
                    // breaker + preference purposes.
                    let err_kind = match failed_render {
                        Some(detector::FailedRenderReason::NextJsClientError) => {
                            FailoverErrorKind::NextJsClientError
                        }
                        Some(detector::FailedRenderReason::ReactMinifiedError) => {
                            FailoverErrorKind::NextJsClientError
                        }
                        Some(detector::FailedRenderReason::EmptyNextRoot) => {
                            FailoverErrorKind::EmptyNextRoot
                        }
                        None if vendor_block.is_some() => FailoverErrorKind::VendorBlock,
                        None if is_status_blocked => FailoverErrorKind::StatusBlocked,
                        None if is_placeholder => FailoverErrorKind::PlaceholderContent,
                        None if is_bot_wall => FailoverErrorKind::PlaceholderContent,
                        // The classifier caught a block the detector missed.
                        None if antibot_blocked => FailoverErrorKind::AntibotBlock,
                        None => FailoverErrorKind::PlaceholderContent,
                    };
                    last_failover_reason = Some(err_kind.clone());
                    if let Some(k) = trackable {
                        // Thin/placeholder/failed render → classify against
                        // attempt context so deadline-clamped attempts don't
                        // poison the breaker.
                        //
                        // A site-side block is not a tier failure: every tier
                        // egressing from this IP sees the same wall, so counting
                        // it tripped the per-host breaker for lightpanda AND
                        // chrome and left the ladder with nothing to run.
                        //
                        // Computed FRESH here, never from `saw_hard_block` — that
                        // flag is cumulative across ladder iterations, so reading
                        // it would let lightpanda's block mask a genuine chrome
                        // render failure on the same host.
                        //
                        // Derived from the raw signals rather than `err_kind`:
                        // that mapping is lossy (`is_bot_wall` lands on
                        // `PlaceholderContent` and `cf_challenge` is absent), and
                        // the bot-wall case is exactly how Wikimedia serves its
                        // HTTP-200 datacenter ban. Mirrors `JsAttemptClass::
                        // hard_block`, which deliberately omits 404/405/406/410/
                        // 412/451/500 — those are not site-side blocks.
                        let site_blocked = is_hard_block_status(result.status_code)
                            || is_bot_wall
                            || vendor_block.is_some()
                            || cf_challenge
                            || antibot.signal.is_blocked();
                        let outcome = classify_outcome(
                            false,
                            false,
                            false,
                            site_blocked && self.has_recovery_tier,
                            &attempt_ctx,
                        );
                        // Host-scoped: the tier answered, and this is a verdict on
                        // the body it returned. `SiteBlocked` was already ignored
                        // globally; a thin body that is NOT blocked was not, and
                        // that gap is what a single hard domain filled.
                        self.breakers
                            .record_scoped_outcome(&host, k, None, Some(outcome))
                            .await;
                        if k == RendererKind::Lightpanda
                            && let Some(target) =
                                self.preferences.record_failure(&host, &err_kind).await
                        {
                            metrics()
                                .host_preferences_promotions_total
                                .with_label_values(&[k.as_str(), target.as_str()])
                                .inc();
                            tracing::info!(
                                host = %host,
                                "host promoted by preference learner: {} -> {}",
                                k.as_str(),
                                target.as_str()
                            );
                        }
                    }
                    if let Some(g) = probe_guard.take() {
                        g.disarm();
                    }
                    if let Some(vendor) = vendor_block {
                        metrics()
                            .vendor_block_total
                            .with_label_values(&[vendor])
                            .inc();
                        tracing::warn!(
                            renderer = renderer.name(),
                            url,
                            vendor,
                            "vendor anti-bot block detected"
                        );
                    }
                    // Emit the antibot signal regardless of `escalate_in_failover`
                    // — a pre-flip dashboard of escalation pressure.
                    if antibot.signal.is_blocked() {
                        metrics()
                            .antibot_escalation_total
                            .with_label_values(&[antibot.signal.class_name()])
                            .inc();
                        tracing::warn!(
                            renderer = renderer.name(),
                            url,
                            signal = antibot.signal.class_name(),
                            reason = %antibot.reason,
                            status_code = result.status_code,
                            text_len,
                            escalated = antibot_blocked,
                            "antibot classifier flagged a block"
                        );
                    }
                    tracing::info!(
                        renderer = renderer.name(),
                        text_len,
                        is_placeholder,
                        is_bot_wall,
                        vendor_block,
                        is_status_blocked,
                        antibot_signal = antibot.signal.class_name(),
                        antibot_blocked,
                        status_code = result.status_code,
                        failed_render = ?failed_render,
                        "JS renderer returned thin/placeholder/failed content, trying next renderer"
                    );
                    // Annotate the result so it can surface through `thin_result`
                    // if no later renderer succeeds. Preserves any warning the
                    // renderer set, but adds the failover reason. We keep the
                    // first thin result as the body to return (no point in
                    // accumulating bodies), but stitch later renderers'
                    // warnings onto it so debug output reflects every attempt.
                    let mut annotated = result;
                    let attempt_warning = if let Some(reason) = failed_render {
                        format!(
                            "{} returned a failed render ({})",
                            renderer.name(),
                            reason.as_str()
                        )
                    } else if is_placeholder {
                        format!("{} returned a loading placeholder", renderer.name())
                    } else if let Some(vendor) = vendor_block {
                        format!(
                            "{} returned a vendor anti-bot block ({vendor})",
                            renderer.name()
                        )
                    } else if is_bot_wall {
                        format!(
                            "{} returned a generic anti-bot interstitial",
                            renderer.name()
                        )
                    } else if is_status_blocked {
                        format!(
                            "{} returned HTTP {} (treated as blocked)",
                            renderer.name(),
                            annotated.status_code
                        )
                    } else if antibot_blocked {
                        format!(
                            "{} returned an anti-bot block ({}: {})",
                            renderer.name(),
                            antibot.signal.class_name(),
                            antibot.reason
                        )
                    } else {
                        format!(
                            "{} returned thin content (text_len={text_len})",
                            renderer.name()
                        )
                    };
                    if is_bot_wall || vendor_block.is_some() || is_status_blocked || antibot_blocked
                    {
                        // Surface bot-wall as a RendererError so, if every
                        // renderer in the chain hits a wall, the final error
                        // (line ~1052) carries an actionable message.
                        // RendererError maps to FailoverErrorKind::LightpandaCrash
                        // via classify_renderer_error — that's intentional:
                        // bot-wall hosts SHOULD be promoted to Chrome by the
                        // host preference learner, since LightPanda lacks the
                        // TLS/header fingerprint to clear them.
                        let msg = if let Some(v) = vendor_block {
                            format!("{} returned a vendor anti-bot block ({v})", renderer.name())
                        } else if is_status_blocked {
                            format!(
                                "{} returned HTTP {} (treated as blocked)",
                                renderer.name(),
                                annotated.status_code
                            )
                        } else if is_bot_wall {
                            format!(
                                "{} returned a generic anti-bot interstitial",
                                renderer.name()
                            )
                        } else {
                            format!(
                                "{} returned an anti-bot block ({}: {})",
                                renderer.name(),
                                antibot.signal.class_name(),
                                antibot.reason
                            )
                        };
                        last_error = Some(CrwError::RendererError(msg));
                    }
                    annotated.warnings.push(attempt_warning.clone());
                    annotated.warning = Some(match annotated.warning {
                        Some(prev) => format!("{prev}; {attempt_warning}"),
                        None => attempt_warning.clone(),
                    });
                    thin_result = Some(match thin_result {
                        None => annotated,
                        Some(existing) => {
                            // Prefer the larger HTML when stitching thin
                            // results — a later renderer (e.g. chrome) often
                            // returns a CAPTCHA shell that, while small,
                            // contains anti-bot markers absent from an even
                            // smaller earlier shell. Diagnostics & block
                            // detection then have something to match on.
                            let (mut keeper, dropped) =
                                if annotated.html.len() > existing.html.len() {
                                    (annotated, existing)
                                } else {
                                    (existing, annotated)
                                };
                            keeper.warnings.push(attempt_warning.clone());
                            keeper.warning = Some(match keeper.warning {
                                Some(prev) => format!("{prev}; {attempt_warning}"),
                                None => attempt_warning,
                            });
                            // Carry over any extra warnings from the dropped
                            // attempt so debug output stays complete.
                            for w in dropped.warnings {
                                if !keeper.warnings.contains(&w) {
                                    keeper.warnings.push(w);
                                }
                            }
                            keeper
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(renderer = renderer.name(), "JS renderer failed: {e}");
                    let err_kind = classify_renderer_error(&e);
                    last_failover_reason = Some(err_kind.clone());
                    if let Some(k) = trackable {
                        let was_timeout = matches!(e, CrwError::Timeout(_));
                        // Reaching this arm means the renderer errored with no
                        // response to inspect. That used to be read as proof the
                        // TIER is sick, and the outcome went to the global window
                        // as well as the host one. It is not proof: a dead origin
                        // produces exactly the same shape. Six hours of production
                        // held 24 net::ERR_ABORTED, 10 PeerFailedVerification,
                        // 5 ERR_CERT_DATE_INVALID, 3 ERR_CONNECTION_RESET,
                        // 2 ERR_CONNECTION_REFUSED and 2 CouldntConnect, every one
                        // of them the target's fault and every one advancing the
                        // global window. Two of the four observed global trips came
                        // from here, with the cause logged 0.6ms and 0.2ms earlier.
                        //
                        // An origin failure says nothing about this tier, because
                        // every tier egressing from this box sees the same thing.
                        // That is the same reasoning `SiteBlocked` already rests
                        // on, and `record_scoped_outcome` exists for exactly it.
                        //
                        // The Timeout arm is excluded deliberately: a hung CDP pool
                        // also times out, and that IS a tier signal. It is what
                        // still catches a renderer that is genuinely down.
                        let outcome =
                            classify_outcome(false, false, was_timeout, false, &attempt_ctx);
                        let global =
                            (!(is_origin_fault_for_breaker(&e) && !was_timeout)).then_some(outcome);
                        self.breakers
                            .record_scoped_outcome(&host, k, global, Some(outcome))
                            .await;
                        if k == RendererKind::Lightpanda {
                            let _ = self.preferences.record_failure(&host, &err_kind).await;
                        }
                    }
                    if let Some(g) = probe_guard.take() {
                        g.disarm();
                    }
                    last_error = Some(e);
                    continue;
                }
            }
        }
        // Leak-through fallback: every renderer was rejected by the global
        // breaker, but the host itself has no failures recorded. Rather
        // than fail the request outright (which is what made the bench
        // shed ~12% on broad lightpanda outages), give one renderer a
        // single attempt without recording its outcome to the global
        // window. The host tier still records, so a host that's actually
        // broken trips its own breaker on the next attempt.
        // Trigger when every chain attempt failed outright (no thin_result,
        // no Ok return) AND at least one renderer was skipped by the global
        // breaker. Common case: lightpanda runs and errors, chrome gets
        // globally rejected → without leak we'd return error even though
        // chrome's host breaker is clean and would likely succeed.
        //
        // Skip when the request deadline is already (near-)exhausted:
        // entering a renderer with <500ms budget produced 37/128 of the
        // first leak run's failures as "Timeout after 1-2ms" — the
        // attempt cannot succeed and just consumes a CDP connection.
        // (Same reasoning now guards the main ladder loop; see MIN_TIER_BUDGET.)
        if thin_result.is_none()
            && !breaker_skipped.is_empty()
            && !is_user_pinned
            && deadline.remaining() >= MIN_TIER_BUDGET
        {
            for renderer in &renderers_snapshot {
                let kind = renderer_kind_for(renderer.name());
                let trackable = kind.filter(|_| !host.is_empty());
                let Some(k) = trackable else { continue };
                if !breaker_skipped.contains(&k) {
                    continue;
                }
                let permit = self.breakers.try_acquire_host_only(&host, k).await;
                if permit == Permit::Rejected {
                    continue;
                }
                // That acquire awaits; re-check the budget before dispatching so the
                // floor above is not merely advisory (same TOCTOU as the serial loop).
                if deadline.remaining() < MIN_TIER_BUDGET {
                    continue;
                }
                tracing::info!(
                    renderer = renderer.name(),
                    host = %host,
                    "global breaker open, host clean — leaking through one attempt"
                );
                metrics()
                    .render_route_decision_total
                    .with_label_values(&[k.as_str(), "leakThrough"])
                    .inc();
                let attempt_ctx = {
                    let remaining = deadline.remaining();
                    let tier_budget = self.tier_timeouts.get(&k).copied().unwrap_or(remaining);
                    AttemptContext::capture(remaining, tier_budget)
                };
                let res = renderer.fetch(url, headers, wait_for_ms, deadline).await;
                match res {
                    Ok(mut result) => {
                        // One shared classification instead of four hand-rolled
                        // detector calls: it feeds the accept gate, the breaker
                        // outcome, and the recovery-arm flags below.
                        let cls = self.classify_js_attempt(&result);
                        let text_len = cls.text_len;
                        let truncated = result.truncated;
                        // The ladder ran nothing, so the recovery arm's flags are
                        // still false and it could not fire even though this
                        // attempt just proved the page is walled. Seed BOTH — a
                        // lone `hard_block` would fire the slow residential tier
                        // on a fingerprint wall it cannot clear (the regression
                        // ff09f30 was tuned against: success -2pp, p90 +69%).
                        saw_hard_block |= cls.hard_block;
                        saw_unrecoverable_wall |= cls.unrecoverable_wall;
                        // A large CF challenge shell has body text > 50 and no
                        // placeholder/failed marker, so guard it explicitly or it
                        // would leak through this path as success. Same for a
                        // generic bot wall (a Wikimedia ban shell clears the text
                        // threshold) and a vendor block.
                        //
                        // Body detectors only — deliberately NOT `cls.acceptable`,
                        // which also rejects `is_status_blocked` and would discard
                        // a 403 that carries the real page. That behaviour is
                        // intentional (see `crw_crawl::single`).
                        let content_ok = text_len >= Self::MIN_RENDERED_TEXT_LEN
                            && !cls.is_placeholder
                            && cls.failed_render.is_none()
                            && !cls.is_bot_wall
                            && cls.vendor_block.is_none()
                            // Covers the CF interstitial AND the vendor walls that
                            // `classify` recognises from visible text alone (a
                            // PerimeterX/Imperva page with no SDK marker), which
                            // clear the 50-char threshold and evade the two lighter
                            // detectors above.
                            //
                            // Deliberately NOT the broader `cls.antibot_blocked`:
                            // `classify` returns GenericBlock for ANY 403 with a
                            // non-data body regardless of how substantial it is
                            // (`antibot.rs`), so gating on it would discard a 403
                            // that carries the real page — which is accepted on
                            // purpose (see `crw_crawl::single`).
                            && !cls.unrecoverable_wall;
                        // Same rule as the serial loop: a wall is not this tier's
                        // fault. Without it the leak arm — which runs precisely
                        // when breakers are already stressed — keeps advancing the
                        // host window that F1 exists to protect.
                        let outcome = classify_outcome(
                            content_ok,
                            truncated,
                            false,
                            cls.hard_block && self.has_recovery_tier,
                            &attempt_ctx,
                        );
                        // Record host only — global stays untouched so the
                        // existing trip can finish its cooldown naturally.
                        self.breakers
                            .record_scoped_outcome(&host, k, None, Some(outcome))
                            .await;
                        if content_ok {
                            result.credit_cost = credit_for(k);
                            result.render_decision =
                                Some(RenderDecision::AutoDefault { chosen: k });
                            return Ok(result);
                        }
                        // Thin/placeholder/blocked on the leak path → fall through
                        // to the normal "no JS renderer" return below.
                        //
                        // Keep the body as the thin candidate rather than dropping
                        // it. The tail returns `Err(last_error)` when `thin_result`
                        // is None, and the `render_js = true` branch propagates that
                        // straight to the caller — it has no "JS failed, fall back to
                        // the HTTP body" net, unlike the auto branch. Dropping a
                        // rejected body would therefore turn a response this path
                        // used to return as `Ok` into a 5xx; `classify_block`
                        // downstream still surfaces it as blocked and suppresses
                        // billing, which is the pre-existing contract.
                        // `best-result-wins` is unaffected: the leak arm only runs
                        // when `thin_result` is None.
                        last_error = Some(CrwError::RendererError(format!(
                            "leak attempt on {} returned thin content (text_len={text_len})",
                            renderer.name()
                        )));
                        thin_result = Some(result);
                        break;
                    }
                    Err(e) => {
                        let was_timeout = matches!(e, CrwError::Timeout(_));
                        // No response body to inspect → a genuine tier signal.
                        let outcome =
                            classify_outcome(false, false, was_timeout, false, &attempt_ctx);
                        self.breakers
                            .record_scoped_outcome(&host, k, None, Some(outcome))
                            .await;
                        last_error = Some(e);
                        break;
                    }
                }
            }
        }

        // Phase 2 (latency-qn): gated auto-egress recovery. chrome_proxy was held
        // out of the ladder; fire it ONCE iff the ladder hit a hard block AND the
        // deadline can still absorb a full chrome_proxy attempt (so it never
        // causes a timeout the baseline wouldn't have — the failure mode the
        // naive always-on ladder tier showed: success −2pp, p90 +69%).
        // best-result-wins: never replace usable content with an empty retry.
        //
        // CF-challenge routing: a managed Turnstile challenge cannot be cleared
        // by chrome_proxy (default Chrome fingerprint), so route it STRAIGHT to
        // the cloak recovery arm and suppress chrome_proxy for it. Re-detect the
        // signal once from `thin_result` (the richest-HTML thin body; a CF
        // challenge is 100-300KB so it wins the size race) — this covers both
        // the serial and hedge paths without threading a new bool. `route_to_cloak`
        // is a cfg-const that folds to `false` in a lean build → chrome_proxy
        // firing is byte-identical there.
        #[cfg(feature = "cloak")]
        // The firewall-block veto is scoped to THIS decision on purpose.
        // `looks_like_cloudflare_challenge` has a dozen other callers that use it
        // as an "is this body still bad" accept gate, where a 1020 must keep
        // reading as bad; changing it there would let a block page ship as
        // content. The only decision it gets wrong is this one: whether to spend
        // a Turnstile solve, which a 1020 can never satisfy.
        let saw_cf_challenge = thin_result
            .as_ref()
            .map(|r| {
                detector::looks_like_cloudflare_challenge(&r.html)
                    && !detector::looks_like_cloudflare_firewall_block(&r.html)
            })
            .unwrap_or(false);
        let route_to_cloak = {
            #[cfg(feature = "cloak")]
            {
                // `!cloak_attempted`: if the pre-ladder cloak-first hint already
                // fired a cloak attempt this request, do NOT fire the recovery arm
                // again (one page, one cloak permit).
                saw_cf_challenge && self.cloak_arm.is_some() && !cloak_attempted
            }
            #[cfg(not(feature = "cloak"))]
            {
                let _ = cloak_attempted;
                let _ = self.cloak_recover_on_cf;
                let _ = self.cloak_after_egress;
                false
            }
        };
        // Set by the auto-egress arm when its OWN body is a Cloudflare
        // challenge. That is the case `route_to_cloak` structurally cannot see:
        // it is decided from the ladder's body, above, and on an origin that
        // WAF-blocks datacenter IPs the challenge only appears once the request
        // comes from a residential exit.
        #[cfg(feature = "cloak")]
        let mut arm_uncovered_cf = false;
        if let Some(arm) = auto_egress_arm {
            let kind = RendererKind::ChromeProxy;
            // chrome_proxy is default-Chrome + residential IP: it recovers
            // IP-reputation blocks (429 / IP-ban 403 / generic bot walls / CF
            // error-1020 / Wikimedia origin bans) but CANNOT clear a
            // fingerprint-vendor challenge — CF managed challenge, DataDome,
            // PerimeterX, Kasada, Akamai, Imperva — which needs the stealth
            // (camoufox/cloak) tier. Firing the slow residential tier on those
            // just burns the deadline (the p90 regression this arm was tuned
            // against: success −2pp, p90 +69%), so suppress it there.
            // `saw_unrecoverable_wall` is tracked at detection time across the
            // serial and hedge paths (see `is_fingerprint_vendor_wall`); it works
            // in a lean (no-cloak) build where `route_to_cloak` is always false,
            // and unlike re-detecting from `thin_result` it can't miss a small
            // vendor shell that the largest-HTML stitch dropped mid-chain.
            //
            // Load-shed (non-blocking, mirrors the cloak arm): the residential
            // pool blocking-acquires ~pool_size slots, so a burst of
            // datacenter-blocked URLs (batch/crawl) would otherwise queue every
            // page on it for up to the SaaS 120s timeout and collapse co-tenant
            // throughput. No permit → skip recovery and return the block. There is
            // deliberately NO `deadline.remaining()` gate: the ladder leaves the
            // shared deadline near-exhausted (~2s of a 15s scrape deadline), which
            // starved the arm below any floor and is exactly why it never fired.
            //
            // The `arm_sem == chrome_proxy conn_semaphore == chrome_proxy_pool_size()`
            // sizing (decoupled from the general `pool_size`, see
            // `Config::chrome_proxy_pool_size`) makes
            // "a permit implies a free pool slot" hold on the managed prod path,
            // where `chrome_proxy` is arm-EXCLUSIVE (`!proxy_active` retained it out
            // of the ladder). In a mixed self-host config that ALSO runs
            // `chrome_proxy` in-ladder (a per-request proxy / pin / screenshot), an
            // arm can win a permit yet briefly wait on `conn_semaphore` behind
            // in-ladder traffic — still bounded by the arm's own ~12s fetch timeout,
            // never a deadlock or a 120s queue.
            let arm_wanted = saw_hard_block
                && !route_to_cloak
                && !saw_unrecoverable_wall
                && !self.breakers.host_for(&host, kind).await.is_open();
            let arm_permit = if arm_wanted {
                self.chrome_proxy_arm_sem.clone().try_acquire_owned().ok()
            } else {
                None
            };
            if arm_wanted && arm_permit.is_none() {
                // Wanted to recover but the residential pool was saturated — shed
                // (return the block) rather than queue. Counted so the shed rate is
                // observable: a rising `armShed` is the signal the chrome_proxy pool
                // is undersized under sustained hard-block load.
                metrics()
                    .render_route_decision_total
                    .with_label_values(&[kind.as_str(), "armShed"])
                    .inc();
            }
            if let Some(_permit) = arm_permit {
                chain.push(kind);
                let entry = self.pick_proxy_for_url(url);
                // Fresh budget (not the exhausted shared deadline) so the
                // residential connect + nav + render can actually complete; the
                // SaaS→engine fetch tolerates up to 120s. Effective render budget
                // is min(CHROME_PROXY_ARM_BUDGET_MS, chrome_nav_budget_ms).
                let arm_deadline = crw_core::Deadline::now_plus(std::time::Duration::from_millis(
                    CHROME_PROXY_ARM_BUDGET_MS,
                ));
                let attempt = REQUEST_PROXY
                    .scope(entry, arm.fetch(url, headers, wait_for_ms, arm_deadline))
                    .await;
                match attempt {
                    Ok(r) => {
                        let r_text = html_body_text_len(&r.html);
                        // Block detection is load-bearing here, not defensive: a
                        // residential exit can land in the SAME ban range as the
                        // box (or be challenged on its own), and this gate drives
                        // both the ChromeProxy breaker outcome and `better` below.
                        // Without it a ban shell is recorded Success — so the arm
                        // keeps firing on a host it provably cannot serve — and,
                        // being larger than the ladder's thin result, replaces it.
                        // CF is checked too: a managed challenge is 100-300KB and
                        // evades the size-capped vendor detector.
                        // Hoisted out of the `&&` chain below rather than read
                        // off it: `r_ok` short-circuits, and the CF check was
                        // its LAST term, so a small challenge shell (which
                        // carries "Enable JavaScript and cookies to continue",
                        // verbatim in the generic-wall phrase list) exits at
                        // `looks_like_generic_bot_wall` and never reaches it.
                        // Reading the verdict off `!r_ok` instead would be
                        // worse: that is true for a DataDome shell, a vendor
                        // wall, a placeholder and anything under the text
                        // floor, and would buy every one of them a cold
                        // interactive solve.
                        let r_cf_challenge = detector::looks_like_cloudflare_challenge(&r.html);
                        let r_ok = r_text >= Self::MIN_RENDERED_TEXT_LEN
                            && detector::looks_like_failed_render(&r.html).is_none()
                            && !detector::looks_like_loading_placeholder(&r.html)
                            && !detector::looks_like_generic_bot_wall(&r.html, r.truncated)
                            && detector::looks_like_vendor_block(&r.html).is_none()
                            && !r_cf_challenge;
                        // The challenge this arm uncovered is the ONLY thing
                        // that can open the cloak gate below. Set under cfg so
                        // the lean build neither binds nor assigns it.
                        #[cfg(feature = "cloak")]
                        {
                            if r_cf_challenge {
                                arm_uncovered_cf = true;
                                // Counted whether or not the flag is on, so the
                                // prod-wide arrival rate of this shape is readable
                                // on real traffic BEFORE the route is enabled.
                                metrics()
                                    .render_route_decision_total
                                    .with_label_values(&[
                                        RendererKind::Cloak.as_str(),
                                        "afterEgressEligible",
                                    ])
                                    .inc();
                            }
                        }
                        if !host.is_empty() {
                            // Same split as the other content verdicts: success
                            // globally, the page judgement host-only. This arm had
                            // no SiteBlocked path at all, so a wall it could not
                            // clear was booked as a renderer fault unconditionally.
                            if r_ok {
                                self.breakers
                                    .record_outcome(&host, kind, BreakerOutcome::Success)
                                    .await;
                            } else {
                                // A wall is not a renderer fault. `SiteBlocked`
                                // exists for exactly this (see `breaker.rs`: an
                                // anti-bot wall, a vendor block or a Cloudflare
                                // interstitial "says nothing about tier
                                // health") and is the one outcome that neither
                                // fails nor advances the window.
                                //
                                // Load-bearing rather than tidiness: now that
                                // the challenge this arm uncovers opens the
                                // cloak gate, booking it `RenderError` would
                                // open the ChromeProxy host breaker after ~50
                                // walled pages (min_calls 50, failure rate
                                // 0.80). `arm_wanted` would go false, the arm
                                // would stop firing, and the challenge would
                                // stop being uncovered: the recovery would
                                // switch itself off on exactly the hosts it
                                // exists for, and the better it worked the
                                // faster it would happen.
                                // Only the CF-challenge shape, and only while the
                                // flag routes it onward: a DataDome or PerimeterX
                                // wall must still open the ChromeProxy host breaker
                                // after 50 calls (cloak never fires for those), or a
                                // crawl of such a host pays ladder + 20s residential
                                // Chrome on every page instead of 50. With the flag
                                // off this is the same RenderError call as before.
                                let cf_routed_onward = {
                                    #[cfg(feature = "cloak")]
                                    {
                                        r_cf_challenge
                                            && self.cloak_after_egress
                                            && self.cloak_arm.is_some()
                                    }
                                    #[cfg(not(feature = "cloak"))]
                                    {
                                        false
                                    }
                                };
                                let verdict = if cf_routed_onward {
                                    BreakerOutcome::SiteBlocked
                                } else {
                                    BreakerOutcome::RenderError
                                };
                                self.breakers
                                    .record_scoped_outcome(&host, kind, None, Some(verdict))
                                    .await;
                            }
                        }
                        // best-result-wins vs the ladder's thin_result: ONLY take
                        // the proxy result if it is content-OK (red line: a thin/
                        // empty proxy result must never turn a baseline Err into an
                        // Ok(empty), nor replace a usable thin_result). The `None`
                        // case is gated on r_ok too, else an all-tiers-errored run
                        // would ship an empty proxy body as success.
                        let better = r_ok
                            && match &thin_result {
                                Some(prev) => r.html.len() > prev.html.len(),
                                None => true,
                            };
                        if self.latency_breakdown {
                            tracing::info!(
                                target: "latency_breakdown",
                                url, tier = "chrome_proxy",
                                ok = r_ok, consumed = better,
                                "auto_egress fired"
                            );
                        }
                        if better {
                            thin_result = Some(r);
                        }
                    }
                    Err(e) => {
                        if !host.is_empty() {
                            // Same split as the cloak-first arm: a body the tier
                            // returned and we rejected is the page's verdict, an
                            // unreachable sidecar or proxy is ours. The predicate
                            // matches only cloak's own content messages, so this
                            // arm keeps recording a real transport failure
                            // globally.
                            let global = (!is_cloak_content_verdict(&e))
                                .then_some(BreakerOutcome::ConnectionError);
                            self.breakers
                                .record_scoped_outcome(
                                    &host,
                                    kind,
                                    global,
                                    Some(BreakerOutcome::ConnectionError),
                                )
                                .await;
                        }
                        if self.latency_breakdown {
                            tracing::info!(
                                target: "latency_breakdown",
                                url, tier = "chrome_proxy", error = %e,
                                "auto_egress fired (error)"
                            );
                        }
                    }
                }
            }
        }

        // Cloak Turnstile recovery arm — fired ONLY on a detected CF challenge,
        // with a DECOUPLED floor (`CLOAK_ARM_FLOOR_MS`, one cold solve) so it can
        // arm even though the per-attempt budget is larger. Load-shed via
        // `cloak_sem` (non-blocking); pre-fire read-only breaker check; and
        // best-result-wins IDENTICAL to the chrome_proxy arm above so an Err/thin
        // cloak result never turns the baseline CF-block into Ok(empty).
        //
        // Second entry (`cloak_after_egress`, default off): the auto-egress arm
        // uncovered a challenge the ladder could not see, because the origin
        // WAF-blocks datacenter IPs and only serves the challenge to the
        // residential exit. `!cloak_attempted` is repeated here deliberately: it
        // is a conjunct INSIDE `route_to_cloak`, so an `||` that omitted it would
        // let a cloak-first request fire a second permit and a second cold solve
        // on the same page.
        #[cfg(feature = "cloak")]
        let cloak_after_egress_route = arm_uncovered_cf
            && self.cloak_after_egress
            && self.cloak_arm.is_some()
            && !cloak_attempted;
        #[cfg(feature = "cloak")]
        if (route_to_cloak || cloak_after_egress_route)
            && let Some(arm) = &self.cloak_arm
        {
            let kind = RendererKind::Cloak;
            let floor = std::time::Duration::from_millis(crw_core::config::CLOAK_ARM_FLOOR_MS);
            let deadline_ok = deadline.remaining() >= floor;
            // `cloak_recover_on_cf` relaxes the entry gate itself (not just the
            // `Deadline` passed to `.fetch()` below) — reproducing the original
            // starvation bug is exactly "permit gated on `deadline_ok`, fetch
            // deadline relaxed": the permit would still never be acquired under
            // a small SaaS deadline. `cloak_sem` load-shed and the per-host
            // breaker check are unchanged either way (mirrors `arm_wanted` in
            // the chrome_proxy arm above, breaker-open excluded from "wanted"
            // so it isn't double-counted against `armShed`).
            let wanted = (self.cloak_recover_on_cf || deadline_ok)
                && !self.breakers.host_for(&host, kind).await.is_open();
            let permit = if wanted {
                self.cloak_sem.clone().try_acquire_owned().ok()
            } else {
                None
            };
            if wanted && permit.is_none() {
                // Wanted to recover but the cloak pool was saturated — shed
                // rather than queue. Mirrors the chrome_proxy arm's `armShed`
                // so the A/B is measurable.
                metrics()
                    .render_route_decision_total
                    .with_label_values(&[kind.as_str(), "armShed"])
                    .inc();
            }
            if let Some(_permit) = permit {
                chain.push(kind);
                // Fresh, decoupled budget when the shared deadline can't clear
                // the floor and the caller opted in: a cold Turnstile solve
                // needs ~21-40s regardless of how little of the shared deadline
                // is left. Reuses the shared deadline unchanged otherwise (byte
                // identical to today when it still clears the floor).
                // The fresh budget is the larger of the constant and the
                // configured cloak tier timeout, so `CLOAK_TIMEOUT_MS` is a
                // real knob here. And it is taken whenever the SHARED
                // remainder cannot fit a whole mint, not only under the 24s
                // floor: the sidecar refuses to start a mint it cannot finish
                // (its guard is sized to echo + launch + solve + close), so a
                // shared remainder in the band between the floor and a full
                // mint would hand it a budget it declines, where a remainder
                // under the floor already gets a full fresh one. That band
                // would be strictly worse than being under the floor.
                let fresh_budget =
                    std::time::Duration::from_millis(crw_core::config::CLOAK_ARM_RECOVER_BUDGET_MS)
                        .max(
                            self.tier_timeouts
                                .get(&RendererKind::Cloak)
                                .copied()
                                .unwrap_or_default(),
                        );
                let arm_deadline = if deadline.remaining() >= fresh_budget {
                    deadline
                } else {
                    crw_core::Deadline::now_plus(fresh_budget)
                };
                metrics()
                    .render_route_decision_total
                    .with_label_values(&[
                        kind.as_str(),
                        // Split so the new entry is measurable on its own: the
                        // whole reason this change exists is that a "cloak
                        // fired" counter could not tell anyone WHICH route fired,
                        // and production had been serving zero cloak requests
                        // without that being visible.
                        if cloak_after_egress_route {
                            "firedAfterEgress"
                        } else {
                            "fired"
                        },
                    ])
                    .inc();
                let entry = self.pick_proxy_for_url(url);
                let attempt = REQUEST_PROXY
                    .scope(entry, arm.fetch(url, headers, wait_for_ms, arm_deadline))
                    .await;
                match attempt {
                    Ok(r) => {
                        let r_ok = html_body_text_len(&r.html) >= Self::MIN_RENDERED_TEXT_LEN
                            && detector::looks_like_failed_render(&r.html).is_none()
                            && !detector::looks_like_loading_placeholder(&r.html)
                            && !detector::looks_like_cloudflare_challenge(&r.html)
                            // Same gap as the chrome_proxy arm: the stealth tier
                            // clears CF, but a DataDome / generic ban shell it
                            // cannot clear would otherwise ship as content.
                            && !detector::looks_like_generic_bot_wall(&r.html, r.truncated)
                            && detector::looks_like_vendor_block(&r.html).is_none();
                        if !host.is_empty() {
                            // Same split as the other content verdicts: success
                            // globally, the page judgement host-only. This arm had
                            // no SiteBlocked path at all, so a wall it could not
                            // clear was booked as a renderer fault unconditionally.
                            if r_ok {
                                self.breakers
                                    .record_outcome(&host, kind, BreakerOutcome::Success)
                                    .await;
                            } else {
                                self.breakers
                                    .record_scoped_outcome(
                                        &host,
                                        kind,
                                        None,
                                        Some(BreakerOutcome::RenderError),
                                    )
                                    .await;
                            }
                        }
                        if r_ok {
                            metrics()
                                .render_route_decision_total
                                .with_label_values(&[kind.as_str(), "success"])
                                .inc();
                        }
                        let better = r_ok
                            && match &thin_result {
                                Some(prev) => r.html.len() > prev.html.len(),
                                None => true,
                            };
                        // Ungated, host-only (these paths can be person names
                        // and stdout leaves the box): the one line `docker logs`
                        // needs to tell WHICH route fired and whether it won.
                        // The metric above cannot say that per request.
                        tracing::info!(
                            host = %host,
                            route = if cloak_after_egress_route { "afterEgress" } else { "ladder" },
                            ok = r_ok,
                            consumed = better,
                            "cloak arm result"
                        );
                        if self.latency_breakdown {
                            tracing::info!(
                                target: "latency_breakdown",
                                url, tier = "cloak",
                                ok = r_ok, consumed = better,
                                "cloak recovery fired"
                            );
                        }
                        if better {
                            thin_result = Some(r);
                        }
                    }
                    Err(e) => {
                        if !host.is_empty() {
                            // Same split as the cloak-first arm: a body the tier
                            // returned and we rejected is the page's verdict, an
                            // unreachable sidecar or proxy is ours. The predicate
                            // matches only cloak's own content messages, so this
                            // arm keeps recording a real transport failure
                            // globally.
                            let global = (!is_cloak_content_verdict(&e))
                                .then_some(BreakerOutcome::ConnectionError);
                            self.breakers
                                .record_scoped_outcome(
                                    &host,
                                    kind,
                                    global,
                                    Some(BreakerOutcome::ConnectionError),
                                )
                                .await;
                        }
                        tracing::info!(
                            host = %host,
                            route = if cloak_after_egress_route { "afterEgress" } else { "ladder" },
                            error = %e,
                            "cloak arm result (error)"
                        );
                        if self.latency_breakdown {
                            tracing::info!(
                                target: "latency_breakdown",
                                url, tier = "cloak", error = %e,
                                "cloak recovery fired (error)"
                            );
                        }
                    }
                }
            }
        }

        // Return the best thin result if we have one, otherwise the last error.
        if let Some(mut result) = thin_result {
            // Stamp routing metadata on the soft-failure result too — callers
            // need to know which chain was attempted for debugging.
            if let Some(last) = chain.last().copied() {
                result.credit_cost = credit_for(last);
                result.render_decision = Some(RenderDecision::Failover {
                    chain: chain.clone(),
                    reason: last_failover_reason
                        .clone()
                        .unwrap_or(FailoverErrorKind::Other),
                });
            }
            // When the user hard-pinned a single renderer and it failed thin,
            // failover never ran — surface an actionable hint so callers (SaaS
            // playground, CLI, MCP) can show a banner instead of silently
            // returning broken markdown with `success: true`.
            if is_user_pinned
                && chain.len() == 1
                && let Some(pinned) = chain.first().copied()
            {
                let reason = last_failover_reason
                    .as_ref()
                    .map(|r| r.as_str())
                    .unwrap_or("unknown");
                let hint = format!(
                    "Pinned renderer '{}' returned a failed render ({}). Content may be unreliable. Retry with renderer=\"chrome\" or omit the renderer field for auto-failover.",
                    pinned.as_str(),
                    reason,
                );
                result.warnings.push(hint);
            }
            // A wall no tier could clear is not content. Every tier already
            // classified it — that verdict is exactly why the result landed in
            // `thin_result` instead of being accepted — and this was the one
            // place that discarded it: the interstitial went back stamped with a
            // page credit under `success: true`, so the caller paid for
            // "Checking your browser" and an agent read it as the page.
            //
            // Scoped deliberately to the generic phrase-list wall. A CF
            // challenge or a named vendor shell keeps shipping as a thin result:
            // those have their own recovery arms (cloak, chrome_proxy) that read
            // the body back, and `cloak_recover_on_cf_false_skips_arm_on_exhausted_deadline`
            // pins that shape. A genuinely thin but real page still ships too,
            // because returning the best available body is the point of this
            // tail; the recall invariant depends on that staying true.
            //
            // The verdict travels as DATA now, not as an error. It used to be
            // `CrwError::HttpError`, which `crw-server` maps to 502, so a target
            // refusing us was reported as our own gateway failing: the caller saw
            // "Bad Gateway", the SaaS booked `upstream_5xx`, and the watchdog
            // paged for a platform incident that was not happening.
            //
            // Returning `Ok` here does NOT mean the wall wins over a better body.
            // Every caller that has a fallback body demotes a walled ladder
            // result back to a ladder failure first (see
            // `demote_wall_to_ladder_failure`), which is the same preference the
            // error carried implicitly. Callers with no fallback, where the HTTP
            // tier failed outright, now get the verdict instead of a 502.
            if detector::looks_like_generic_bot_wall(&result.html, result.truncated) {
                result.block = Some(unclearable_wall_verdict());
            }
            Ok(result)
        } else {
            Err(last_error
                .unwrap_or_else(|| CrwError::RendererError("No JS renderer available".to_string())))
        }
    }

    /// Check availability of all renderers.
    pub async fn check_health(&self) -> HashMap<String, bool> {
        let mut health = HashMap::new();
        health.insert("http".to_string(), self.http.is_available().await);
        for r in &self.js_renderers {
            health.insert(r.name().to_string(), r.is_available().await);
        }
        health
    }
}

/// Rough estimate of visible text length in an HTML document.
/// Strips tags and collapses whitespace. Used to detect "thin" renders
/// where a renderer returned HTML but failed to execute JavaScript.
fn html_body_text_len(html: &str) -> usize {
    // Extract body content if present, otherwise use entire HTML.
    //
    // The closing tag is searched from `start`, not from 0. Searching the whole
    // document finds the FIRST `</body>` anywhere, which on a page that mentions
    // the literal string before its real body — a script writing markup, an
    // escaped snippet in a docs page, plain malformed HTML — lands BEFORE the
    // opening tag. `&html[start..end]` then panics with
    // "byte range starts at 198294 but ends at 197897" and kills the request.
    // Seen in production 2026-08-11, 9 times in 30 minutes.
    let body = if let Some(start) = html.find("<body") {
        let start = html[start..].find('>').map(|i| start + i + 1).unwrap_or(0);
        let end = html[start..]
            .find("</body>")
            .map(|i| start + i)
            .unwrap_or(html.len());
        &html[start..end]
    } else {
        html
    };
    // Strip tags crudely.
    let mut in_tag = false;
    let mut text_len = 0;
    let mut prev_ws = true;
    for ch in body.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            if ch.is_whitespace() {
                if !prev_ws {
                    text_len += 1;
                    prev_ws = true;
                }
            } else {
                text_len += 1;
                prev_ws = false;
            }
        }
    }
    text_len
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::breaker::BreakerConfig;
    #[cfg(feature = "camoufox")]
    use crw_core::config::CamoufoxEndpoint;
    #[cfg(feature = "cdp")]
    use crw_core::config::CdpEndpoint;
    use std::time::Duration;

    /// Generous deadline used by tests that don't care about budget enforcement.
    fn tdl() -> crw_core::Deadline {
        crw_core::Deadline::now_plus(Duration::from_secs(60))
    }

    #[test]
    fn body_text_len_survives_a_closing_tag_before_the_opening_one() {
        // A page that prints the literal "</body>" before its real body — a script
        // writing markup, an escaped snippet in documentation, or plain malformed
        // HTML. Searching the whole document for the closing tag found this one,
        // producing end < start and panicking on the slice. Production hit it 9
        // times in 30 minutes on 2026-08-11.
        let html = concat!(
            "<html><head><script>var tpl = \"</body>\";</script></head>",
            "<body><p>real content here</p></body></html>"
        );
        assert!(html.find("</body>").unwrap() < html.find("<body").unwrap());
        assert!(html_body_text_len(html) > 0);
    }

    #[test]
    fn body_text_len_measures_only_the_body() {
        let html = "<html><head><title>ignored</title></head><body>hello there</body></html>";
        // "hello there" collapses to 11 visible characters; the head must not count.
        assert_eq!(html_body_text_len(html), 11);
    }

    #[test]
    fn body_text_len_handles_a_missing_closing_tag() {
        let html = "<html><body><p>unclosed document";
        assert!(html_body_text_len(html) > 0);
    }

    #[test]
    fn body_text_len_handles_no_body_at_all() {
        assert!(html_body_text_len("<html><p>fragment</p></html>") > 0);
    }

    fn base_cfg(mode: RendererMode) -> RendererConfig {
        let mut cfg = RendererConfig {
            mode,
            ..Default::default()
        };
        // Unit tests override `r.impersonated` with mocks anyway; building a
        // REAL wreq client per make_renderer_with_mocks call (the default is
        // enabled=true) multiplied btls clients across the parallel suite and
        // made the plain-tier loopback fetches flake with transient connect
        // errors under load. The tier-present default is asserted separately
        // in `new_default_config_constructs_impersonated_tier`.
        cfg.impersonated.enabled = false;
        cfg
    }

    #[cfg(feature = "impersonated")]
    #[test]
    fn new_default_config_constructs_impersonated_tier() {
        // Production default: the tier is ON in any feature-on build, no
        // config section needed.
        let r = FallbackRenderer::new(
            &RendererConfig::default(),
            "crw-test",
            None,
            &StealthConfig::default(),
        )
        .unwrap();
        assert!(r.has_impersonated_tier());
        assert_eq!(
            r.available_renderer_names().first(),
            Some(&"impersonated-http")
        );
    }

    #[cfg(feature = "impersonated")]
    #[test]
    fn impersonated_disabled_by_config_leaves_no_tier() {
        let mut cfg = RendererConfig::default();
        cfg.impersonated.enabled = false;
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        assert!(!r.has_impersonated_tier());
        assert!(!r.available_renderer_names().contains(&"impersonated-http"));
    }

    #[test]
    fn new_mode_none_ok_no_js_renderers() {
        let cfg = base_cfg(RendererMode::None);
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        assert!(r.js_renderer_names().is_empty());
        assert_eq!(r.render_js_default, None);
    }

    #[test]
    fn new_mode_auto_no_endpoints_ok_http_only() {
        let cfg = base_cfg(RendererMode::Auto);
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        assert!(r.js_renderer_names().is_empty());
    }

    #[cfg(feature = "cdp")]
    #[test]
    fn new_mode_chrome_without_endpoint_errors() {
        let cfg = base_cfg(RendererMode::Chrome);
        let err =
            FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("chrome"), "expected chrome in error: {msg}");
        assert!(
            msg.contains("ws_url") || msg.contains("not configured"),
            "expected ws_url hint in error: {msg}"
        );
    }

    #[cfg(feature = "cdp")]
    #[test]
    fn new_mode_chrome_with_endpoint_ok_only_chrome() {
        let cfg = RendererConfig {
            mode: RendererMode::Chrome,
            chrome: Some(CdpEndpoint {
                ws_url: "ws://127.0.0.1:9222/".into(),
            }),
            lightpanda: Some(CdpEndpoint {
                ws_url: "ws://127.0.0.1:9223/".into(),
            }),
            ..Default::default()
        };
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        assert_eq!(r.js_renderer_names(), vec!["chrome"]);
    }

    #[cfg(feature = "cdp")]
    #[test]
    fn new_mode_lightpanda_without_endpoint_errors() {
        let cfg = base_cfg(RendererMode::Lightpanda);
        let err =
            FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("lightpanda"));
    }

    #[cfg(feature = "cdp")]
    #[test]
    fn new_mode_auto_with_both_endpoints_preserves_order() {
        let cfg = RendererConfig {
            mode: RendererMode::Auto,
            lightpanda: Some(CdpEndpoint {
                ws_url: "ws://127.0.0.1:9222/".into(),
            }),
            chrome: Some(CdpEndpoint {
                ws_url: "ws://127.0.0.1:9223/".into(),
            }),
            ..Default::default()
        };
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        assert_eq!(r.js_renderer_names(), vec!["lightpanda", "chrome"]);
    }

    #[cfg(feature = "cdp")]
    #[test]
    fn ladder_includes_chrome_proxy_when_configured() {
        let cfg = RendererConfig {
            mode: RendererMode::Auto,
            lightpanda: Some(CdpEndpoint {
                ws_url: "ws://127.0.0.1:9222/".into(),
            }),
            chrome: Some(CdpEndpoint {
                ws_url: "ws://127.0.0.1:9223/".into(),
            }),
            chrome_proxy: Some(CdpEndpoint {
                ws_url: "ws://127.0.0.1:9224/".into(),
            }),
            ..Default::default()
        };
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        // chrome_proxy must be the LAST tier — fallback chain tries Chrome
        // direct first and only falls through to the proxy on Chrome failure.
        assert_eq!(
            r.js_renderer_names(),
            vec!["lightpanda", "chrome", "chrome_proxy"]
        );
    }

    /// The escalation after a thin LightPanda body picks the first non-lightpanda
    /// entry of `auto_ladder_names`. Pinning a fixed name instead dead-ends on any
    /// pool that does not hold it, so these four shapes are what that choice has
    /// to get right.
    #[cfg(feature = "cdp")]
    #[test]
    fn lightpanda_escalation_target_picks_a_tier_the_pool_actually_holds() {
        let ladder = |cfg: &RendererConfig| {
            let r =
                FallbackRenderer::new(cfg, "crw-test", None, &StealthConfig::default()).unwrap();
            r.lightpanda_escalation_target().map(str::to_string)
        };
        let lp = || {
            Some(CdpEndpoint {
                ws_url: "ws://127.0.0.1:9222/".into(),
            })
        };
        let ep = |p: &str| {
            Some(CdpEndpoint {
                ws_url: format!("ws://127.0.0.1:{p}/"),
            })
        };

        // Production shape: chrome present, so the target is unchanged.
        assert_eq!(
            ladder(&RendererConfig {
                mode: RendererMode::Auto,
                lightpanda: lp(),
                chrome: ep("9223"),
                chrome_proxy: ep("9224"),
                ..Default::default()
            }),
            Some("chrome".to_string())
        );

        // No chrome sidecar: the escalation must reach the tier that IS configured
        // instead of failing on a pin the pool cannot satisfy.
        assert_eq!(
            ladder(&RendererConfig {
                mode: RendererMode::Auto,
                lightpanda: lp(),
                chrome_proxy: ep("9224"),
                ..Default::default()
            }),
            Some("chrome_proxy".to_string())
        );
        // playwright is untracked (no `RendererKind`), so it is not a target.
        assert_eq!(
            ladder(&RendererConfig {
                mode: RendererMode::Auto,
                lightpanda: lp(),
                playwright: ep("9225"),
                ..Default::default()
            }),
            None
        );

        // Nothing stronger than lightpanda: no target, so no unsatisfiable pin.
        assert_eq!(
            ladder(&RendererConfig {
                mode: RendererMode::Auto,
                lightpanda: lp(),
                ..Default::default()
            }),
            None
        );

        // chrome stays the first choice even when it is not first in construction
        // order, so a pool that already reached chrome keeps reaching chrome.
        assert_eq!(
            ladder(&RendererConfig {
                mode: RendererMode::Auto,
                lightpanda: lp(),
                playwright: ep("9225"),
                chrome: ep("9223"),
                ..Default::default()
            }),
            Some("chrome".to_string())
        );

        // Under auto_egress_escalation the chain lifts chrome_proxy out of the
        // ladder and gates it behind a hard block, so it must not become the
        // escalation target: pinning it by name would skip that gate and put a
        // paid residential render on every thin page.
        assert_eq!(
            ladder(&RendererConfig {
                mode: RendererMode::Auto,
                lightpanda: lp(),
                chrome_proxy: ep("9224"),
                auto_egress_escalation: true,
                ..Default::default()
            }),
            None
        );
    }

    /// A camoufox held out of the auto chain is constructed and pinnable, but an
    /// internally chosen escalation must not reach it: naming it bypasses the
    /// exclusion, which would override a deliberate `include_in_auto = false`.
    #[cfg(all(feature = "cdp", feature = "camoufox"))]
    #[test]
    fn auto_ladder_names_respects_the_camoufox_opt_out() {
        let with_lp = |include_in_auto: bool| {
            let mut cfg = camoufox_cfg(RendererMode::Auto, include_in_auto);
            cfg.lightpanda = Some(CdpEndpoint {
                ws_url: "ws://127.0.0.1:9222/".into(),
            });
            FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap()
        };

        let held_out = with_lp(false);
        assert!(
            held_out.js_renderer_names().contains(&"camoufox"),
            "the tier is still constructed so an explicit pin can reach it"
        );
        assert_eq!(held_out.auto_ladder_names(), vec!["lightpanda"]);

        let in_chain = with_lp(true);
        assert_eq!(in_chain.auto_ladder_names(), vec!["lightpanda", "camoufox"]);
    }

    /// The HTTP tier decodes every non-PDF body as HTML regardless of its
    /// declared type, so without this gate an empty `application/json` or
    /// `image/*` 2xx would buy a full browser render for nothing.
    #[test]
    fn empty_2xx_escalation_is_limited_to_htmlish_bodies() {
        for ct in [
            None,
            Some(""),
            Some("text/html"),
            Some("application/xhtml+xml"),
        ] {
            assert!(is_html_like_content_type(ct), "{ct:?} should stay eligible");
        }
        for ct in [
            "application/json",
            "text/plain",
            "image/png",
            "application/zip",
            "text/csv",
        ] {
            assert!(
                !is_html_like_content_type(Some(ct)),
                "{ct} cannot be improved by a browser"
            );
        }
    }

    #[cfg(feature = "cdp")]
    #[test]
    fn ladder_omits_chrome_proxy_when_not_configured() {
        let cfg = RendererConfig {
            mode: RendererMode::Auto,
            chrome: Some(CdpEndpoint {
                ws_url: "ws://127.0.0.1:9223/".into(),
            }),
            chrome_proxy: None,
            ..Default::default()
        };
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        assert!(!r.js_renderer_names().contains(&"chrome_proxy"));
    }

    #[cfg(not(feature = "cdp"))]
    #[test]
    fn new_mode_chrome_errors_without_cdp_feature() {
        let cfg = base_cfg(RendererMode::Chrome);
        let err =
            FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("cdp"), "expected cdp in error: {msg}");
    }

    #[cfg(feature = "camoufox")]
    fn camoufox_cfg(mode: RendererMode, include_in_auto: bool) -> RendererConfig {
        RendererConfig {
            mode,
            camoufox: Some(CamoufoxEndpoint {
                base_url: "http://127.0.0.1:9377".into(),
                api_key: String::new(),
                include_in_auto,
            }),
            ..Default::default()
        }
    }

    /// Opt-in default: a configured endpoint is CONSTRUCTED (so an explicit
    /// `renderer = "camoufox"` pin can reach it) but does NOT join the auto
    /// ladder when `include_in_auto = false`.
    #[cfg(feature = "camoufox")]
    #[test]
    fn camoufox_constructed_for_pin_but_excluded_from_auto() {
        let cfg = camoufox_cfg(RendererMode::Auto, false);
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        assert!(
            r.js_renderer_names().contains(&"camoufox"),
            "configured camoufox must be constructed for pin-reachability"
        );
        assert!(
            !r.camoufox_in_auto,
            "include_in_auto=false must keep camoufox out of the auto ladder"
        );
    }

    #[cfg(feature = "camoufox")]
    #[test]
    fn camoufox_joins_auto_when_include_in_auto_true() {
        let cfg = camoufox_cfg(RendererMode::Auto, true);
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        assert!(r.js_renderer_names().contains(&"camoufox"));
        assert!(r.camoufox_in_auto);
    }

    /// `mode = "camoufox"` pins to ONLY camoufox, and must mark it in-auto so a
    /// non-pinned request is not left with zero renderers.
    #[cfg(feature = "camoufox")]
    #[test]
    fn camoufox_pinned_mode_uses_only_camoufox() {
        let cfg = camoufox_cfg(RendererMode::Camoufox, false);
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        assert_eq!(r.js_renderer_names(), vec!["camoufox"]);
        assert!(r.camoufox_in_auto);
    }

    #[cfg(feature = "camoufox")]
    #[test]
    fn camoufox_pinned_mode_without_base_url_errors() {
        let cfg = RendererConfig {
            mode: RendererMode::Camoufox,
            camoufox: Some(CamoufoxEndpoint::default()), // empty base_url
            ..Default::default()
        };
        let err =
            FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("camoufox"));
    }

    #[cfg(feature = "camoufox")]
    #[test]
    fn camoufox_absent_when_not_configured() {
        let cfg = base_cfg(RendererMode::Auto);
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        assert!(!r.js_renderer_names().contains(&"camoufox"));
        assert!(!r.camoufox_in_auto);
    }

    #[test]
    fn new_render_js_default_stored() {
        let cfg = RendererConfig {
            mode: RendererMode::None,
            render_js_default: Some(true),
            ..Default::default()
        };
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        assert_eq!(r.render_js_default, Some(true));
    }

    /// Mock fetcher for unit-testing dispatch logic without real CDP/HTTP.
    struct MockFetcher {
        name: &'static str,
        behavior: MockBehavior,
    }

    #[derive(Clone)]
    enum MockBehavior {
        Ok(String),
        OkStatus(u16, String),
        Err(String),
        Timeout,
        /// Panics if fetched: proves a tier was NEVER consulted. Only the
        /// `impersonated` routing tests use it, so allow dead code in the
        /// lean build where those tests are cfg'd out.
        #[cfg_attr(not(feature = "impersonated"), allow(dead_code))]
        Panic(&'static str),
    }

    #[async_trait::async_trait]
    impl PageFetcher for MockFetcher {
        async fn fetch(
            &self,
            url: &str,
            _headers: &HashMap<String, String>,
            _wait_for_ms: Option<u64>,
            _deadline: crw_core::Deadline,
        ) -> CrwResult<FetchResult> {
            let (status, html) = match &self.behavior {
                MockBehavior::Ok(html) => (200u16, html.clone()),
                MockBehavior::OkStatus(s, html) => (*s, html.clone()),
                MockBehavior::Err(msg) => return Err(CrwError::RendererError(msg.clone())),
                MockBehavior::Timeout => return Err(CrwError::Timeout(1)),
                MockBehavior::Panic(msg) => panic!("{msg}"),
            };
            Ok(FetchResult {
                url: url.to_string(),
                final_url: None,
                status_code: status,
                html,
                content_type: Some("text/html".to_string()),
                raw_bytes: None,
                rendered_with: Some(self.name.to_string()),
                elapsed_ms: 0,
                warning: None,
                render_decision: None,
                credit_cost: 0,
                warnings: Vec::new(),
                truncated: false,
                deadline_exceeded: false,
                captured_responses: Vec::new(),
                screenshot: None,
                block: None,
            })
        }

        fn name(&self) -> &str {
            self.name
        }
        fn supports_js(&self) -> bool {
            true
        }
        async fn is_available(&self) -> bool {
            true
        }
    }

    fn rich_html(marker: &str) -> String {
        format!(
            "<html><body><article>{}{}</article></body></html>",
            marker,
            "x".repeat(200)
        )
    }

    /// Wikimedia's HTTP-200 datacenter-IP block shell (no <body>, scriptless).
    fn wikimedia_block_html() -> String {
        String::from(
            r#"<!DOCTYPE html>
<html lang="en">
<title>Wikimedia Error</title>
<div class="content" role="main">
<h1>Error</h1>
<p>Contabo networks are forbidden due to abuse. Contact noc@wikimedia.org for assistance.</p>
</div>
<div class="footer">
<p>If you report this error to the Wikimedia System Administrators, please include the details below.</p>
</div>
</html>"#,
        )
    }

    /// Mock that records whether it was invoked. Separate from `MockFetcher` so the
    /// ~12 existing constructor sites stay untouched.
    struct CountingFetcher {
        name: &'static str,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl PageFetcher for CountingFetcher {
        async fn fetch(
            &self,
            url: &str,
            _headers: &HashMap<String, String>,
            _wait_for_ms: Option<u64>,
            _deadline: crw_core::Deadline,
        ) -> CrwResult<FetchResult> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(FetchResult {
                url: url.to_string(),
                final_url: None,
                status_code: 200,
                html: rich_html("rendered"),
                content_type: Some("text/html".to_string()),
                raw_bytes: None,
                rendered_with: Some(self.name.to_string()),
                elapsed_ms: 0,
                warning: None,
                render_decision: None,
                credit_cost: 0,
                warnings: Vec::new(),
                truncated: false,
                deadline_exceeded: false,
                captured_responses: Vec::new(),
                screenshot: None,
                block: None,
            })
        }
        fn name(&self) -> &str {
            self.name
        }
        fn supports_js(&self) -> bool {
            true
        }
        async fn is_available(&self) -> bool {
            true
        }
    }

    /// A degenerate budget must not invoke a JS tier at all (prod: 432 of 536
    /// escalations ran with <50ms, returning a fabricated `Timeout after 5ms`),
    /// and the request must still surface `CrwError::Timeout` so the server keeps
    /// mapping it to 504 rather than a 500 from the `RendererError` tail.
    #[tokio::test]
    async fn degenerate_budget_skips_js_tier_and_preserves_timeout() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mock = Arc::new(CountingFetcher {
            name: "chrome",
            calls: calls.clone(),
        });
        let r = make_renderer_with_mocks(vec![mock]);

        let err = r
            .fetch_with_js(
                "https://example.com",
                &HashMap::new(),
                None,
                None,
                false,
                crw_core::Deadline::from_request_ms(0),
            )
            .await
            .expect_err("an exhausted budget must not produce a rendered page");

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "renderer must be skipped, not invoked with a few milliseconds"
        );
        assert!(
            matches!(err, CrwError::Timeout(_)),
            "must stay a Timeout (504), not RendererError (500); got {err:?}"
        );
    }

    /// Burns most of the budget, then fails — so the NEXT tier lands below the floor.
    struct SlowFailingFetcher {
        name: &'static str,
        burn: Duration,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl PageFetcher for SlowFailingFetcher {
        async fn fetch(
            &self,
            _url: &str,
            _headers: &HashMap<String, String>,
            _wait_for_ms: Option<u64>,
            _deadline: crw_core::Deadline,
        ) -> CrwResult<FetchResult> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(self.burn).await;
            Err(CrwError::RendererError("anti-bot wall".to_string()))
        }
        fn name(&self) -> &str {
            self.name
        }
        fn supports_js(&self) -> bool {
            true
        }
        async fn is_available(&self) -> bool {
            true
        }
    }

    /// A tier that fails for a real reason, followed by a tier skipped for lack of
    /// budget, must still report Timeout (504) — not the earlier RendererError (500).
    /// The skipped tier would have been invoked, timed out, and overwritten
    /// `last_error`; skipping must not change the status the caller sees. Guards
    /// against reintroducing `get_or_insert_with` here.
    #[tokio::test]
    async fn budget_skip_overrides_an_earlier_renderer_error() {
        let slow_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let slow = Arc::new(SlowFailingFetcher {
            name: "lightpanda",
            burn: Duration::from_millis(1_200),
            calls: slow_calls.clone(),
        });
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let chrome = Arc::new(CountingFetcher {
            name: "chrome",
            calls: calls.clone(),
        });
        let r = make_renderer_with_mocks(vec![slow, chrome]);

        // 1500ms budget: lightpanda is comfortably above the 500ms floor even under
        // CI scheduling jitter, burns 1200ms and errors, leaving ~300ms — chrome is
        // then below the floor and is skipped.
        let err = r
            .fetch_with_js(
                "https://example.com",
                &HashMap::new(),
                None,
                None,
                false,
                crw_core::Deadline::from_request_ms(1_500),
            )
            .await
            .expect_err("both tiers must fail");

        assert_eq!(
            slow_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the first tier must actually run, or this test proves nothing"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "chrome must be skipped for lack of budget"
        );
        assert!(
            matches!(err, CrwError::Timeout(_)),
            "a budget skip must overwrite the earlier RendererError so the server \
             still maps this to 504; got {err:?}"
        );
    }

    /// The cloak tier's own `Err` messages must split into page verdicts and tier
    /// faults, or the length floor on its accept arm quietly relocates the
    /// dead-origin case from a host-scoped path onto a global one.
    ///
    /// Chained with `empty_fail_open_body_is_an_error_not_a_thin_ok` in
    /// `cloak.rs`, which pins that the fail-open error actually carries this
    /// wording: together they cover the whole path from sidecar to breaker.
    #[test]
    fn cloak_content_verdicts_are_not_tier_faults() {
        assert!(is_cloak_content_verdict(&CrwError::RendererError(
            "cloak: empty or unusably thin body (sidecar fail-open) (HTTP 200) for a.example"
                .into()
        )));
        assert!(is_cloak_content_verdict(&CrwError::RendererError(
            "cloak: still challenged (HTTP 403) for a.example".into()
        )));
        // A sidecar we could not reach at all IS a tier fault and must keep
        // reaching the global window, or a dead sidecar is never concluded dead.
        assert!(!is_cloak_content_verdict(&CrwError::RendererError(
            "error sending request for url (http://cloak-sidecar:8000/)".into()
        )));
        assert!(!is_cloak_content_verdict(&CrwError::Timeout(40_000)));
        // Not anchored loosely: a message that merely mentions cloak is not a
        // verdict.
        assert!(!is_cloak_content_verdict(&CrwError::RendererError(
            "chrome failed while cloak: still challenged was logged".into()
        )));
    }

    /// A tier that keeps returning THIN content must not be disabled for every
    /// host, and this pins it at the call site rather than at the registry.
    ///
    /// The registry-level tests only prove `record_scoped_outcome` behaves; they
    /// pass whether or not the ladder actually calls it. A mutation reverting the
    /// serial thin arm to `record_outcome` left all 995 other tests green, which
    /// is the gap this closes. A thin body is a verdict about the PAGE, so it
    /// belongs to the host tier: the 1000-URL bench traced roughly 12% of
    /// failures to false global trips from exactly this.
    #[tokio::test]
    async fn thin_content_does_not_open_the_global_tier() {
        // Under MIN_RENDERED_TEXT_LEN (50), so every attempt classifies as thin
        // rather than acceptable, and nothing here is a wall or a hard block.
        let thin = "<html><body>tiny</body></html>";
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(thin.to_string()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(thin.to_string()),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp, chrome]);
        r.breakers = Arc::new(BreakerRegistry::new(BreakerConfig {
            base_cooldown: Duration::from_secs(300),
            max_cooldown: Duration::from_secs(300),
            ..BreakerConfig::default()
        }));

        for _ in 0..80 {
            let _ = r
                .fetch(
                    "https://thin-host.example/page",
                    &HashMap::new(),
                    Some(true),
                    None,
                    None,
                    tdl(),
                )
                .await;
        }

        assert_eq!(
            r.breakers
                .global_for(RendererKind::Lightpanda)
                .snapshot()
                .state,
            "closed",
            "a thin body is a judgement about the page, so it must not disable \
             the tier for every other host"
        );
        assert_eq!(
            r.breakers
                .host_for("thin-host.example", RendererKind::Lightpanda)
                .await
                .snapshot()
                .state,
            "open",
            "the host tier must still learn this host renders thin"
        );
    }

    /// The other half of the origin carve-out, and the one a mutation test showed
    /// nothing was guarding.
    ///
    /// `is_origin_fault_for_breaker` returns true for `CrwError::Timeout(_)`,
    /// because it is shared with the error-attribution path where that arm is
    /// correct. The breaker path must undo it with `&& !was_timeout`, or a hung
    /// CDP pool, which times out on every host, would stop reaching the global
    /// window and nothing would ever conclude the tier is sick. Dropping that
    /// clause is a one-token mutation that left all 995 other tests green, which
    /// is exactly why this test exists.
    #[tokio::test]
    async fn tier_timeouts_still_open_the_global_tier() {
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Timeout,
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Timeout,
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp, chrome]);
        r.breakers = Arc::new(BreakerRegistry::new(BreakerConfig {
            base_cooldown: Duration::from_secs(300),
            max_cooldown: Duration::from_secs(300),
            ..BreakerConfig::default()
        }));

        // Spread across distinct hosts, which is what a genuinely sick tier looks
        // like and what a single dead origin does not.
        for i in 0..80 {
            let _ = r
                .fetch(
                    &format!("https://host{i}.example/page"),
                    &HashMap::new(),
                    Some(true),
                    None,
                    None,
                    tdl(),
                )
                .await;
        }

        assert_eq!(
            r.breakers
                .global_for(RendererKind::Lightpanda)
                .snapshot()
                .state,
            "open",
            "a tier timing out on every host must still trip globally, or a hung \
             pool is never caught"
        );
    }

    /// A dead ORIGIN must not disable a renderer tier for every other host.
    ///
    /// The serial error arm used to write its outcome to the global window as
    /// well as the host one, on the stated premise that "the renderer itself
    /// errored (no response to inspect), which is a genuine tier signal". A dead
    /// origin produces exactly that shape: six hours of production held 24
    /// net::ERR_ABORTED, 10 PeerFailedVerification, 5 ERR_CERT_DATE_INVALID and
    /// more, every one the target's fault and every one advancing the global
    /// window. Two of the four observed global trips came from here.
    ///
    /// Eighty such failures across eighty DISTINCT hosts is far past
    /// `min_calls: 50` at `failure_rate_threshold: 0.80`, so this test fails
    /// against the old code and passes only once the outcome is host-scoped.
    #[tokio::test]
    async fn origin_navigation_failures_do_not_open_the_global_tier() {
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Err(
                "Renderer error: Navigation failed: net::ERR_CONNECTION_REFUSED".into(),
            ),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-OK")),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp, chrome]);
        // Long cooldown so a half-open transition cannot mask the verdict under
        // parallel test load, matching the sibling breaker test above.
        r.breakers = Arc::new(BreakerRegistry::new(BreakerConfig {
            base_cooldown: Duration::from_secs(300),
            max_cooldown: Duration::from_secs(300),
            ..BreakerConfig::default()
        }));

        // One dead origin, hammered. 80 failures is past `min_calls: 50` at
        // `failure_rate_threshold: 0.80`, so this is exactly the shape that used
        // to take the tier out for everyone: in production one domain
        // contributed 38 of the ~214 lightpanda outcomes that reached any window
        // in six hours.
        for _ in 0..80 {
            let _ = r
                .fetch(
                    "https://dead-origin.example/page",
                    &HashMap::new(),
                    Some(true),
                    None,
                    None,
                    tdl(),
                )
                .await;
        }

        assert_eq!(
            r.breakers
                .global_for(RendererKind::Lightpanda)
                .snapshot()
                .state,
            "closed",
            "one dead origin must not disable lightpanda for every other host"
        );
        assert_eq!(
            r.breakers
                .host_for("dead-origin.example", RendererKind::Lightpanda)
                .await
                .snapshot()
                .state,
            "open",
            "the host tier must still learn that this origin is unreachable"
        );
    }

    /// A skipped tier must report the budget the CALLER was given, never the
    /// sub-minimum remainder that is left when the skip fires.
    ///
    /// Regression guard for the production bug this pairs with: `remaining` is by
    /// definition below MIN_TIER_BUDGET on that branch and in production is ~0, so
    /// it rendered as the literal `Timeout after 1ms` on requests that had been
    /// granted 30s and had spent ~25s of it walking the ladder. The sibling test
    /// above only asserts the Timeout *variant*, so without this the exact bug can
    /// come back without failing anything.
    #[tokio::test]
    async fn budget_skip_reports_the_requested_budget_not_the_remainder() {
        let slow_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let slow = Arc::new(SlowFailingFetcher {
            name: "lightpanda",
            burn: Duration::from_millis(1_200),
            calls: slow_calls.clone(),
        });
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let chrome = Arc::new(CountingFetcher {
            name: "chrome",
            calls: calls.clone(),
        });
        let r = make_renderer_with_mocks(vec![slow, chrome]);

        let err = r
            .fetch_with_js(
                "https://example.com",
                &HashMap::new(),
                None,
                None,
                false,
                crw_core::Deadline::from_request_ms(1_500),
            )
            .await
            .expect_err("both tiers must fail");

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "chrome must be skipped for lack of budget, or this test proves nothing"
        );
        match err {
            CrwError::Timeout(ms) => assert_eq!(
                ms, 1_500,
                "a budget skip must report the requested budget (1500ms), not the \
                 sub-minimum remainder; got {ms}ms"
            ),
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    /// An HTTP tier that cannot reach the origin at all, for the two attribution tests
    /// below.
    struct Unreachable;
    #[async_trait::async_trait]
    impl PageFetcher for Unreachable {
        async fn fetch(
            &self,
            url: &str,
            _h: &HashMap<String, String>,
            _w: Option<u64>,
            _d: crw_core::Deadline,
        ) -> CrwResult<FetchResult> {
            Err(CrwError::TargetUnreachable(format!(
                "Could not reach {url}"
            )))
        }
        fn name(&self) -> &str {
            "http"
        }
        fn supports_js(&self) -> bool {
            false
        }
        async fn is_available(&self) -> bool {
            true
        }
    }

    /// When the HTTP tier could not reach the origin at all, that error must win over
    /// the JS tier's generic RendererError. `TargetUnreachable` maps to 422 (the caller
    /// gave us a dead target); `RendererError` falls through to a 500 and reads as "our
    /// server broke". Production emitted 11 such 500s that should have been 422s.
    #[tokio::test]
    async fn unreachable_origin_beats_js_renderer_error() {
        let js = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Err("Navigation failed: net::ERR_SSL".to_string()),
        });
        let mut r = make_renderer_with_mocks(vec![js]);
        r.http = Arc::new(Unreachable);
        r.render_js_default = None; // auto branch

        let err = r
            .fetch(
                "https://dead.example",
                &HashMap::new(),
                None, // render_js: auto
                None, // wait_for_ms
                None, // requested_renderer
                tdl(),
            )
            .await
            .expect_err("both tiers fail");

        assert!(
            matches!(err, CrwError::TargetUnreachable(_)),
            "an unreachable origin must surface as TargetUnreachable (422), not the JS \
             tier's RendererError (500); got {err:?}"
        );
    }

    /// The same rule when the JS tier TIMES OUT instead of reporting a navigation
    /// error. A host that blackholes SYNs hangs the browser rather than producing a
    /// `net::ERR_*`, so this is the shape the class actually takes in production: it
    /// surfaced as a 504 that paged the 5xx watchdog, told the caller to raise a
    /// `timeout` no host would ever answer, and billed them for it.
    #[tokio::test]
    async fn unreachable_origin_beats_js_timeout() {
        let js = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Timeout,
        });
        let mut r = make_renderer_with_mocks(vec![js]);
        r.http = Arc::new(Unreachable);
        r.render_js_default = None; // auto branch

        let err = r
            .fetch(
                "https://dead.example",
                &HashMap::new(),
                None, // render_js: auto
                None, // wait_for_ms
                None, // requested_renderer
                tdl(),
            )
            .await
            .expect_err("both tiers fail");

        assert!(
            matches!(err, CrwError::TargetUnreachable(_)),
            "a dead origin that hangs the browser must surface as TargetUnreachable \
             (422, refunded), not Timeout (504, billed); got {err:?}"
        );
    }

    /// An HTTP tier whose origin answered with a body that is not a web page.
    struct BinaryBody;
    #[async_trait::async_trait]
    impl PageFetcher for BinaryBody {
        async fn fetch(
            &self,
            _url: &str,
            _h: &HashMap<String, String>,
            _w: Option<u64>,
            _d: crw_core::Deadline,
        ) -> CrwResult<FetchResult> {
            Err(CrwError::UnsupportedContentType(
                "application/zip (1200 bytes): the body is binary".to_string(),
            ))
        }
        fn name(&self) -> &str {
            "http"
        }
        fn supports_js(&self) -> bool {
            false
        }
        async fn is_available(&self) -> bool {
            true
        }
    }

    /// A body that is not a web page must NOT climb the ladder. No browser turns
    /// a .docx into a page, so escalating one only spends a Chromium/Camoufox
    /// session before failing anyway, and the ladder's generic "no usable
    /// content" replaces the content type the caller needs to see.
    ///
    /// The call COUNT is the assertion that matters: the error variant alone
    /// would survive deleting the guard, since the JS tier's own failure loses
    /// to nothing here.
    #[tokio::test]
    async fn unsupported_content_type_does_not_climb_the_ladder_in_auto() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let js = Arc::new(CountingFetcher {
            name: "chrome",
            calls: calls.clone(),
        });
        let mut r = make_renderer_with_mocks(vec![js]);
        r.http = Arc::new(BinaryBody);
        r.render_js_default = None; // auto branch

        let err = r
            .fetch(
                "https://example.com/spec.docx",
                &HashMap::new(),
                None, // render_js: auto
                None,
                None,
                tdl(),
            )
            .await
            .expect_err("a binary body has nothing to extract");

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the JS renderer must never be invoked for a binary body"
        );
        assert!(
            matches!(err, CrwError::UnsupportedContentType(_)),
            "the content type must reach the caller, not the ladder's generic \
             failure; got {err:?}"
        );
    }

    /// Same rule on the forced-JS arm. `renderJs: true` (and every screenshot
    /// request, which is routed down this arm) fetches over HTTP first for the
    /// content-type check, so it hits the identical guard.
    #[tokio::test]
    async fn unsupported_content_type_does_not_climb_the_ladder_when_js_is_forced() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let js = Arc::new(CountingFetcher {
            name: "chrome",
            calls: calls.clone(),
        });
        let mut r = make_renderer_with_mocks(vec![js]);
        r.http = Arc::new(BinaryBody);

        let err = r
            .fetch(
                "https://example.com/spec.docx",
                &HashMap::new(),
                Some(true), // render_js: on
                None,
                None,
                tdl(),
            )
            .await
            .expect_err("a binary body has nothing to extract");

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "an explicit renderJs:true must not spend a browser on a binary body"
        );
        assert!(
            matches!(err, CrwError::UnsupportedContentType(_)),
            "got {err:?}"
        );
    }

    /// Control: with a healthy budget the same tier IS invoked. Guards against the
    /// floor silently disabling the ladder.
    #[tokio::test]
    async fn healthy_budget_still_invokes_js_tier() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mock = Arc::new(CountingFetcher {
            name: "chrome",
            calls: calls.clone(),
        });
        let r = make_renderer_with_mocks(vec![mock]);

        let res = r
            .fetch_with_js(
                "https://example.com",
                &HashMap::new(),
                None,
                None,
                false,
                tdl(),
            )
            .await
            .expect("healthy budget must render");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(res.html.contains("rendered"));
    }

    fn make_renderer_with_mocks(mocks: Vec<Arc<dyn PageFetcher>>) -> FallbackRenderer {
        // Builds a REAL HTTP fetcher. The forced-JS arm fetches HTTP before the
        // ladder (content-type check), so any test that exercises it should
        // override `r.http` rather than reach the network.
        let cfg = base_cfg(RendererMode::None);
        let mut r =
            FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        r.js_renderers = mocks;
        r
    }

    #[tokio::test]
    async fn proxy_active_lightpanda_only_fails_closed() {
        // When a proxy is active but the only JS renderer is lightpanda (which
        // cannot proxy), fetch_with_js must hard-error, never egress direct.
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(rich_html("LP-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp]);
        let entry = Arc::new(crw_core::ProxyEntry::parse("http://p:8080").unwrap());
        // Call fetch_with_js directly to isolate the lightpanda guard from the
        // HTTP pre-fetch (which would otherwise fail against the fake proxy).
        let res = REQUEST_PROXY
            .scope(Some(entry), async {
                r.fetch_with_js(
                    "https://example.com",
                    &HashMap::new(),
                    None,
                    None,
                    false,
                    crw_core::Deadline::from_request_ms(5000),
                )
                .await
            })
            .await;
        assert!(
            res.is_err(),
            "lightpanda-only + proxy active must fail closed, got {res:?}"
        );
    }

    #[tokio::test]
    async fn proxy_active_prefers_chrome_over_lightpanda() {
        // With a proxy active, lightpanda is skipped and chrome (proxy-capable)
        // serves the request.
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(rich_html("LP-")),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp, chrome]);
        let entry = Arc::new(crw_core::ProxyEntry::parse("http://p:8080").unwrap());
        let res = REQUEST_PROXY
            .scope(Some(entry), async {
                r.fetch_with_js(
                    "https://example.com",
                    &HashMap::new(),
                    None,
                    None,
                    false,
                    crw_core::Deadline::from_request_ms(5000),
                )
                .await
            })
            .await
            .unwrap();
        assert_eq!(res.rendered_with.as_deref(), Some("chrome"));
    }

    #[tokio::test]
    async fn fetch_with_pinned_renderer_filters_pool() {
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(rich_html("LP-")),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp, chrome]);

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("chrome"),
                tdl(),
            )
            .await
            .unwrap();
        assert!(result.html.contains("CHROME-"), "expected chrome output");
        assert_eq!(result.rendered_with.as_deref(), Some("chrome"));
    }

    #[tokio::test]
    async fn fetch_with_pinned_renderer_unknown_returns_error() {
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![chrome]);

        let err = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("lightpanda"),
                tdl(),
            )
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("lightpanda") && msg.contains("chrome"),
            "expected error to name pinned + available: {msg}"
        );
    }

    // -- impersonated-http tier routing ----------------------------------

    /// The Amazon interstitial fixture: HTTP 200, small body, the full IT
    /// sentence. Anything serving this shape must escalate.
    #[cfg(feature = "impersonated")]
    fn amazon_interstitial_html() -> String {
        "<html><body><p>Fai clic sul pulsante qui sotto per continuare a fare \
         acquisti.</p></body></html>"
            .to_string()
    }

    /// A recognizable SPA shell (no wall markers, `needs_js_rendering` true).
    #[cfg(feature = "impersonated")]
    fn spa_shell_html() -> String {
        r#"<html><body><div id="root"></div><script src="/app.js"></script></body></html>"#
            .to_string()
    }

    #[cfg(feature = "impersonated")]
    #[tokio::test]
    async fn fetch_auto_wall_falls_through_impersonated_before_js() {
        let http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::OkStatus(200, amazon_interstitial_html()),
        }) as Arc<dyn PageFetcher>;
        let impersonated = Arc::new(MockFetcher {
            name: "impersonated-http",
            behavior: MockBehavior::Ok(rich_html("IMP-")),
        }) as Arc<dyn PageFetcher>;
        // The ladder must never be consulted: a fetch here fails the test.
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Panic(
                "the JS ladder must not run when the impersonated hop accepts",
            ),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![chrome]);
        r.http = http;
        r.impersonated = Some(impersonated);

        let result = r
            .fetch(
                "https://www.amazon.it/dp/B0FHQGLXBP",
                &HashMap::new(),
                None,
                None,
                None,
                tdl(),
            )
            .await
            .unwrap();
        assert_eq!(
            result.rendered_with.as_deref(),
            Some("impersonated-http"),
            "the hop must win the wall shape"
        );
        assert!(result.html.contains("IMP-"));
        assert_eq!(
            result.render_decision,
            Some(RenderDecision::Failover {
                chain: vec![RendererKind::Http, RendererKind::ImpersonatedHttp],
                reason: FailoverErrorKind::VendorBlock,
            })
        );
    }

    #[cfg(feature = "impersonated")]
    #[tokio::test]
    async fn fetch_auto_clean_site_never_consults_impersonated() {
        let http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::Ok(rich_html("CLEAN-")),
        }) as Arc<dyn PageFetcher>;
        let impersonated = Arc::new(MockFetcher {
            name: "impersonated-http",
            behavior: MockBehavior::Panic("clean sites must stay on the plain HTTP tier"),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(Vec::new());
        r.http = http;
        r.impersonated = Some(impersonated);

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                None,
                None,
                None,
                tdl(),
            )
            .await
            .unwrap();
        assert_eq!(result.rendered_with.as_deref(), Some("http"));
        assert_eq!(
            result.render_decision,
            Some(RenderDecision::AutoDefault {
                chosen: RendererKind::Http
            })
        );
    }

    #[cfg(feature = "impersonated")]
    #[tokio::test]
    async fn fetch_auto_impersonated_still_blocked_continues_to_ladder() {
        let http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::OkStatus(200, amazon_interstitial_html()),
        }) as Arc<dyn PageFetcher>;
        let impersonated = Arc::new(MockFetcher {
            name: "impersonated-http",
            behavior: MockBehavior::OkStatus(200, amazon_interstitial_html()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![chrome]);
        r.http = http;
        r.impersonated = Some(impersonated);

        let result = r
            .fetch(
                "https://walled.example/product",
                &HashMap::new(),
                None,
                None,
                None,
                tdl(),
            )
            .await
            .unwrap();
        assert_eq!(
            result.rendered_with.as_deref(),
            Some("chrome"),
            "a hop that stays blocked must fall through to the ladder"
        );
    }

    #[cfg(feature = "impersonated")]
    #[tokio::test]
    async fn fetch_auto_spa_shell_skips_impersonated() {
        // An SPA shell needs JS; impersonation cannot execute it, so the hop
        // must not fire and the ladder serves directly.
        let http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::Ok(spa_shell_html()),
        }) as Arc<dyn PageFetcher>;
        let impersonated = Arc::new(MockFetcher {
            name: "impersonated-http",
            behavior: MockBehavior::Panic("the hop must not fire on SPA shapes"),
        }) as Arc<dyn PageFetcher>;
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(rich_html("LP-")),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp]);
        r.http = http;
        r.impersonated = Some(impersonated);

        let result = r
            .fetch(
                "https://spa.example/page",
                &HashMap::new(),
                None,
                None,
                None,
                tdl(),
            )
            .await
            .unwrap();
        assert_eq!(result.rendered_with.as_deref(), Some("lightpanda"));
    }

    /// A JS shell served BY THE HOP: the impersonated client cleared the TLS
    /// gate and got the origin's real server HTML, but that HTML is a
    /// client-rendered shell. It carries well over `MIN_RENDERED_TEXT_LEN`
    /// characters of nav/banner text, so the bare `acceptable` gate would take
    /// it and end the chain. The ladder must still run.
    #[cfg(feature = "impersonated")]
    fn hydrated_shell_html() -> String {
        format!(
            r#"<html><body><nav>{}</nav><div id="root"></div><script src="/app.js"></script></body></html>"#,
            "Home Shop Account Cart Help Sign in Cookies "
        )
    }

    #[cfg(feature = "impersonated")]
    #[tokio::test]
    async fn fetch_auto_impersonated_js_shell_continues_to_ladder() {
        // The hop clears the wall but comes back with a client-rendered shell.
        // Accepting it would bill a shell as a success and skip the browser
        // that renders the actual page, which is the recall regression the
        // hop's accept gate exists to prevent.
        let http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::OkStatus(200, amazon_interstitial_html()),
        }) as Arc<dyn PageFetcher>;
        let impersonated = Arc::new(MockFetcher {
            name: "impersonated-http",
            behavior: MockBehavior::OkStatus(200, hydrated_shell_html()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![chrome]);
        r.http = http;
        r.impersonated = Some(impersonated);

        let result = r
            .fetch(
                "https://shop.example.it/p/123",
                &HashMap::new(),
                None,
                None,
                None,
                tdl(),
            )
            .await
            .unwrap();
        assert_eq!(
            result.rendered_with.as_deref(),
            Some("chrome"),
            "a hop body the ladder would escalate must not end the chain"
        );
        assert!(result.html.contains("CHROME-"));
    }

    /// The pin has no ladder to fall through to, so it must return ordinary
    /// non-wall shapes as results. A thin body is not a wall: it gets an
    /// actionable hint, exactly as a hard browser pin's thin render does.
    #[cfg(feature = "impersonated")]
    #[tokio::test]
    async fn fetch_pinned_impersonated_http_thin_body_warns_instead_of_erroring() {
        let impersonated = Arc::new(MockFetcher {
            name: "impersonated-http",
            behavior: MockBehavior::OkStatus(
                200,
                "<html><body><p>Short.</p></body></html>".to_string(),
            ),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(Vec::new());
        r.impersonated = Some(impersonated);

        let result = r
            .fetch(
                "https://example.com/tiny",
                &HashMap::new(),
                None,
                None,
                Some("impersonated-http"),
                tdl(),
            )
            .await
            .expect("a thin but unwalled body must come back as a result");
        assert_eq!(result.rendered_with.as_deref(), Some("impersonated-http"));
        assert_eq!(
            result.render_decision,
            Some(RenderDecision::UserPinned {
                renderer: RendererKind::ImpersonatedHttp
            })
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("little or no content")),
            "a thin pinned result must carry the hint, got {:?}",
            result.warnings
        );
    }

    /// A PDF reaches the pin arm with an EMPTY `html` (the bytes live in
    /// `raw_bytes`), which every body-shaped detector reads as a structural
    /// failure. It is not a wall, and the pin must hand it back so the
    /// downstream PDF parser sees it.
    #[cfg(feature = "impersonated")]
    #[test]
    fn impersonated_pdf_result_is_not_a_wall() {
        let r = make_renderer_with_mocks(Vec::new());
        let mut res = fr(200, "");
        res.content_type = Some("application/pdf".to_string());
        res.raw_bytes = Some(b"%PDF-1.4".to_vec());
        res.rendered_with = Some("pdf".to_string());
        assert!(
            !r.impersonation_blocked(&res),
            "a PDF must not be reported as an anti-bot wall"
        );
        // The auto-chain hop is stricter on purpose: an empty body is not
        // content, so the ladder still gets its turn.
        assert!(!r.impersonation_accepted(&res));
    }

    /// A 404 is a real answer from the origin, not an anti-bot block. Pinning
    /// the tier on a crawl must not turn every dead link into a fabricated
    /// wall verdict.
    #[cfg(feature = "impersonated")]
    #[tokio::test]
    async fn fetch_pinned_impersonated_http_404_is_not_a_wall() {
        let impersonated = Arc::new(MockFetcher {
            name: "impersonated-http",
            behavior: MockBehavior::OkStatus(404, rich_html("GONE-")),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(Vec::new());
        r.impersonated = Some(impersonated);

        let result = r
            .fetch(
                "https://example.com/missing",
                &HashMap::new(),
                None,
                None,
                Some("impersonated-http"),
                tdl(),
            )
            .await
            .expect("a 404 body must come back as a result, not a wall error");
        assert_eq!(result.status_code, 404);
        assert_eq!(result.rendered_with.as_deref(), Some("impersonated-http"));
    }

    /// A 403 IS a wall, and the hard-pin contract says failures surface.
    #[cfg(feature = "impersonated")]
    #[tokio::test]
    async fn fetch_pinned_impersonated_http_hard_block_status_errors() {
        let impersonated = Arc::new(MockFetcher {
            name: "impersonated-http",
            behavior: MockBehavior::OkStatus(403, rich_html("DENIED-")),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(Vec::new());
        r.impersonated = Some(impersonated);

        let res = r
            .fetch(
                "https://walled.example/denied",
                &HashMap::new(),
                None,
                None,
                Some("impersonated-http"),
                tdl(),
            )
            .await;
        assert!(
            res.is_err(),
            "a pinned 403 must surface as an error, got {res:?}"
        );
    }

    #[cfg(feature = "impersonated")]
    #[tokio::test]
    async fn fetch_pinned_impersonated_http_uses_tier() {
        // The pin runs BEFORE any plain HTTP fetch (no forced renderJs, no
        // HTTP prefetch), so the http mock panicking proves the ordering.
        let http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::Panic("an impersonated pin must not fetch plain HTTP first"),
        }) as Arc<dyn PageFetcher>;
        let impersonated = Arc::new(MockFetcher {
            name: "impersonated-http",
            behavior: MockBehavior::Ok(rich_html("PIN-")),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(Vec::new());
        r.http = http;
        r.impersonated = Some(impersonated);

        let result = r
            .fetch(
                "https://www.amazon.it/dp/B0FHQGLXBP",
                &HashMap::new(),
                None,
                None,
                Some("impersonated-http"),
                tdl(),
            )
            .await
            .unwrap();
        assert_eq!(result.rendered_with.as_deref(), Some("impersonated-http"));
        assert_eq!(
            result.render_decision,
            Some(RenderDecision::UserPinned {
                renderer: RendererKind::ImpersonatedHttp
            })
        );
    }

    #[cfg(feature = "impersonated")]
    #[tokio::test]
    async fn fetch_pinned_impersonated_http_without_tier_errors() {
        // Tier disabled (config kill switch leaves the field None): the pin is
        // a hard contract, so it errors instead of silently falling back.
        let http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::Panic("a missing impersonated tier must error, not fetch HTTP"),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(Vec::new());
        r.http = http;
        r.impersonated = None;

        let res = r
            .fetch(
                "https://www.amazon.it/dp/B0FHQGLXBP",
                &HashMap::new(),
                None,
                None,
                Some("impersonated-http"),
                tdl(),
            )
            .await;
        assert!(res.is_err(), "expected an error, got {res:?}");
    }

    #[cfg(feature = "impersonated")]
    #[tokio::test]
    async fn pinned_impersonated_with_bad_request_proxy_fails_closed() {
        // REQUEST_PROXY is honored via `ImpersonatedFetcher::with_proxy`, which
        // is fail-closed: a malformed proxy URL must surface as an error, never
        // as a silent direct egress (real-IP leak) from the pinned tier.
        let impersonated = Arc::new(MockFetcher {
            name: "impersonated-http",
            behavior: MockBehavior::Panic("a set proxy must route the pin, never the shared tier"),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(Vec::new());
        r.impersonated = Some(impersonated);
        // A syntactically valid but unreachable proxy (loopback port 1):
        // the pin must error on the dead proxy rather than fall back to a
        // direct egress. The malformed-URL case is covered by the module's
        // `client_build_is_fail_closed_with_bad_proxy` unit test.
        let entry = Arc::new(crw_core::ProxyEntry::parse("http://127.0.0.1:1").unwrap());
        let res = REQUEST_PROXY
            .scope(Some(entry), async {
                r.fetch(
                    "https://example.com",
                    &HashMap::new(),
                    None,
                    None,
                    Some("impersonated-http"),
                    tdl(),
                )
                .await
            })
            .await;
        assert!(
            res.is_err(),
            "a pinned impersonated request with a dead proxy must error, got {res:?}"
        );
    }

    #[cfg(feature = "impersonated")]
    #[tokio::test]
    async fn fetch_pinned_impersonated_http_wall_shaped_result_errors() {
        // The pin runs the accept gate: a wall-shaped result surfaces as an
        // error, never as a billed userPinned success.
        let impersonated = Arc::new(MockFetcher {
            name: "impersonated-http",
            behavior: MockBehavior::OkStatus(200, amazon_interstitial_html()),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(Vec::new());
        r.impersonated = Some(impersonated);

        let res = r
            .fetch(
                "https://walled.example/product",
                &HashMap::new(),
                None,
                None,
                Some("impersonated-http"),
                tdl(),
            )
            .await;
        match res {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("anti-bot block"),
                    "expected the wall verdict in the error, got: {msg}"
                );
            }
            Ok(r) => panic!(
                "a wall-shaped pinned result must error, got Ok {:?}",
                r.rendered_with
            ),
        }
    }

    #[tokio::test]
    async fn fetch_pinned_impersonated_http_without_feature_errors() {
        // A pin naming a tier the instance cannot construct must error, never
        // silently fall back. Runs in both builds; with the feature on, the
        // tier is explicitly disabled to reach the same absent-tier state.
        #[cfg(feature = "impersonated")]
        {
            let mut r = make_renderer_with_mocks(Vec::new());
            r.impersonated = None;
            let res = r
                .fetch(
                    "https://example.com",
                    &HashMap::new(),
                    None,
                    None,
                    Some("impersonated-http"),
                    tdl(),
                )
                .await;
            assert!(res.is_err());
            return;
        }
        #[cfg(not(feature = "impersonated"))]
        {
            let r = make_renderer_with_mocks(Vec::new());
            let res = r
                .fetch(
                    "https://example.com",
                    &HashMap::new(),
                    None,
                    None,
                    Some("impersonated-http"),
                    tdl(),
                )
                .await;
            match res {
                Err(e) => assert!(
                    e.to_string().contains("impersonated"),
                    "expected the missing-tier error, got: {e}"
                ),
                Ok(r) => panic!("lean build must not serve an impersonated pin: {r:?}"),
            }
        }
    }

    #[tokio::test]
    async fn fetch_with_renderer_auto_uses_full_chain() {
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(rich_html("LP-")),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp, chrome]);

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                tdl(),
            )
            .await
            .unwrap();
        // First renderer in the chain wins when both succeed.
        assert!(result.html.contains("LP-"), "expected lightpanda first");
    }

    #[tokio::test]
    async fn failover_skips_renderer_that_returns_failed_render() {
        // LightPanda returns HTML with a Next.js error boundary marker.
        // The chain must skip it and use Chrome's healthy result.
        let bad_lp_html = format!(
            "<html><body><div id=\"__next-error-0\">{}</div></body></html>",
            "x".repeat(200)
        );
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(bad_lp_html),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-OK")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp, chrome]);

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                None,
                tdl(),
            )
            .await
            .unwrap();
        assert!(result.html.contains("CHROME-OK"));
        assert_eq!(result.rendered_with.as_deref(), Some("chrome"));
    }

    #[tokio::test]
    async fn failover_surfaces_warning_when_only_failed_render_available() {
        // Only LightPanda is configured and it returns a failed render. The
        // call must succeed (best-effort thin_result fallback) but the warning
        // must name the failure so callers can surface it to the user.
        let bad_lp_html = format!(
            "<html><body><div id=\"__next-error-0\">{}</div></body></html>",
            "x".repeat(200)
        );
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(bad_lp_html),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp]);

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                None,
                tdl(),
            )
            .await
            .unwrap();
        let warning = result.warning.expect("expected warning to be set");
        assert!(
            warning.contains("lightpanda") && warning.contains("nextjs_client_error"),
            "warning should name renderer + reason: {warning}"
        );
    }

    #[tokio::test]
    async fn failover_concats_warnings_across_two_failed_renderers() {
        // Both renderers return failed-render HTML. The fallback `thin_result`
        // should carry warnings from BOTH attempts so debugging captures the
        // full chain, not just the first failure.
        let bad_lp_html = format!(
            "<html><body><div id=\"__next-error-0\">{}</div></body></html>",
            "x".repeat(200)
        );
        let bad_chrome_html = format!(
            "<html><body><div id=\"__next_error__\">{}</div></body></html>",
            "y".repeat(200)
        );
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(bad_lp_html),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(bad_chrome_html),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp, chrome]);

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                None,
                tdl(),
            )
            .await
            .unwrap();
        let warning = result.warning.expect("expected warning to be set");
        assert!(
            warning.contains("lightpanda") && warning.contains("chrome"),
            "warning should mention both renderers: {warning}"
        );
    }

    #[tokio::test]
    async fn fetch_pinned_renderer_failure_propagates() {
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Err("boom".into()),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![chrome]);
        // Stub the HTTP tier: the forced-JS arm fetches it before the ladder, so
        // without this the assertion depends on reaching example.com over the
        // network — and now that a JS failure can fall back to the HTTP body,
        // this test is the only thing pinning the hard-pin exclusion.
        r.http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::Ok(rich_html("HTTP-")),
        });

        let err = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("chrome"),
                tdl(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("boom"));
    }

    /// `renderJs:true` used to be the only arm that threw away a perfectly good
    /// HTTP body when the JS ladder failed, so a forced-JS scrape returned a 504
    /// while holding the document.
    #[tokio::test]
    async fn forced_js_failure_falls_back_to_http_body() {
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Err("Timeout after 1ms".into()),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![chrome]);
        r.http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::Ok(rich_html("HTTP-")),
        });

        let res = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true), // forced JS
                None,
                None, // unpinned
                tdl(),
            )
            .await
            .expect("a failed JS ladder must not discard a valid HTTP body");
        assert!(res.html.contains("HTTP-"));
        // A 2xx fallback is the motivating case and the one most at risk of
        // going out silently: the caller asked for a browser, is billed either
        // way, and has nothing else in the response to tell them they got HTTP.
        assert!(
            res.warning
                .as_deref()
                .is_some_and(|w| w.contains("js_escalation_failed")),
            "every forced-JS fallback must be announced; got {:?}",
            res.warning
        );
        assert!(
            res.warning
                .as_deref()
                .is_some_and(|w| w.contains(JS_ESCALATION_FAILED)),
            "single.rs reads this exact prefix to skip re-escalation: {:?}",
            res.warning
        );
    }

    /// A 4xx/5xx body is usually an error shell, so the swap is surfaced rather
    /// than made silently — same rule the auto arm applies.
    #[tokio::test]
    async fn forced_js_failure_on_soft_block_warns() {
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Err("Timeout after 1ms".into()),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![chrome]);
        r.http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::OkStatus(403, rich_html("SHELL-")),
        });

        let res = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                None,
                tdl(),
            )
            .await
            .expect("soft-block bodies still ship, with a warning");
        assert!(
            res.warning
                .as_deref()
                .is_some_and(|w| w.contains("js_escalation_failed")),
            "the caller must be able to tell the JS tier failed; got {:?}",
            res.warning
        );
    }

    /// A capture has no HTTP substitute: returning a body with no `screenshot`
    /// field would silently drop the thing the caller actually asked for.
    #[tokio::test]
    async fn forced_js_failure_with_screenshot_fails_closed() {
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Err("Timeout after 1ms".into()),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![chrome]);
        r.http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::Ok(rich_html("HTTP-")),
        });

        // `render_js: None`, not `Some(true)`: a real caller just sends
        // `formats: ["screenshot"]`, and :1242 is what forces them onto this
        // arm. Passing None exercises that promotion together with the guard.
        let err = REQUEST_SCREENSHOT
            .scope(
                Some(ScreenshotReq { full_page: false }),
                r.fetch(
                    "https://example.com",
                    &HashMap::new(),
                    None,
                    None,
                    None,
                    tdl(),
                ),
            )
            .await
            .expect_err("a screenshot request cannot be satisfied by an HTTP body");
        assert!(err.to_string().contains("Timeout"), "got {err:?}");
    }

    /// The real production failure is a deadline, and `MockBehavior::Err` can
    /// only build a `RendererError` — so the timeout shape gets its own fetcher.
    #[tokio::test]
    async fn forced_js_timeout_falls_back_to_http_body() {
        struct TimesOut;
        #[async_trait::async_trait]
        impl PageFetcher for TimesOut {
            async fn fetch(
                &self,
                _u: &str,
                _h: &HashMap<String, String>,
                _w: Option<u64>,
                _d: crw_core::Deadline,
            ) -> CrwResult<FetchResult> {
                Err(CrwError::Timeout(1))
            }
            fn name(&self) -> &str {
                "chrome"
            }
            fn supports_js(&self) -> bool {
                true
            }
            async fn is_available(&self) -> bool {
                true
            }
        }

        let mut r = make_renderer_with_mocks(vec![Arc::new(TimesOut)]);
        r.http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::Ok(rich_html("HTTP-")),
        });

        let res = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                None,
                tdl(),
            )
            .await
            .expect("a ladder timeout must not discard a valid HTTP body");
        assert!(res.html.contains("HTTP-"));
        assert_eq!(res.rendered_with.as_deref(), Some("http"));
    }

    /// An HTTP-layer failure under `renderJs:true` must still reach the browser.
    /// It did not before: the arm used `?`, so a caller who explicitly asked for
    /// JS got LESS recall than one who asked for nothing.
    #[tokio::test]
    async fn forced_js_escalates_when_http_tier_fails() {
        struct Unreachable;
        #[async_trait::async_trait]
        impl PageFetcher for Unreachable {
            async fn fetch(
                &self,
                url: &str,
                _h: &HashMap<String, String>,
                _w: Option<u64>,
                _d: crw_core::Deadline,
            ) -> CrwResult<FetchResult> {
                Err(CrwError::TargetUnreachable(format!(
                    "Could not reach {url}"
                )))
            }
            fn name(&self) -> &str {
                "http"
            }
            fn supports_js(&self) -> bool {
                false
            }
            async fn is_available(&self) -> bool {
                true
            }
        }

        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![chrome]);
        r.http = Arc::new(Unreachable);

        let res = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                None,
                tdl(),
            )
            .await
            .expect("a dead HTTP tier must escalate, not abort the request");
        assert!(res.html.contains("CHROME-"));
    }

    #[tokio::test]
    async fn auto_promoted_host_tries_chrome_first() {
        // Pre-promote example.com via the preference learner so the loop
        // sorts chrome ahead of lightpanda even though lightpanda was
        // declared first. The first renderer in the executed order wins.
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(rich_html("LP-")),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp, chrome]);

        // Force-promote "example.com" by reaching the failure threshold.
        for _ in 0..3 {
            r.preferences
                .record_failure("example.com", &FailoverErrorKind::NextJsClientError)
                .await;
        }

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                None,
                tdl(),
            )
            .await
            .unwrap();
        assert!(
            result.html.contains("CHROME-"),
            "promoted host should hit chrome first, got: {}",
            &result.html[..80.min(result.html.len())]
        );
        assert_eq!(result.credit_cost, 1, "every renderer costs 1 credit");
        assert!(matches!(
            result.render_decision,
            Some(RenderDecision::AutoPromoted {
                chosen: RendererKind::Chrome,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn breaker_skipped_renderer_falls_through_to_next() {
        // Trip the per-host breaker for lightpanda, then verify the loop
        // skips it and uses chrome — without ever calling lightpanda.fetch.
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Err("would fire if reached".into()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-OK")),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp, chrome]);

        // Use a custom breaker config: long cooldown so the breaker can't
        // transition to half-open under parallel test load (the default
        // 5s cooldown was racing against scheduler latency on workspace runs).
        // Threshold/window stay tuned to default: 80 consecutive failures
        // satisfies min_calls=50 and far exceeds failure_rate=0.80.
        let breaker_cfg = BreakerConfig {
            base_cooldown: Duration::from_secs(300),
            max_cooldown: Duration::from_secs(300),
            ..BreakerConfig::default()
        };
        r.breakers = Arc::new(BreakerRegistry::new(breaker_cfg));
        for _ in 0..80 {
            r.breakers
                .record_result("example.com", RendererKind::Lightpanda, false)
                .await;
        }

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                None,
                tdl(),
            )
            .await
            .unwrap();
        assert!(result.html.contains("CHROME-OK"));
        assert!(matches!(
            result.render_decision,
            Some(RenderDecision::BreakerSkipped {
                skipped: RendererKind::Lightpanda,
                chosen: RendererKind::Chrome
            })
        ));
    }

    /// The ladder correctly refuses a wall, and then the HTTP-shell fallback
    /// hands the same wall back anyway. Prod log, 2026-09-03: "JS escalation
    /// failed for soft-block status; surfacing HTTP shell with warning: ...
    /// blocked by an anti-bot wall that none of the 3 renderer tier(s)
    /// attempted could clear". The ladder's verdict has to survive that
    /// substitution, or rejecting the wall upstream buys nothing.
    #[tokio::test]
    async fn http_shell_fallback_does_not_resurrect_a_wall() {
        let wall = "<html><body><h1>Security Check</h1>\
                    <p>Checking your browser before accessing the site</p></body></html>";
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(wall.to_string()),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp]);
        r.http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::Ok(wall.to_string()),
        });

        let out = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                None,
                tdl(),
            )
            .await;
        let out = out.expect("a wall is a verdict about the page, not a transport failure");
        assert_eq!(
            out.block.as_ref().map(|b| b.vendor.as_str()),
            Some("generic_block"),
            "the wall must come back carrying its verdict, not as content: {:?}",
            (out.rendered_with.as_deref(), out.html.len())
        );
    }

    /// The same substitution, with a shell that is NOT a wall. This is the
    /// recall half of the pair: the HTTP body is real content the ladder simply
    /// failed to improve on, and it must still be handed back clean, with no
    /// verdict stamped on it. Getting this wrong turns every failed escalation
    /// into a block.
    #[tokio::test]
    async fn http_shell_fallback_still_substitutes_a_real_page() {
        let wall = "<html><body><h1>Security Check</h1>\
                    <p>Checking your browser before accessing the site</p></body></html>";
        let real = "<html><body><article><h1>Real page</h1><p>\
                    Enough prose here that nothing could mistake this for an \
                    interstitial shell of any kind whatsoever.</p></article></body></html>";
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(wall.to_string()),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp]);
        r.http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::Ok(real.to_string()),
        });

        let out = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                None,
                tdl(),
            )
            .await
            .expect("a real HTTP body must still substitute for a failed ladder");
        assert!(out.html.contains("Real page"));
        assert!(
            out.block.is_none(),
            "a real page must not carry a wall verdict: {:?}",
            out.block
        );
    }

    /// Prod shipped an anti-bot interstitial to a paying customer as
    /// `success: true` with `creditCost: 1`, because the ladder tail returned
    /// the best thin result without re-reading the verdict every tier had
    /// already reached. Live case: https://www.prlib.ru/en/history/619410 on
    /// 2026-09-03, "Security Check / Checking your browser", 95 chars.
    #[tokio::test]
    async fn wall_the_whole_ladder_failed_to_clear_is_not_a_success() {
        let wall = "<html><body><h1>Security Check</h1>\
                    <p>Checking your browser before accessing the site</p></body></html>";
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(wall.to_string()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(wall.to_string()),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp, chrome]);
        // The HTTP tier cannot reach the origin, so the JS ladder actually runs
        // — the real shape of the prod case, where HTTP escalated and every JS
        // tier then hit the same wall.
        r.http = Arc::new(Unreachable);

        let out = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                None,
                tdl(),
            )
            .await;

        // The judgement is unchanged: a wall no tier could clear is not content.
        // Only its carrier moved. It used to be `CrwError::HttpError`, which the
        // server maps to 502 and which told the caller our gateway had broken
        // when in fact the target refused us. It now rides on the result, so the
        // scrape choke can turn it into `success: false` + `anti_bot` at 200.
        let out = out.expect("a wall is a verdict about the page, not a transport failure");
        let block = out
            .block
            .as_ref()
            .expect("a wall no tier could clear must carry its verdict");
        assert_eq!(block.vendor, "generic_block");
        assert!(
            block.reason.contains("anti-bot wall"),
            "expected the wall to be named, got: {}",
            block.reason
        );
    }

    /// The other half of the same gate: a page that is merely thin, with no
    /// wall phrasing, must still ship. Returning the best available body is the
    /// point of the ladder tail and the recall invariant rests on it.
    #[tokio::test]
    async fn thin_but_real_page_still_ships_from_the_ladder_tail() {
        let thin = "<html><body><h1>Notice</h1><p>Short but genuine page.</p></body></html>";
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(thin.to_string()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(thin.to_string()),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp, chrome]);
        // The forced-JS arm fetches HTTP first for the content-type check, so
        // the real fetcher would answer before the ladder ever runs.
        r.http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::Ok(thin.to_string()),
        });

        let out = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                None,
                tdl(),
            )
            .await;
        assert!(
            out.is_ok(),
            "a thin but wall-free body must still be returned, got {:?}",
            out.err()
        );
    }

    #[tokio::test]
    async fn user_pinned_failed_render_emits_warning() {
        // Pin lightpanda. It returns failed-render HTML (Next.js error
        // boundary). Because the user hard-pinned, no failover happens.
        // The thin result must carry an actionable warning so callers can
        // surface it instead of silently returning broken markdown.
        let bad_html = format!(
            "<html><body><div id=\"__next-error-0\">{}</div></body></html>",
            "x".repeat(200)
        );
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(bad_html),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp, chrome]);

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("lightpanda"),
                tdl(),
            )
            .await
            .unwrap();
        let pin_hint = result
            .warnings
            .iter()
            .find(|w| w.starts_with("Pinned renderer 'lightpanda'"));
        assert!(
            pin_hint.is_some(),
            "expected pin-failure hint in warnings, got: {:?}",
            result.warnings
        );
        let hint = pin_hint.unwrap();
        assert!(
            hint.contains("nextJsClientError"),
            "hint should name camelCase reason: {hint}"
        );
        assert!(
            hint.contains("renderer=\"chrome\""),
            "hint should suggest a fix: {hint}"
        );
        // chain stays single-element because user pinned → no chrome attempt
        assert!(matches!(
            result.render_decision,
            Some(RenderDecision::Failover { ref chain, .. }) if chain.len() == 1
        ));
    }

    #[tokio::test]
    async fn user_pinned_decision_records_credit_and_kind() {
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![chrome]);
        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("chrome"),
                tdl(),
            )
            .await
            .unwrap();
        assert_eq!(result.credit_cost, 1);
        assert!(matches!(
            result.render_decision,
            Some(RenderDecision::UserPinned {
                renderer: RendererKind::Chrome
            })
        ));
    }

    #[tokio::test]
    async fn js_tier_escalates_on_403_status() {
        // LightPanda returns 403 with content (e.g. WAF block masked as content).
        // The chain must escalate to Chrome instead of accepting the 403 body.
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::OkStatus(403, rich_html("BLOCKED-")),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp, chrome]);

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                tdl(),
            )
            .await
            .unwrap();
        assert!(
            result.html.contains("CHROME-"),
            "expected chrome output after lightpanda 403"
        );
        assert_eq!(result.status_code, 200);
    }

    #[tokio::test]
    async fn js_tier_escalates_on_vendor_block_with_200() {
        // LightPanda returns 200 with a Cloudflare challenge page. The chain
        // must escalate even though the status code is "successful".
        let cf_html = format!(
            "<html><head><script src=\"/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1\"></script></head><body>{}</body></html>",
            "x".repeat(200)
        );
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(cf_html),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp, chrome]);

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                tdl(),
            )
            .await
            .unwrap();
        assert!(
            result.html.contains("CHROME-"),
            "expected chrome output after lightpanda vendor block"
        );
    }

    #[tokio::test]
    async fn js_tier_escalates_on_wikimedia_block_with_200() {
        // Wikimedia serves its datacenter-IP ban as an HTTP-200 scriptless error
        // shell with NO <body> tag. The chain must escalate off it instead of
        // returning the shell as a successful render.
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(wikimedia_block_html()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp, chrome]);

        let result = r
            .fetch(
                "https://en.wikipedia.org/wiki/Radcliffe_College",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                tdl(),
            )
            .await
            .unwrap();
        assert!(
            result.html.contains("CHROME-"),
            "expected chrome output after lightpanda wikimedia block, got: {}",
            result.html
        );
    }

    #[tokio::test]
    async fn js_tier_accepts_200_clean_response() {
        // Regression: a clean 200 from the first renderer must still be
        // accepted — no false escalation triggered by the new gates.
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(rich_html("LP-CLEAN-")),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp, chrome]);

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                tdl(),
            )
            .await
            .unwrap();
        assert!(result.html.contains("LP-CLEAN-"));
        assert_eq!(result.status_code, 200);
    }

    /// A page the lightweight `detector` heuristics pass but the
    /// `crw_extract::antibot` classifier flags — a Reddit-class WAF block
    /// ("blocked by network security") served with HTTP 200.
    fn network_security_block_html() -> String {
        format!(
            "<html><body><article>You've been blocked by network security.{}</article></body></html>",
            "x".repeat(200)
        )
    }

    /// A 200-status vendor wall must tag the ladder as exhausted, exactly like a
    /// 403 one does.
    ///
    /// `crw_crawl::single` reads `JS_ESCALATION_FAILED` off the warning to decide
    /// whether the ladder is spent (`js_ladder_exhausted`). The tag used to be
    /// attached only on the `is_auth_blocked` arm, so a wall served under HTTP
    /// 200 — the canonical Turnstile shape — came back untagged and the whole
    /// ladder ran a SECOND time against a site that had just failed it, on a
    /// deadline it had already spent.
    #[tokio::test]
    async fn js_escalation_failure_tags_exhaustion_on_a_200_wall() {
        let js = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Err("Timeout after 5000ms".to_string()),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![js]);
        // HTTP 200 carrying a challenge shell: escalation fires via `is_blocked`,
        // NOT `is_auth_blocked` — the arm that never tagged.
        r.http = Arc::new(MockFetcher {
            name: "http",
            behavior: MockBehavior::OkStatus(
                200,
                "<html><head><title>Just a moment...</title></head><body>\
                 <div id=\"cf-browser-verification\"></div></body></html>"
                    .to_string(),
            ),
        });
        r.render_js_default = None; // auto branch

        let result = r
            .fetch(
                "https://walled.example",
                &HashMap::new(),
                None,
                None,
                None,
                tdl(),
            )
            .await
            .expect("falls back to the HTTP shell");

        let warning = result.warning.unwrap_or_default();
        assert!(
            warning.contains(JS_ESCALATION_FAILED),
            "a 200-status wall must report the ladder as exhausted so the caller \
             does not re-run it; got {warning:?}"
        );
    }

    #[tokio::test]
    async fn js_tier_escalates_to_chrome_proxy_on_antibot_block() {
        // lightpanda + chrome both return a 200 WAF block the detector
        // misses; only the residential chrome_proxy tier clears it.
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(network_security_block_html()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(network_security_block_html()),
        }) as Arc<dyn PageFetcher>;
        let chrome_proxy = Arc::new(MockFetcher {
            name: "chrome_proxy",
            behavior: MockBehavior::Ok(rich_html("PROXY-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp, chrome, chrome_proxy]);

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                tdl(),
            )
            .await
            .unwrap();
        assert!(
            result.html.contains("PROXY-"),
            "expected chrome_proxy output after antibot block"
        );
        assert_eq!(
            result.render_decision,
            Some(RenderDecision::Failover {
                chain: vec![
                    RendererKind::Lightpanda,
                    RendererKind::Chrome,
                    RendererKind::ChromeProxy,
                ],
                reason: FailoverErrorKind::AntibotBlock,
            })
        );
    }

    #[tokio::test]
    async fn chrome_proxy_arm_fires_below_old_floor() {
        // Regression lock for the budget-STARVATION bug: a non-CF hard block with
        // only a sliver of request deadline left (real prod: the ladder burns a
        // 15s scrape deadline down to ~2s before the arm) must STILL fire
        // chrome_proxy. The arm now runs on a fresh `Deadline::now_plus(ARM_BUDGET)`,
        // so any floor against `deadline.remaining()` is gone. Old 8s-floor code
        // fails here — the budget below only has to stay under that floor, so it
        // carries enough headroom to survive a loaded machine (a 2s budget made
        // this flake ~1 run in 3 when the whole suite runs in parallel).
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(network_security_block_html()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(network_security_block_html()),
        }) as Arc<dyn PageFetcher>;
        // Recovered content must out-size the thin block for best-result-wins.
        let chrome_proxy = Arc::new(MockFetcher {
            name: "chrome_proxy",
            behavior: MockBehavior::Ok(format!(
                "<html><body><article>PROXY-RECOVERED{}</article></body></html>",
                "y".repeat(400)
            )),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp, chrome, chrome_proxy]);
        r.auto_egress_escalation = true; // pull chrome_proxy into the gated arm

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                crw_core::Deadline::from_request_ms(6_000),
            )
            .await
            .unwrap();
        assert!(
            result.html.contains("PROXY-"),
            "chrome_proxy must fire well below the old 8s floor (fresh arm budget), got: {}",
            result.html
        );
    }

    #[tokio::test]
    async fn chrome_proxy_arm_load_shed_when_pool_saturated() {
        // Load-shed: when the arm's permits are all held (a burst of
        // datacenter-blocked URLs already saturating the residential pool), the
        // arm must NOT fire — a blocking acquire would queue the request for up
        // to the SaaS 120s timeout and collapse co-tenant throughput. No permit →
        // return the block, no chrome_proxy in the chain.
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(network_security_block_html()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(network_security_block_html()),
        }) as Arc<dyn PageFetcher>;
        let chrome_proxy = Arc::new(MockFetcher {
            name: "chrome_proxy",
            behavior: MockBehavior::Ok(rich_html("PROXY-")),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp, chrome, chrome_proxy]);
        r.auto_egress_escalation = true;
        // Drain every arm permit so try_acquire_owned() returns None.
        let held: Vec<_> = (0..r.chrome_proxy_arm_sem.available_permits())
            .map(|_| {
                r.chrome_proxy_arm_sem
                    .clone()
                    .try_acquire_owned()
                    .expect("permit")
            })
            .collect();
        assert_eq!(r.chrome_proxy_arm_sem.available_permits(), 0);

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                tdl(),
            )
            .await
            .unwrap();
        drop(held);
        assert!(
            !result.html.contains("PROXY-"),
            "arm must be load-shed (skipped) when no permit is free"
        );
        if let Some(RenderDecision::Failover { chain, .. }) = &result.render_decision {
            assert!(
                !chain.contains(&RendererKind::ChromeProxy),
                "chain must not include chrome_proxy when load-shed: {chain:?}"
            );
        }
    }

    #[tokio::test]
    async fn chrome_proxy_suppressed_on_cloudflare_challenge() {
        // chrome_proxy (default Chrome fingerprint) cannot clear a CF managed
        // challenge, so the arm must NOT fire on it — it would only burn the
        // deadline. The chain stops at chrome and returns the honest block.
        // 60s deadline isolates the suppression from the budget floor.
        let cf = format!(
            "<html><body><div id=\"cf-browser-verification\">Just a moment...</div>{}</body></html>",
            "x".repeat(200)
        );
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(cf.clone()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(cf),
        }) as Arc<dyn PageFetcher>;
        let chrome_proxy = Arc::new(MockFetcher {
            name: "chrome_proxy",
            behavior: MockBehavior::Ok(rich_html("PROXY-")),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp, chrome, chrome_proxy]);
        r.auto_egress_escalation = true; // pull chrome_proxy into the gated arm

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                tdl(),
            )
            .await
            .unwrap();
        assert!(
            !result.html.contains("PROXY-"),
            "chrome_proxy must be suppressed on a CF challenge, got proxy output"
        );
        if let Some(RenderDecision::Failover { chain, .. }) = &result.render_decision {
            assert!(
                !chain.contains(&RendererKind::ChromeProxy),
                "chain must not include chrome_proxy on a CF challenge: {chain:?}"
            );
        }
    }

    #[tokio::test]
    async fn chrome_proxy_suppressed_on_datadome_wall() {
        // DataDome is a fingerprint wall like CF — chrome_proxy can't clear it,
        // so the arm must suppress it rather than burn the deadline.
        let dd = format!(
            "<html><body><iframe src=\"https://geo.captcha-delivery.com/captcha/?cid=x\"></iframe>{}</body></html>",
            "x".repeat(200)
        );
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(dd.clone()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(dd),
        }) as Arc<dyn PageFetcher>;
        let chrome_proxy = Arc::new(MockFetcher {
            name: "chrome_proxy",
            behavior: MockBehavior::Ok(rich_html("PROXY-")),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp, chrome, chrome_proxy]);
        r.auto_egress_escalation = true; // pull chrome_proxy into the gated arm

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                tdl(),
            )
            .await
            .unwrap();
        assert!(
            !result.html.contains("PROXY-"),
            "chrome_proxy must be suppressed on a DataDome wall, got proxy output"
        );
        if let Some(RenderDecision::Failover { chain, .. }) = &result.render_decision {
            assert!(
                !chain.contains(&RendererKind::ChromeProxy),
                "chain must not include chrome_proxy on a DataDome wall: {chain:?}"
            );
        }
    }

    #[tokio::test]
    async fn chrome_proxy_suppressed_on_antibot_only_vendor_wall() {
        // Regression: a PerimeterX wall recognised ONLY by antibot::classify from
        // visible text (no `window._pxAppId` SDK marker, so `looks_like_vendor_block`
        // returns None and `looks_like_cloudflare_challenge` is false). It still
        // sets saw_hard_block via `antibot.signal.is_blocked()`, so the arm must
        // suppress it off the antibot signal — else the residential tier burns the
        // deadline on a fingerprint wall it can't clear.
        let px = format!(
            "<html><body><h1>Access to This Page Has Been Blocked</h1>{}</body></html>",
            "x".repeat(200)
        );
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::OkStatus(403, px.clone()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::OkStatus(403, px),
        }) as Arc<dyn PageFetcher>;
        let chrome_proxy = Arc::new(MockFetcher {
            name: "chrome_proxy",
            behavior: MockBehavior::Ok(rich_html("PROXY-")),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp, chrome, chrome_proxy]);
        r.auto_egress_escalation = true; // pull chrome_proxy into the gated arm

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                tdl(),
            )
            .await
            .unwrap();
        assert!(
            !result.html.contains("PROXY-"),
            "chrome_proxy must be suppressed on an antibot-detected PerimeterX wall"
        );
    }

    #[tokio::test]
    async fn antibot_block_returns_as_success_when_escalation_disabled() {
        // Kill switch: escalate_in_failover = false → classify() still runs
        // for telemetry, but the block page is returned as success with no
        // escalation. Proves the gate is wired correctly.
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(network_security_block_html()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp, chrome]);
        r.antibot.escalate_in_failover = false;

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                tdl(),
            )
            .await
            .unwrap();
        assert!(
            result.html.contains("network security"),
            "block page should be returned as-is when escalation is disabled"
        );
        assert_eq!(result.rendered_with.as_deref(), Some("lightpanda"));
    }

    #[tokio::test]
    async fn promoted_host_escalates_chrome_to_chrome_proxy_not_lightpanda() {
        // After host promotion the preference sort must place chrome_proxy
        // immediately after chrome — a chrome block escalates straight to
        // the residential tier, never back down to lightpanda.
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::Ok(rich_html("LP-")),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(network_security_block_html()),
        }) as Arc<dyn PageFetcher>;
        let chrome_proxy = Arc::new(MockFetcher {
            name: "chrome_proxy",
            behavior: MockBehavior::Ok(rich_html("PROXY-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp, chrome, chrome_proxy]);

        // Force-promote "example.com" so the loop sorts chrome first.
        for _ in 0..3 {
            r.preferences
                .record_failure("example.com", &FailoverErrorKind::NextJsClientError)
                .await;
        }

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                None,
                tdl(),
            )
            .await
            .unwrap();
        assert!(
            result.html.contains("PROXY-"),
            "expected chrome_proxy output"
        );
        assert_eq!(
            result.render_decision,
            Some(RenderDecision::Failover {
                chain: vec![RendererKind::Chrome, RendererKind::ChromeProxy],
                reason: FailoverErrorKind::AntibotBlock,
            }),
            "chrome must escalate straight to chrome_proxy, skipping lightpanda"
        );
    }

    // ---- Phase 2a: cloak-first routing hint (`force_cloak`) ----

    /// A cloak-arm stub that returns a caller-chosen body and counts its calls,
    /// so a test can assert both the body handling AND that the arm fired exactly
    /// once (the double-fire guard).
    #[cfg(feature = "cloak")]
    struct CountingBodyFetcher {
        name: &'static str,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        html: String,
        fail: bool,
    }

    #[cfg(feature = "cloak")]
    #[async_trait::async_trait]
    impl PageFetcher for CountingBodyFetcher {
        async fn fetch(
            &self,
            url: &str,
            _headers: &HashMap<String, String>,
            _wait_for_ms: Option<u64>,
            _deadline: crw_core::Deadline,
        ) -> CrwResult<FetchResult> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail {
                return Err(CrwError::RendererError("cloak stub failure".into()));
            }
            Ok(FetchResult {
                url: url.to_string(),
                final_url: None,
                status_code: 200,
                html: self.html.clone(),
                content_type: Some("text/html".to_string()),
                raw_bytes: None,
                rendered_with: Some(self.name.to_string()),
                elapsed_ms: 0,
                warning: None,
                render_decision: None,
                credit_cost: 0,
                warnings: Vec::new(),
                truncated: false,
                deadline_exceeded: false,
                captured_responses: Vec::new(),
                screenshot: None,
                block: None,
            })
        }
        fn name(&self) -> &str {
            self.name
        }
        fn supports_js(&self) -> bool {
            true
        }
        async fn is_available(&self) -> bool {
            true
        }
    }

    #[cfg(feature = "cloak")]
    fn renderer_with_cloak(
        cloak: Arc<dyn PageFetcher>,
        ladder: Vec<Arc<dyn PageFetcher>>,
    ) -> FallbackRenderer {
        let cfg = base_cfg(RendererMode::None);
        let mut r =
            FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        r.js_renderers = ladder;
        r.cloak_arm = Some(cloak);
        r
    }

    /// (i) force_cloak + cleared budget + a content-OK cloak arm: cloak fires
    /// FIRST and the ladder never runs.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_first_fires_before_ladder_on_success() {
        let ladder_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ladder = Arc::new(CountingFetcher {
            name: "chrome",
            calls: ladder_calls.clone(),
        });
        let cloak = Arc::new(MockFetcher {
            name: "cloak",
            behavior: MockBehavior::Ok(rich_html("CLOAKED")),
        });
        let r = renderer_with_cloak(cloak, vec![ladder]);
        let res = r
            .fetch_hinted(
                "https://glassdoor.com/x",
                &HashMap::new(),
                Some(true),
                None,
                None,
                true,
                tdl(),
            )
            .await
            .unwrap();
        assert_eq!(res.rendered_with.as_deref(), Some("cloak"));
        assert!(res.html.contains("CLOAKED"));
        assert_eq!(
            res.credit_cost, 1,
            "cloak-first success must stamp the flat 1-credit page cost"
        );
        assert!(
            res.render_decision.is_some(),
            "cloak-first success must stamp routing metadata"
        );
        assert_eq!(
            ladder_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "ladder must not run when cloak-first succeeds"
        );
    }

    /// (ii) RED-1: a caller deadline below CLOAK_ARM_FLOOR_MS (24s) must make the
    /// hint a no-op so the request runs today's ladder (recall-safe).
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_first_skipped_when_deadline_below_floor() {
        let cloak_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cloak = Arc::new(CountingFetcher {
            name: "cloak",
            calls: cloak_calls.clone(),
        });
        let ladder = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("LADDER")),
        });
        let r = renderer_with_cloak(cloak, vec![ladder]);
        let res = r
            .fetch_hinted(
                "https://glassdoor.com/x",
                &HashMap::new(),
                Some(true),
                None,
                None,
                true,
                // 5s: below the 24s cloak floor (hint ignored) but above the
                // ladder's own MIN_TIER_BUDGET so the ladder still renders.
                crw_core::Deadline::from_request_ms(5_000),
            )
            .await
            .unwrap();
        assert_eq!(
            cloak_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "cloak-first must not fire below the floor"
        );
        assert!(res.html.contains("LADDER"));
    }

    /// (v) RED-2: a thin cloak body must NOT be shipped; it falls through to the
    /// ladder, which returns the fuller render.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_first_falls_through_to_ladder_on_thin() {
        let cloak = Arc::new(MockFetcher {
            name: "cloak",
            behavior: MockBehavior::Ok("<html><body>hi</body></html>".to_string()),
        });
        let ladder = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("LADDER")),
        });
        let r = renderer_with_cloak(cloak, vec![ladder]);
        let res = r
            .fetch_hinted(
                "https://glassdoor.com/x",
                &HashMap::new(),
                Some(true),
                None,
                None,
                true,
                tdl(),
            )
            .await
            .unwrap();
        assert!(
            res.html.contains("LADDER"),
            "thin cloak result must fall through to the ladder"
        );
        assert_eq!(res.rendered_with.as_deref(), Some("chrome"));
    }

    /// (vi) Double-fire guard: cloak-first fires (thin) and falls through; the
    /// ladder returns a CF challenge; the post-ladder recovery arm must NOT fire
    /// the cloak arm a second time (one page, one cloak permit).
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_first_suppresses_post_ladder_recovery_arm() {
        let cloak_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cloak = Arc::new(CountingBodyFetcher {
            name: "cloak",
            calls: cloak_calls.clone(),
            html: "<html><body>hi</body></html>".to_string(), // thin -> fall through
            fail: false,
        });
        let challenge = format!(
            "<html><head><script>window._cf_chl_opt={{cvId:'3'}};</script></head><body>{}</body></html>",
            "x".repeat(300)
        );
        let ladder = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(challenge),
        });
        let r = renderer_with_cloak(cloak, vec![ladder]);
        let _ = r
            .fetch_hinted(
                "https://glassdoor.com/x",
                &HashMap::new(),
                Some(true),
                None,
                None,
                true,
                tdl(),
            )
            .await;
        assert_eq!(
            cloak_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "cloak arm must fire exactly once (no double-fire via the recovery arm)"
        );
    }

    /// (vi-b) Same suppression when cloak-first ERRORS (not just thin): the arm
    /// still fired (attempted=true), so the post-ladder recovery arm is suppressed.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_first_error_still_suppresses_recovery_arm() {
        let cloak_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cloak = Arc::new(CountingBodyFetcher {
            name: "cloak",
            calls: cloak_calls.clone(),
            html: String::new(),
            fail: true, // Err -> fall through
        });
        let challenge = format!(
            "<html><head><script>window._cf_chl_opt={{cvId:'3'}};</script></head><body>{}</body></html>",
            "x".repeat(300)
        );
        let ladder = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(challenge),
        });
        let r = renderer_with_cloak(cloak, vec![ladder]);
        let _ = r
            .fetch_hinted(
                "https://glassdoor.com/x",
                &HashMap::new(),
                Some(true),
                None,
                None,
                true,
                tdl(),
            )
            .await;
        assert_eq!(
            cloak_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "cloak-first Err must still suppress the recovery arm (fired once)"
        );
    }

    /// (viii) renderJs:false is an explicit caller contract; the hint must not
    /// silently run a browser-class cloak solve. Cloak-first is skipped.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_first_skipped_when_render_js_false() {
        let cloak_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cloak = Arc::new(CountingFetcher {
            name: "cloak",
            calls: cloak_calls.clone(),
        });
        let r = renderer_with_cloak(cloak, vec![]);
        // render_js = Some(false): the request must stay HTTP-only.
        let _ = r
            .fetch_hinted(
                "https://glassdoor.com/x",
                &HashMap::new(),
                Some(false),
                None,
                None,
                true,
                tdl(),
            )
            .await;
        assert_eq!(
            cloak_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "cloak-first must not fire when the caller set renderJs:false"
        );
    }

    /// (vii) force_cloak=false is byte-identical to today: cloak never pre-fires.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn force_cloak_false_never_fires_cloak() {
        let cloak_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cloak = Arc::new(CountingFetcher {
            name: "cloak",
            calls: cloak_calls.clone(),
        });
        let ladder = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("LADDER")),
        });
        let r = renderer_with_cloak(cloak, vec![ladder]);
        let res = r
            .fetch_hinted(
                "https://glassdoor.com/x",
                &HashMap::new(),
                Some(true),
                None,
                None,
                false,
                tdl(),
            )
            .await
            .unwrap();
        assert_eq!(
            cloak_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "force_cloak=false must never fire cloak"
        );
        assert!(res.html.contains("LADDER"));
    }

    // ---- Post-ladder cloak recovery arm: fresh decoupled budget (`cloak_recover_on_cf`) ----

    /// A cloak-arm stub that records the `Deadline::remaining()` it was called
    /// with, so a test can tell a FRESH budget apart from the exhausted shared
    /// one, and returns a caller-chosen "solved" body.
    #[cfg(feature = "cloak")]
    struct DeadlineRecordingFetcher {
        recorded_remaining: Arc<std::sync::Mutex<Option<Duration>>>,
        html: String,
    }

    #[cfg(feature = "cloak")]
    #[async_trait::async_trait]
    impl PageFetcher for DeadlineRecordingFetcher {
        async fn fetch(
            &self,
            url: &str,
            _headers: &HashMap<String, String>,
            _wait_for_ms: Option<u64>,
            deadline: crw_core::Deadline,
        ) -> CrwResult<FetchResult> {
            *self.recorded_remaining.lock().unwrap() = Some(deadline.remaining());
            Ok(FetchResult {
                url: url.to_string(),
                final_url: None,
                status_code: 200,
                html: self.html.clone(),
                content_type: Some("text/html".to_string()),
                raw_bytes: None,
                rendered_with: Some("cloak".to_string()),
                elapsed_ms: 0,
                warning: None,
                render_decision: None,
                credit_cost: 0,
                warnings: Vec::new(),
                truncated: false,
                deadline_exceeded: false,
                captured_responses: Vec::new(),
                screenshot: None,
                block: None,
            })
        }
        fn name(&self) -> &str {
            "cloak"
        }
        fn supports_js(&self) -> bool {
            true
        }
        async fn is_available(&self) -> bool {
            true
        }
    }

    /// A CF managed-challenge shell — same shape used by
    /// `cloak_first_suppresses_post_ladder_recovery_arm` — thin enough to be
    /// detected as a challenge but large enough (300 filler bytes) to win the
    /// largest-HTML best-result-wins race as `thin_result`.
    #[cfg(feature = "cloak")]
    fn cf_challenge_html() -> String {
        format!(
            "<html><head><script>window._cf_chl_opt={{cvId:'3'}};</script></head><body>{}</body></html>",
            "x".repeat(300)
        )
    }

    /// A solved cloak body, larger than [`cf_challenge_html`] so it always wins
    /// the best-result-wins length race in these tests (real solves are real
    /// page content, typically far larger than a challenge shell).
    #[cfg(feature = "cloak")]
    fn solved_html() -> String {
        format!(
            "<html><body><article>SOLVED{}</article></body></html>",
            "x".repeat(500)
        )
    }

    /// `cloak_recover_on_cf=true`: a CF-challenge thin result on a near-exhausted
    /// shared deadline (5s, below the 24s `CLOAK_ARM_FLOOR_MS`) still FIRES the
    /// post-ladder cloak recovery arm — on a FRESH budget
    /// (`CLOAK_ARM_RECOVER_BUDGET_MS`, 40s), not the exhausted shared one — and
    /// the solved body wins.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_recover_on_cf_fires_on_fresh_budget_when_enabled() {
        let recorded = Arc::new(std::sync::Mutex::new(None));
        let cloak = Arc::new(DeadlineRecordingFetcher {
            recorded_remaining: recorded.clone(),
            html: solved_html(),
        });
        let ladder = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(cf_challenge_html()),
        });
        let mut r = renderer_with_cloak(cloak, vec![ladder]);
        r.cloak_recover_on_cf = true;
        let res = r
            .fetch_hinted(
                "https://glassdoor.com/x",
                &HashMap::new(),
                Some(true),
                None,
                None,
                false, // force_cloak off: exercise ONLY the post-ladder recovery arm
                crw_core::Deadline::from_request_ms(5_000), // below CLOAK_ARM_FLOOR_MS (24s)
            )
            .await
            .unwrap();
        let remaining = recorded
            .lock()
            .unwrap()
            .expect("cloak recovery arm must have been called");
        assert!(
            remaining > Duration::from_secs(20),
            "cloak arm must run on a FRESH ~40s budget, not the exhausted 5s shared \
             deadline (recorded remaining: {remaining:?})"
        );
        assert!(
            res.html.contains("SOLVED"),
            "the solved cloak body must win over the CF-challenge thin result"
        );
        assert_eq!(res.rendered_with.as_deref(), Some("cloak"));
    }

    /// `cloak_recover_on_cf=false` (default): byte-identical to today — the same
    /// near-exhausted deadline + CF challenge must NOT fire the recovery arm.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_recover_on_cf_false_skips_arm_on_exhausted_deadline() {
        let recorded = Arc::new(std::sync::Mutex::new(None));
        let cloak = Arc::new(DeadlineRecordingFetcher {
            recorded_remaining: recorded.clone(),
            html: solved_html(),
        });
        let ladder = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(cf_challenge_html()),
        });
        let r = renderer_with_cloak(cloak, vec![ladder]); // cloak_recover_on_cf defaults false
        let res = r
            .fetch_hinted(
                "https://glassdoor.com/x",
                &HashMap::new(),
                Some(true),
                None,
                None,
                false,
                crw_core::Deadline::from_request_ms(5_000),
            )
            .await
            .unwrap();
        assert!(
            recorded.lock().unwrap().is_none(),
            "cloak recovery arm must be skipped when cloak_recover_on_cf is off"
        );
        assert_eq!(
            res.rendered_with.as_deref(),
            Some("chrome"),
            "the CF-challenge thin ladder result must ship unchanged (byte-identical to today)"
        );
    }

    // ---- cloak_after_egress: the challenge the residential arm uncovers ----

    /// Builds the shape no existing helper could: a real chrome_proxy auto-egress
    /// arm AND a cloak arm on one renderer. Every suppression test in this file
    /// builds one or the other, which is exactly why none of them could catch a
    /// regression on this path.
    #[cfg(feature = "cloak")]
    fn renderer_with_egress_and_cloak(
        proxy_body: String,
        cloak: Arc<dyn PageFetcher>,
    ) -> FallbackRenderer {
        renderer_with_egress_and_cloak_ladder(network_security_block_html(), proxy_body, cloak)
    }

    /// Same, with the ladder's body chosen by the test: the live-capture test
    /// needs the ladder to return the real Cloudflare 1020 block page.
    #[cfg(feature = "cloak")]
    fn renderer_with_egress_and_cloak_ladder(
        ladder_body: String,
        proxy_body: String,
        cloak: Arc<dyn PageFetcher>,
    ) -> FallbackRenderer {
        renderer_with_egress_and_cloak_ladder_status(200, ladder_body, proxy_body, cloak)
    }

    /// And with the ladder's HTTP status chosen too: the live 1020 page is
    /// served as a 403, and `saw_hard_block` reads the status.
    #[cfg(feature = "cloak")]
    fn renderer_with_egress_and_cloak_ladder_status(
        ladder_status: u16,
        ladder_body: String,
        proxy_body: String,
        cloak: Arc<dyn PageFetcher>,
    ) -> FallbackRenderer {
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::OkStatus(ladder_status, ladder_body.clone()),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::OkStatus(ladder_status, ladder_body),
        }) as Arc<dyn PageFetcher>;
        let chrome_proxy = Arc::new(MockFetcher {
            name: "chrome_proxy",
            behavior: MockBehavior::Ok(proxy_body),
        }) as Arc<dyn PageFetcher>;
        let mut r = make_renderer_with_mocks(vec![lp, chrome, chrome_proxy]);
        r.auto_egress_escalation = true;
        r.cloak_arm = Some(cloak);
        // After the arm has run, the shared deadline can never clear the 24s
        // floor, so the entry gate rests entirely on this flag. Without it the
        // test would pass for the wrong reason on a fast machine.
        r.cloak_recover_on_cf = true;
        r
    }

    /// The customer-reported shape: the ladder sees a WAF BLOCK (not a
    /// challenge), the residential arm clears the WAF and uncovers the managed
    /// challenge, and cloak now gets its turn and solves it.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_fires_on_a_challenge_the_egress_arm_uncovered() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cloak = Arc::new(CountingBodyFetcher {
            name: "cloak",
            calls: calls.clone(),
            html: solved_html(),
            fail: false,
        }) as Arc<dyn PageFetcher>;
        let mut r = renderer_with_egress_and_cloak(cf_challenge_html(), cloak);
        r.cloak_after_egress = true;

        let res = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                crw_core::Deadline::from_request_ms(6_000),
            )
            .await
            .unwrap();

        assert!(
            res.html.contains("SOLVED"),
            "the uncovered challenge must reach the cloak arm, got: {}",
            res.html
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one page must still buy exactly one cloak permit"
        );
    }

    /// The wrong-tier guard. A residential body that is a NON-CF wall (DataDome,
    /// PerimeterX, a plain bot wall) must NOT buy a cold interactive Turnstile
    /// solve. This is the test that fails if the verdict is ever read off
    /// `!r_ok` instead of the hoisted CF predicate.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_stays_out_when_the_egress_arm_hit_a_non_cf_wall() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cloak = Arc::new(CountingBodyFetcher {
            name: "cloak",
            calls: calls.clone(),
            html: solved_html(),
            fail: false,
        }) as Arc<dyn PageFetcher>;
        let mut r = renderer_with_egress_and_cloak(network_security_block_html(), cloak);
        r.cloak_after_egress = true;

        let _ = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                crw_core::Deadline::from_request_ms(6_000),
            )
            .await;

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a non-CF wall must never buy a cold Turnstile solve"
        );
    }

    /// Default off: with the flag clear the path is byte-identical to today, so
    /// the arm cannot start costing anyone a solve until an operator opts in.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_after_egress_is_off_by_default() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cloak = Arc::new(CountingBodyFetcher {
            name: "cloak",
            calls: calls.clone(),
            html: solved_html(),
            fail: false,
        }) as Arc<dyn PageFetcher>;
        // renderer_with_egress_and_cloak leaves cloak_after_egress at its default.
        let r = renderer_with_egress_and_cloak(cf_challenge_html(), cloak);
        assert!(!r.cloak_after_egress, "the flag must default to off");

        let _ = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                crw_core::Deadline::from_request_ms(6_000),
            )
            .await;

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "with the flag off nothing about this path may change"
        );
    }

    /// One page, one cloak permit, across the NEW route too. A cloak-first
    /// attempt that already ran must not be followed by a second solve just
    /// because the residential arm later uncovered a challenge.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_first_still_suppresses_the_after_egress_route() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cloak = Arc::new(CountingBodyFetcher {
            name: "cloak",
            calls: calls.clone(),
            html: "<html><body>thin</body></html>".to_string(),
            fail: false,
        }) as Arc<dyn PageFetcher>;
        let mut r = renderer_with_egress_and_cloak(cf_challenge_html(), cloak);
        r.cloak_after_egress = true;

        let _ = r
            .fetch_hinted(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                true, // force_cloak: the pre-ladder attempt runs and sets cloak_attempted
                crw_core::Deadline::from_request_ms(6_000),
            )
            .await;

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "cloak_attempted must still cap the page at one solve"
        );
    }

    /// The live Cloudflare 1020 block page (scrubbed capture) as the ladder's
    /// body: it carries no strong challenge marker and only one weak one, so
    /// `looks_like_cloudflare_challenge` is false, `antibot::classify` says
    /// GenericBlock (not a fingerprint-vendor wall), the chrome_proxy arm MUST
    /// still fire, and the challenge it uncovers MUST reach cloak. This is the
    /// test that would fail if the vendor stamp ever moved into `antibot.rs`.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_fires_after_egress_on_the_live_cf_block_page() {
        let page = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../crw-crawl/tests/fixtures/cloudflare_1020_block.html"
        ))
        .to_string();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // Out-sizes the 4.5KB block page on purpose: the arm's best-result-wins
        // compares RAW HTML length, and a genuinely solved person page is
        // ~190KB against the block page's ~4.5KB. A 540-byte `solved_html()`
        // would lose that race and the block page would ship, which says
        // nothing about routing.
        let solved = format!(
            "<html><body><article>SOLVED{}</article></body></html>",
            "x".repeat(page.len() + 1_000)
        );
        let cloak = Arc::new(CountingBodyFetcher {
            name: "cloak",
            calls: calls.clone(),
            html: solved,
            fail: false,
        }) as Arc<dyn PageFetcher>;
        let mut r =
            renderer_with_egress_and_cloak_ladder_status(403, page, cf_challenge_html(), cloak);
        r.cloak_after_egress = true;
        let res = r
            .fetch(
                "https://example.com/find/person/x",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                crw_core::Deadline::from_request_ms(6_000),
            )
            .await
            .unwrap();
        assert!(
            res.html.contains("SOLVED"),
            "the real 1020 page must not suppress the residential arm; got: {}",
            &res.html[..res.html.len().min(200)]
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// Dead band, after-egress route: a shared remainder that clears the 24s
    /// floor but cannot fit a whole mint used to be handed to the sidecar as
    /// is (~30s), which its guard refuses. The arm must take the fresh budget.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_after_egress_takes_a_fresh_budget_inside_the_dead_band() {
        let recorded = Arc::new(std::sync::Mutex::new(None));
        let cloak = Arc::new(DeadlineRecordingFetcher {
            recorded_remaining: recorded.clone(),
            html: solved_html(),
        }) as Arc<dyn PageFetcher>;
        let mut r = renderer_with_egress_and_cloak(cf_challenge_html(), cloak);
        r.cloak_after_egress = true;
        let _ = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                crw_core::Deadline::from_request_ms(30_000), // above the 24s floor
            )
            .await;
        let remaining = recorded.lock().unwrap().expect("cloak arm must fire");
        assert!(
            remaining > Duration::from_secs(35),
            "inside the band the arm must run on the fresh budget, not the ~30s \
             shared remainder (recorded {remaining:?})"
        );
    }

    /// Same rule on the ladder route, and `CLOAK_TIMEOUT_MS` is the knob: with
    /// the cloak tier timeout at 45s, a 40s shared remainder is below a whole
    /// mint and the arm must run on a fresh 45s budget.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn cloak_ladder_route_fresh_budget_follows_the_configured_timeout() {
        let recorded = Arc::new(std::sync::Mutex::new(None));
        let cloak = Arc::new(DeadlineRecordingFetcher {
            recorded_remaining: recorded.clone(),
            html: solved_html(),
        });
        let ladder = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(cf_challenge_html()),
        });
        let mut r = renderer_with_cloak(cloak, vec![ladder]);
        r.cloak_recover_on_cf = true;
        r.tier_timeouts
            .insert(RendererKind::Cloak, Duration::from_secs(45));
        let _ = r
            .fetch_hinted(
                "https://glassdoor.com/x",
                &HashMap::new(),
                Some(true),
                None,
                None,
                false,
                crw_core::Deadline::from_request_ms(40_000),
            )
            .await;
        let remaining = recorded.lock().unwrap().expect("cloak arm must fire");
        assert!(
            remaining > Duration::from_secs(43),
            "a 40s remainder cannot fit a 45s mint: fresh budget expected \
             (recorded {remaining:?})"
        );
    }

    /// The breaker verdict is gated on the flag: with it OFF a residential body
    /// that is a CF challenge is still a RenderError and opens the ChromeProxy
    /// host breaker exactly as before (main's behaviour); with it ON the same
    /// body is SiteBlocked and the breaker stays closed, so the arm keeps
    /// uncovering the challenge for cloak. Low `min_calls` so a handful of
    /// fetches decides it.
    #[cfg(feature = "cloak")]
    #[tokio::test]
    async fn breaker_verdict_on_an_uncovered_challenge_follows_the_flag() {
        use crate::breaker::{BreakerConfig, BreakerRegistry};
        async fn run(flag: bool) -> bool {
            let cloak = Arc::new(MockFetcher {
                name: "cloak",
                behavior: MockBehavior::Ok(solved_html()),
            }) as Arc<dyn PageFetcher>;
            let mut r = renderer_with_egress_and_cloak(cf_challenge_html(), cloak);
            r.cloak_after_egress = flag;
            r.breakers = Arc::new(BreakerRegistry::new(BreakerConfig {
                window_size: 10,
                min_calls: 4,
                failure_rate_threshold: 0.5,
                base_cooldown: Duration::from_secs(60),
                max_cooldown: Duration::from_secs(60),
                max_probes: 1,
                half_open_success_rate: 1.0,
                eval_timeout: Duration::from_secs(1),
                ejection_reset_after_closed: Duration::from_secs(60),
                count_truncated_as_failure: false,
            }));
            for _ in 0..6 {
                let _ = r
                    .fetch(
                        "https://example.com/p",
                        &HashMap::new(),
                        Some(true),
                        None,
                        Some("auto"),
                        crw_core::Deadline::from_request_ms(6_000),
                    )
                    .await;
            }
            r.breakers
                .host_for("example.com", RendererKind::ChromeProxy)
                .await
                .is_open()
        }
        assert!(
            run(false).await,
            "flag off: an uncovered challenge is a RenderError and must open the \
             ChromeProxy host breaker (main's behaviour)"
        );
        assert!(
            !run(true).await,
            "flag on: the same body is SiteBlocked and must leave the breaker closed"
        );
    }

    // =====================================================================
    // Pure-function coverage: host_of, pick_ua, renderer_kind_for,
    // classify_renderer_error, tier_timeouts_from, credit_for,
    // stamp_http_decision, has_recovery_tier, is_fingerprint_vendor_wall,
    // is_origin_navigation_failure, is_soft_block_status,
    // renderer_can_screenshot, screenshot task-locals, is_html_like_content_type.
    // None of these need a browser, network, or sleep — every case below is
    // deterministic given the source they exercise.
    // =====================================================================

    use crw_extract::antibot::AntibotSignal;

    // -- host_of --------------------------------------------------------

    #[test]
    fn host_of_plain_https() {
        assert_eq!(host_of("https://example.com/path"), "example.com");
    }

    #[test]
    fn host_of_ignores_port() {
        assert_eq!(host_of("http://example.com:8080/x"), "example.com");
    }

    #[test]
    fn host_of_ignores_query_and_fragment() {
        assert_eq!(
            host_of("https://example.com/path?q=1&x=2#frag"),
            "example.com"
        );
    }

    #[test]
    fn host_of_lowercases_the_host() {
        // WHATWG URL parsing lowercases the host; this matters because host
        // preferences / breakers key on this string and must not fork state
        // for "Example.com" vs "example.com".
        assert_eq!(host_of("https://Example.COM/x"), "example.com");
    }

    #[test]
    fn host_of_strips_userinfo() {
        assert_eq!(host_of("https://user:pass@example.com/x"), "example.com");
    }

    #[test]
    fn host_of_works_for_non_http_schemes() {
        assert_eq!(host_of("ftp://example.com/x"), "example.com");
    }

    #[test]
    fn host_of_ipv4_literal() {
        assert_eq!(host_of("https://192.168.1.1:3000/"), "192.168.1.1");
    }

    #[test]
    fn host_of_ipv6_literal() {
        assert_eq!(host_of("https://[::1]:9222/"), "[::1]");
    }

    #[test]
    fn host_of_punycode_idn_host() {
        // A non-ASCII host is normalized to its punycode (xn--) form by the
        // URL parser; host_of must not panic or return the raw unicode.
        let host = host_of("https://xn--nxasmq6b.example/");
        assert_eq!(host, "xn--nxasmq6b.example");
    }

    #[test]
    fn host_of_unicode_host_gets_punycode_encoded() {
        let host = host_of("https://\u{4f8b}\u{3048}.test/");
        assert!(
            host.starts_with("xn--"),
            "unicode host must come back punycode-encoded, got {host:?}"
        );
    }

    #[test]
    fn host_of_malformed_url_returns_empty_string() {
        assert_eq!(host_of("not a url at all"), "");
    }

    #[test]
    fn host_of_empty_string_returns_empty_string() {
        assert_eq!(host_of(""), "");
    }

    #[test]
    fn host_of_scheme_relative_string_returns_empty_string() {
        // No scheme at all — `Url::parse` requires an absolute URL.
        assert_eq!(host_of("example.com/path"), "");
    }

    // -- pick_ua ----------------------------------------------------------

    #[test]
    fn pick_ua_disabled_stealth_always_returns_default() {
        let stealth = StealthConfig {
            enabled: false,
            ..StealthConfig::default()
        };
        for _ in 0..10 {
            assert_eq!(pick_ua("my-default-ua/1.0", &stealth), "my-default-ua/1.0");
        }
    }

    #[test]
    fn pick_ua_enabled_with_custom_pool_stays_within_pool() {
        let pool = vec!["ua-a".to_string(), "ua-b".to_string(), "ua-c".to_string()];
        let stealth = StealthConfig {
            enabled: true,
            user_agents: pool.clone(),
            ..StealthConfig::default()
        };
        for _ in 0..30 {
            let picked = pick_ua("default", &stealth);
            assert!(pool.contains(&picked), "{picked} not in configured pool");
        }
    }

    #[test]
    fn pick_ua_enabled_with_single_item_pool_is_deterministic() {
        let stealth = StealthConfig {
            enabled: true,
            user_agents: vec!["only-one".to_string()],
            ..StealthConfig::default()
        };
        assert_eq!(pick_ua("default", &stealth), "only-one");
    }

    #[test]
    fn pick_ua_enabled_with_empty_pool_falls_back_to_builtin() {
        let stealth = StealthConfig {
            enabled: true,
            user_agents: Vec::new(),
            ..StealthConfig::default()
        };
        for _ in 0..30 {
            let picked = pick_ua("default", &stealth);
            assert!(
                BUILTIN_UA_POOL.contains(&picked.as_str()),
                "{picked} not in BUILTIN_UA_POOL"
            );
        }
    }

    // -- renderer_kind_for --------------------------------------------------

    #[test]
    fn renderer_kind_for_http() {
        assert_eq!(renderer_kind_for("http"), Some(RendererKind::Http));
    }

    #[test]
    fn renderer_kind_for_http_only_fallback_maps_to_http() {
        assert_eq!(
            renderer_kind_for("http_only_fallback"),
            Some(RendererKind::Http)
        );
    }

    #[test]
    fn renderer_kind_for_lightpanda() {
        assert_eq!(
            renderer_kind_for("lightpanda"),
            Some(RendererKind::Lightpanda)
        );
    }

    #[test]
    fn renderer_kind_for_chrome() {
        assert_eq!(renderer_kind_for("chrome"), Some(RendererKind::Chrome));
    }

    #[test]
    fn renderer_kind_for_chrome_proxy() {
        assert_eq!(
            renderer_kind_for("chrome_proxy"),
            Some(RendererKind::ChromeProxy)
        );
    }

    #[test]
    fn renderer_kind_for_camoufox() {
        assert_eq!(renderer_kind_for("camoufox"), Some(RendererKind::Camoufox));
    }

    #[test]
    fn renderer_kind_for_cloak() {
        assert_eq!(renderer_kind_for("cloak"), Some(RendererKind::Cloak));
    }

    #[test]
    fn renderer_kind_for_playwright_is_untracked() {
        // Doc comment: playwright is treated as a JS renderer but is not
        // tracked in metrics/preferences.
        assert_eq!(renderer_kind_for("playwright"), None);
    }

    #[test]
    fn renderer_kind_for_unknown_name_is_none() {
        assert_eq!(renderer_kind_for("some_future_renderer"), None);
    }

    #[test]
    fn renderer_kind_for_empty_string_is_none() {
        assert_eq!(renderer_kind_for(""), None);
    }

    #[test]
    fn renderer_kind_for_is_case_sensitive() {
        assert_eq!(renderer_kind_for("Chrome"), None);
        assert_eq!(renderer_kind_for("CHROME"), None);
    }

    // -- classify_renderer_error --------------------------------------------

    #[test]
    fn classify_renderer_error_timeout() {
        assert_eq!(
            classify_renderer_error(&CrwError::Timeout(1000)),
            FailoverErrorKind::LightpandaTimeout
        );
    }

    #[test]
    fn classify_renderer_error_target_unreachable() {
        assert_eq!(
            classify_renderer_error(&CrwError::TargetUnreachable("dead".into())),
            FailoverErrorKind::NetworkError
        );
    }

    #[test]
    fn classify_renderer_error_http_error() {
        assert_eq!(
            classify_renderer_error(&CrwError::HttpError("connection reset".into())),
            FailoverErrorKind::NetworkError
        );
    }

    #[test]
    fn classify_renderer_error_renderer_error_is_lightpanda_crash() {
        assert_eq!(
            classify_renderer_error(&CrwError::RendererError("ws disconnected".into())),
            FailoverErrorKind::LightpandaCrash
        );
    }

    #[test]
    fn classify_renderer_error_invalid_request_is_other() {
        // `Other` does NOT count for promotion — an invalid request is our
        // caller's mistake, not a LightPanda-attributable failure.
        assert_eq!(
            classify_renderer_error(&CrwError::InvalidRequest("bad url".into())),
            FailoverErrorKind::Other
        );
    }

    #[test]
    fn classify_renderer_error_rate_limited_is_other() {
        assert_eq!(
            classify_renderer_error(&CrwError::RateLimited),
            FailoverErrorKind::Other
        );
    }

    #[test]
    fn classify_renderer_error_not_found_is_other() {
        assert_eq!(
            classify_renderer_error(&CrwError::NotFound("x".into())),
            FailoverErrorKind::Other
        );
    }

    // -- tier_timeouts_from ---------------------------------------------

    #[test]
    fn tier_timeouts_from_reflects_explicit_overrides() {
        let cfg = RendererConfig {
            http_timeout_ms: Some(1_111),
            lightpanda_timeout_ms: Some(2_222),
            chrome_timeout_ms: Some(3_333),
            chrome_proxy_timeout_ms: Some(4_444),
            camoufox_timeout_ms: Some(5_555),
            cloak_timeout_ms: Some(6_666),
            ..Default::default()
        };
        let m = tier_timeouts_from(&cfg);
        assert_eq!(m.len(), 6, "one entry per JS-ladder renderer tier");
        assert_eq!(m[&RendererKind::Http], Duration::from_millis(1_111));
        assert_eq!(m[&RendererKind::Lightpanda], Duration::from_millis(2_222));
        assert_eq!(m[&RendererKind::Chrome], Duration::from_millis(3_333));
        assert_eq!(m[&RendererKind::ChromeProxy], Duration::from_millis(4_444));
        assert_eq!(m[&RendererKind::Camoufox], Duration::from_millis(5_555));
        assert_eq!(m[&RendererKind::Cloak], Duration::from_millis(6_666));
    }

    #[test]
    fn tier_timeouts_from_default_config_matches_getters() {
        // Self-consistency check that doesn't hardcode the actual default
        // millisecond values (which live in crw-core and can change).
        let cfg = RendererConfig::default();
        let m = tier_timeouts_from(&cfg);
        assert_eq!(
            m[&RendererKind::Http],
            Duration::from_millis(cfg.http_timeout())
        );
        assert_eq!(
            m[&RendererKind::Lightpanda],
            Duration::from_millis(cfg.lightpanda_timeout())
        );
        assert_eq!(
            m[&RendererKind::Chrome],
            Duration::from_millis(cfg.chrome_timeout())
        );
        assert_eq!(
            m[&RendererKind::ChromeProxy],
            Duration::from_millis(cfg.chrome_proxy_timeout())
        );
        assert_eq!(
            m[&RendererKind::Camoufox],
            Duration::from_millis(cfg.camoufox_timeout())
        );
        assert_eq!(
            m[&RendererKind::Cloak],
            Duration::from_millis(cfg.cloak_timeout())
        );
    }

    #[test]
    fn tier_timeouts_from_contains_every_kind_exactly_once() {
        let cfg = RendererConfig::default();
        let m = tier_timeouts_from(&cfg);
        for kind in [
            RendererKind::Http,
            RendererKind::Lightpanda,
            RendererKind::Chrome,
            RendererKind::ChromeProxy,
            RendererKind::Camoufox,
            RendererKind::Cloak,
        ] {
            assert!(m.contains_key(&kind), "missing entry for {kind:?}");
        }
    }

    // -- credit_for -----------------------------------------------------

    #[test]
    fn credit_for_is_flat_one_for_every_kind() {
        // Flat 1-credit-per-page invariant: the SaaS bills exactly 1 credit
        // per scrape regardless of which tier served it.
        for kind in [
            RendererKind::Http,
            RendererKind::Lightpanda,
            RendererKind::Chrome,
            RendererKind::ChromeProxy,
            RendererKind::Camoufox,
            RendererKind::Cloak,
            RendererKind::ImpersonatedHttp,
        ] {
            assert_eq!(credit_for(kind), 1, "{kind:?} must cost exactly 1 credit");
        }
    }

    // -- stamp_http_decision ---------------------------------------------

    fn fr(status: u16, html: &str) -> FetchResult {
        FetchResult {
            url: "https://example.com".to_string(),
            final_url: None,
            status_code: status,
            html: html.to_string(),
            content_type: Some("text/html".to_string()),
            raw_bytes: None,
            rendered_with: None,
            elapsed_ms: 0,
            warning: None,
            render_decision: None,
            credit_cost: 0,
            warnings: Vec::new(),
            truncated: false,
            deadline_exceeded: false,
            captured_responses: Vec::new(),
            screenshot: None,
            block: None,
        }
    }

    #[test]
    fn stamp_http_decision_pinned_http_is_user_pinned() {
        let mut result = fr(200, "<html></html>");
        stamp_http_decision(&mut result, Some("http"));
        assert_eq!(result.credit_cost, 1);
        assert_eq!(
            result.render_decision,
            Some(RenderDecision::UserPinned {
                renderer: RendererKind::Http
            })
        );
    }

    #[test]
    fn stamp_http_decision_no_requested_renderer_is_auto_default() {
        let mut result = fr(200, "<html></html>");
        stamp_http_decision(&mut result, None);
        assert_eq!(
            result.render_decision,
            Some(RenderDecision::AutoDefault {
                chosen: RendererKind::Http
            })
        );
    }

    #[test]
    fn stamp_http_decision_pinned_to_a_different_renderer_is_still_auto_default() {
        // Only an explicit `"http"` pin counts as UserPinned here — the HTTP
        // tier serving a request that pinned "chrome" (e.g. chrome unavailable)
        // is not the user's choice, so it must not be mislabeled as one.
        let mut result = fr(200, "<html></html>");
        stamp_http_decision(&mut result, Some("chrome"));
        assert_eq!(
            result.render_decision,
            Some(RenderDecision::AutoDefault {
                chosen: RendererKind::Http
            })
        );
    }

    #[test]
    fn stamp_http_decision_is_idempotent_when_already_set() {
        let mut result = fr(200, "<html></html>");
        result.render_decision = Some(RenderDecision::UserPinned {
            renderer: RendererKind::Chrome,
        });
        result.credit_cost = 42;
        stamp_http_decision(&mut result, Some("http"));
        assert_eq!(
            result.render_decision,
            Some(RenderDecision::UserPinned {
                renderer: RendererKind::Chrome
            }),
            "must not overwrite a decision a caller already stamped"
        );
        assert_eq!(result.credit_cost, 42, "must not touch credit_cost either");
    }

    // -- has_recovery_tier ------------------------------------------------

    fn named_mock(name: &'static str) -> Arc<dyn PageFetcher> {
        Arc::new(MockFetcher {
            name,
            behavior: MockBehavior::Ok(String::new()),
        })
    }

    #[test]
    fn has_recovery_tier_true_when_chrome_proxy_constructed() {
        let cfg = RendererConfig::default();
        assert!(has_recovery_tier(
            &cfg,
            &[named_mock("chrome_proxy")],
            false,
            false
        ));
    }

    #[test]
    fn has_recovery_tier_true_when_camoufox_included_in_auto() {
        let cfg = RendererConfig {
            camoufox: Some(crw_core::config::CamoufoxEndpoint {
                base_url: "http://127.0.0.1:9377".into(),
                api_key: String::new(),
                include_in_auto: true,
            }),
            ..Default::default()
        };
        assert!(has_recovery_tier(
            &cfg,
            &[named_mock("camoufox")],
            false,
            false
        ));
    }

    #[test]
    fn has_recovery_tier_false_when_camoufox_excluded_from_auto() {
        // A camoufox tier configured for pinned-only use (include_in_auto =
        // false) must NOT count as a recovery tier for the auto ladder.
        let cfg = RendererConfig {
            camoufox: Some(crw_core::config::CamoufoxEndpoint {
                base_url: "http://127.0.0.1:9377".into(),
                api_key: String::new(),
                include_in_auto: false,
            }),
            ..Default::default()
        };
        assert!(!has_recovery_tier(
            &cfg,
            &[named_mock("camoufox")],
            false,
            false
        ));
    }

    #[test]
    fn has_recovery_tier_true_via_cloak_arm_alone() {
        let cfg = RendererConfig::default();
        assert!(has_recovery_tier(&cfg, &[], true, false));
    }

    #[test]
    fn has_recovery_tier_true_via_http_fallback_proxy_alone() {
        let cfg = RendererConfig::default();
        assert!(has_recovery_tier(&cfg, &[], false, true));
    }

    #[test]
    fn has_recovery_tier_false_when_nothing_available() {
        let cfg = RendererConfig::default();
        assert!(!has_recovery_tier(&cfg, &[], false, false));
    }

    #[test]
    fn has_recovery_tier_chrome_alone_does_not_count() {
        // Plain "chrome" is the primary ladder tier, not a recovery arm — it
        // egresses direct and cannot clear an IP-reputation block on its own.
        let cfg = RendererConfig::default();
        assert!(!has_recovery_tier(
            &cfg,
            &[named_mock("chrome")],
            false,
            false
        ));
    }

    // -- is_fingerprint_vendor_wall -----------------------------------------

    #[test]
    fn vendor_wall_cf_challenge_flag_alone_is_true() {
        assert!(is_fingerprint_vendor_wall(true, None, AntibotSignal::None));
    }

    #[test]
    fn vendor_wall_datadome_vendor_block_is_true() {
        assert!(is_fingerprint_vendor_wall(
            false,
            Some("datadome"),
            AntibotSignal::None
        ));
    }

    #[test]
    fn vendor_wall_perimeterx_vendor_block_is_true() {
        assert!(is_fingerprint_vendor_wall(
            false,
            Some("perimeterx"),
            AntibotSignal::None
        ));
    }

    #[test]
    fn vendor_wall_kasada_vendor_block_is_true() {
        assert!(is_fingerprint_vendor_wall(
            false,
            Some("kasada"),
            AntibotSignal::None
        ));
    }

    #[test]
    fn vendor_wall_akamai_vendor_block_is_true() {
        assert!(is_fingerprint_vendor_wall(
            false,
            Some("akamai"),
            AntibotSignal::None
        ));
    }

    #[test]
    fn vendor_wall_imperva_vendor_block_is_true() {
        assert!(is_fingerprint_vendor_wall(
            false,
            Some("imperva"),
            AntibotSignal::None
        ));
    }

    #[test]
    fn vendor_wall_cloudflare_vendor_block_string_is_deliberately_excluded() {
        // Cloudflare is matched via the `cf_challenge` flag, not the
        // `vendor_block` string — otherwise a CF error-1020 IP block (which a
        // residential egress CAN recover) would be wrongly suppressed.
        assert!(!is_fingerprint_vendor_wall(
            false,
            Some("cloudflare"),
            AntibotSignal::None
        ));
    }

    #[test]
    fn vendor_wall_sucuri_vendor_block_is_not_a_wall() {
        assert!(!is_fingerprint_vendor_wall(
            false,
            Some("sucuri"),
            AntibotSignal::None
        ));
    }

    #[test]
    fn vendor_wall_antibot_signal_cloudflare_is_true() {
        assert!(is_fingerprint_vendor_wall(
            false,
            None,
            AntibotSignal::Cloudflare
        ));
    }

    #[test]
    fn vendor_wall_antibot_signal_datadome_is_true() {
        assert!(is_fingerprint_vendor_wall(
            false,
            None,
            AntibotSignal::Datadome
        ));
    }

    #[test]
    fn vendor_wall_antibot_signal_perimeterx_is_true() {
        assert!(is_fingerprint_vendor_wall(
            false,
            None,
            AntibotSignal::PerimeterX
        ));
    }

    #[test]
    fn vendor_wall_antibot_signal_akamai_is_true() {
        assert!(is_fingerprint_vendor_wall(
            false,
            None,
            AntibotSignal::Akamai
        ));
    }

    #[test]
    fn vendor_wall_antibot_signal_imperva_is_true() {
        assert!(is_fingerprint_vendor_wall(
            false,
            None,
            AntibotSignal::Imperva
        ));
    }

    #[test]
    fn vendor_wall_antibot_signal_kasada_is_true() {
        assert!(is_fingerprint_vendor_wall(
            false,
            None,
            AntibotSignal::Kasada
        ));
    }

    #[test]
    fn vendor_wall_antibot_signal_sucuri_is_not_a_wall() {
        // Sucuri / NetworkSecurity / GenericBlock / RateLimited /
        // StructuralFailure are IP-reputation-style blocks a residential
        // egress DOES recover, so they must stay out.
        assert!(!is_fingerprint_vendor_wall(
            false,
            None,
            AntibotSignal::Sucuri
        ));
    }

    #[test]
    fn vendor_wall_antibot_signal_network_security_is_not_a_wall() {
        assert!(!is_fingerprint_vendor_wall(
            false,
            None,
            AntibotSignal::NetworkSecurity
        ));
    }

    #[test]
    fn vendor_wall_antibot_signal_rate_limited_is_not_a_wall() {
        assert!(!is_fingerprint_vendor_wall(
            false,
            None,
            AntibotSignal::RateLimited
        ));
    }

    #[test]
    fn vendor_wall_antibot_signal_generic_block_is_not_a_wall() {
        assert!(!is_fingerprint_vendor_wall(
            false,
            None,
            AntibotSignal::GenericBlock
        ));
    }

    #[test]
    fn vendor_wall_antibot_signal_structural_failure_is_not_a_wall() {
        assert!(!is_fingerprint_vendor_wall(
            false,
            None,
            AntibotSignal::StructuralFailure
        ));
    }

    #[test]
    fn vendor_wall_antibot_signal_vercel_is_not_a_wall() {
        assert!(!is_fingerprint_vendor_wall(
            false,
            None,
            AntibotSignal::Vercel
        ));
    }

    #[test]
    fn vendor_wall_all_clear_is_false() {
        assert!(!is_fingerprint_vendor_wall(
            false,
            None,
            AntibotSignal::None
        ));
    }

    // -- is_origin_navigation_failure ---------------------------------------

    #[test]
    fn origin_nav_failure_target_unreachable_is_true() {
        assert!(is_origin_navigation_failure(&CrwError::TargetUnreachable(
            "dead".into()
        )));
    }

    #[test]
    fn origin_nav_failure_renderer_error_navigation_failed_is_true() {
        assert!(is_origin_navigation_failure(&CrwError::RendererError(
            "Navigation failed: net::ERR_CONNECTION_RESET".into()
        )));
    }

    #[test]
    fn origin_nav_failure_renderer_error_net_err_is_case_insensitive() {
        assert!(is_origin_navigation_failure(&CrwError::RendererError(
            "NET::ERR_NAME_NOT_RESOLVED".into()
        )));
    }

    #[test]
    fn origin_nav_failure_renderer_error_outbound_check_unavailable_is_true() {
        assert!(is_origin_navigation_failure(&CrwError::RendererError(
            "Outbound destination check unavailable: DNS timeout".into()
        )));
    }

    #[test]
    fn origin_nav_failure_renderer_error_unrelated_message_is_false() {
        // An internal fault (pool exhausted) must NOT be attributed to the
        // origin — doing so would blame the caller for our own outage.
        assert!(!is_origin_navigation_failure(&CrwError::RendererError(
            "CDP pool exhausted, no browser slots available".into()
        )));
    }

    #[test]
    fn origin_nav_failure_timeout_is_true() {
        // A JS-tier timeout is "absence of evidence, not evidence against" the
        // HTTP tier's already-verified TargetUnreachable finding.
        assert!(is_origin_navigation_failure(&CrwError::Timeout(5_000)));
    }

    #[test]
    fn origin_nav_failure_http_error_is_false() {
        assert!(!is_origin_navigation_failure(&CrwError::HttpError(
            "connection reset".into()
        )));
    }

    #[test]
    fn origin_nav_failure_unrelated_variant_is_false() {
        assert!(!is_origin_navigation_failure(&CrwError::RateLimited));
    }

    // -- is_soft_block_status -----------------------------------------------

    #[test]
    fn soft_block_status_covers_every_documented_code() {
        for code in [401, 403, 404, 405, 406, 410, 412, 429, 451, 500, 503] {
            assert!(is_soft_block_status(code), "{code} should be soft-block");
        }
    }

    #[test]
    fn soft_block_status_excludes_success_redirect_and_neighboring_codes() {
        for code in [
            200, 201, 204, 301, 302, 304, 400, 402, 407, 408, 409, 411, 413, 420, 428, 430, 450,
            452, 499, 501, 502, 504, 520, 599,
        ] {
            assert!(
                !is_soft_block_status(code),
                "{code} must not be treated as soft-block"
            );
        }
    }

    // -- renderer_can_screenshot ---------------------------------------------

    #[test]
    fn can_screenshot_allowlist_true() {
        for name in ["chrome", "chrome_proxy", "playwright"] {
            assert!(renderer_can_screenshot(name), "{name} should capture");
        }
    }

    #[test]
    fn can_screenshot_allowlist_false() {
        for name in ["lightpanda", "camoufox", "cloak", "http", "unknown", ""] {
            assert!(!renderer_can_screenshot(name), "{name} should not capture");
        }
    }

    #[test]
    fn can_screenshot_is_case_sensitive() {
        assert!(!renderer_can_screenshot("Chrome"));
    }

    // -- screenshot task-locals ------------------------------------------

    #[test]
    fn screenshot_state_defaults_to_absent_outside_any_scope() {
        assert!(!screenshot_requested());
        assert!(current_screenshot_req().is_none());
    }

    #[tokio::test]
    async fn screenshot_state_true_inside_a_some_scope() {
        REQUEST_SCREENSHOT
            .scope(Some(ScreenshotReq { full_page: true }), async {
                assert!(screenshot_requested());
                let req = current_screenshot_req().expect("expected Some");
                assert!(req.full_page);
            })
            .await;
    }

    #[tokio::test]
    async fn screenshot_state_full_page_false_is_preserved() {
        REQUEST_SCREENSHOT
            .scope(Some(ScreenshotReq { full_page: false }), async {
                let req = current_screenshot_req().expect("expected Some");
                assert!(!req.full_page);
            })
            .await;
    }

    #[tokio::test]
    async fn screenshot_state_false_inside_an_explicit_none_scope() {
        REQUEST_SCREENSHOT
            .scope(None, async {
                assert!(!screenshot_requested());
                assert!(current_screenshot_req().is_none());
            })
            .await;
    }

    // -- is_html_like_content_type: additional edge cases --------------------

    #[test]
    fn html_like_uppercase_is_normalized() {
        assert!(is_html_like_content_type(Some("TEXT/HTML")));
    }

    #[test]
    fn html_like_surrounding_whitespace_is_trimmed() {
        assert!(is_html_like_content_type(Some("  text/html  ")));
    }

    #[test]
    fn html_like_xml_variants_are_eligible() {
        assert!(is_html_like_content_type(Some("application/xml")));
        assert!(is_html_like_content_type(Some("text/xml")));
    }

    #[test]
    fn html_like_rss_and_json_ld_are_not_eligible() {
        assert!(!is_html_like_content_type(Some("application/rss+xml")));
        assert!(!is_html_like_content_type(Some("application/ld+json")));
    }

    #[test]
    fn html_like_content_type_with_charset_param_is_rejected() {
        // BUG: a real server almost always appends `; charset=...` to
        // `text/html`, and this function only matches the type EXACTLY —
        // so a legitimate HTML response with a charset parameter is
        // classified as "cannot be improved by a browser" and never gets a
        // chance to escalate on an empty-2xx anti-bot shell. Asserting
        // current behavior per the task rules (production code left
        // untouched); reported in the summary.
        assert!(!is_html_like_content_type(Some("text/html; charset=utf-8")));
        assert!(!is_html_like_content_type(Some("text/html;charset=UTF-8")));
    }

    // =====================================================================
    // classify_js_attempt: deterministic single-attempt classification.
    // Every expected field below was traced through `crw_extract::antibot::
    // classify` by hand (see PR discussion / RULES.md) rather than guessed,
    // since that classifier also feeds `hard_block`/`acceptable` here.
    // =====================================================================

    #[test]
    fn classify_js_attempt_accepts_rich_200_html() {
        let r = make_renderer_with_mocks(vec![]);
        let class = r.classify_js_attempt(&fr(200, &rich_html("PAGE-")));
        assert!(class.acceptable);
        assert!(!class.hard_block);
        assert!(!class.is_status_blocked);
        assert!(!class.unrecoverable_wall);
        assert_eq!(class.antibot.signal, AntibotSignal::None);
    }

    #[test]
    fn classify_js_attempt_403_is_hard_block_via_generic_antibot_signal() {
        let r = make_renderer_with_mocks(vec![]);
        let class = r.classify_js_attempt(&fr(403, &rich_html("PAGE-")));
        assert!(class.is_status_blocked);
        assert!(class.hard_block);
        assert!(!class.acceptable);
        assert!(class.antibot_blocked);
        assert_eq!(class.antibot.signal, AntibotSignal::GenericBlock);
        assert!(
            !class.unrecoverable_wall,
            "a generic 403 block is IP-reputation-shaped, not a fingerprint wall"
        );
    }

    #[test]
    fn classify_js_attempt_404_is_status_blocked_but_not_hard_block() {
        // 404 sits in the soft-block/`is_status_blocked` set but NOT in the
        // narrower `hard_block` status subset (401|403|429|503) — the two
        // sets deliberately diverge, and plain content with a 404 doesn't
        // trip the antibot classifier either.
        let r = make_renderer_with_mocks(vec![]);
        let class = r.classify_js_attempt(&fr(404, &rich_html("PAGE-")));
        assert!(class.is_status_blocked);
        assert!(!class.hard_block);
        assert!(!class.acceptable);
        assert_eq!(class.antibot.signal, AntibotSignal::None);
    }

    #[test]
    fn classify_js_attempt_429_is_deterministically_rate_limited() {
        // `crw_extract::antibot::classify` special-cases HTTP 429 with an
        // unconditional early return, independent of body content.
        let r = make_renderer_with_mocks(vec![]);
        let class = r.classify_js_attempt(&fr(429, &rich_html("PAGE-")));
        assert!(class.is_status_blocked);
        assert!(class.hard_block);
        assert!(!class.acceptable);
        assert_eq!(class.antibot.signal, AntibotSignal::RateLimited);
        assert!(!class.unrecoverable_wall);
    }

    #[test]
    fn classify_js_attempt_521_is_hard_block_and_unrecoverable_wall() {
        // `classify` special-cases HTTP 521 as Cloudflare unconditionally.
        // 521 is in the hard_block 520..=530 range but NOT in the
        // `is_status_blocked` set, and a Cloudflare antibot signal makes it
        // an unrecoverable fingerprint wall regardless of body content.
        let r = make_renderer_with_mocks(vec![]);
        let class = r.classify_js_attempt(&fr(521, &rich_html("PAGE-")));
        assert!(!class.is_status_blocked);
        assert!(class.hard_block);
        assert!(!class.acceptable);
        assert_eq!(class.antibot.signal, AntibotSignal::Cloudflare);
        assert!(class.unrecoverable_wall);
    }

    #[test]
    fn classify_js_attempt_generic_bot_wall_phrase_is_hard_block_not_unrecoverable() {
        let html = format!(
            "<html><body><p>Access Denied. You don't have permission to access \
             this resource on this server.{}</p></body></html>",
            "x".repeat(50)
        );
        let r = make_renderer_with_mocks(vec![]);
        let class = r.classify_js_attempt(&fr(200, &html));
        assert!(class.is_bot_wall);
        assert!(class.hard_block);
        assert!(!class.acceptable);
        assert!(!class.is_status_blocked);
        assert!(
            !class.unrecoverable_wall,
            "a generic bot-wall phrase is exactly the class a residential egress recovers"
        );
    }

    #[test]
    fn classify_js_attempt_nextjs_error_boundary_is_hard_block_via_structural_signal() {
        // Non-obvious: `failed_render` alone does not drive `hard_block` — it
        // is the antibot structural-integrity check (no <p>/<article>/... tag
        // in a small page) that flags this shell and makes it a hard block.
        let bad_html = format!(
            "<html><body><div id=\"__next-error-0\">{}</div></body></html>",
            "x".repeat(200)
        );
        let r = make_renderer_with_mocks(vec![]);
        let class = r.classify_js_attempt(&fr(200, &bad_html));
        assert_eq!(
            class.failed_render,
            Some(detector::FailedRenderReason::NextJsClientError)
        );
        assert!(!class.acceptable);
        assert!(class.hard_block);
        assert_eq!(class.antibot.signal, AntibotSignal::StructuralFailure);
        assert!(!class.unrecoverable_wall);
    }

    // =====================================================================
    // FallbackRenderer misc surface: js_capable, supports_screenshot,
    // check_health, proxy plumbing, shutdown, Debug.
    // =====================================================================

    #[test]
    fn js_capable_false_with_no_renderers_and_no_auto_egress() {
        let cfg = base_cfg(RendererMode::None);
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        assert!(!r.js_capable());
    }

    #[test]
    fn js_capable_true_with_a_js_renderer() {
        let r = make_renderer_with_mocks(vec![named_mock("chrome")]);
        assert!(r.js_capable());
    }

    #[test]
    fn js_capable_true_via_auto_egress_escalation_alone() {
        let mut r = make_renderer_with_mocks(vec![]);
        r.auto_egress_escalation = true;
        assert!(r.js_capable());
    }

    #[test]
    fn supports_screenshot_true_for_chrome() {
        let r = make_renderer_with_mocks(vec![named_mock("chrome")]);
        assert!(r.supports_screenshot());
    }

    #[test]
    fn supports_screenshot_true_for_chrome_proxy() {
        let r = make_renderer_with_mocks(vec![named_mock("chrome_proxy")]);
        assert!(r.supports_screenshot());
    }

    #[test]
    fn supports_screenshot_false_for_lightpanda_only() {
        let r = make_renderer_with_mocks(vec![named_mock("lightpanda")]);
        assert!(!r.supports_screenshot());
    }

    #[tokio::test]
    async fn check_health_reports_http_and_every_js_renderer() {
        let r = make_renderer_with_mocks(vec![named_mock("chrome"), named_mock("lightpanda")]);
        let health = r.check_health().await;
        assert_eq!(health.len(), 3);
        assert_eq!(health.get("http"), Some(&true));
        assert_eq!(health.get("chrome"), Some(&true));
        assert_eq!(health.get("lightpanda"), Some(&true));
    }

    #[test]
    fn pick_proxy_none_without_a_rotator() {
        let cfg = base_cfg(RendererMode::None);
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        assert!(r.pick_proxy(Some("example.com")).is_none());
        assert!(r.pick_proxy_for_url("https://example.com/x").is_none());
    }

    #[test]
    fn pick_proxy_some_with_a_configured_rotator() {
        let cfg = base_cfg(RendererMode::None);
        let rotator = crw_core::ProxyRotator::build(
            &[],
            Some("http://proxy.example:8080"),
            crw_core::ProxyRotation::StickyPerHost,
        )
        .unwrap()
        .unwrap();
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default())
            .unwrap()
            .with_proxy_rotator(Some(Arc::new(rotator)))
            .unwrap();
        assert!(r.pick_proxy(Some("example.com")).is_some());
        assert!(r.pick_proxy_for_url("https://example.com/x").is_some());
    }

    #[tokio::test]
    async fn shutdown_chrome_pool_is_a_safe_noop_without_pools() {
        let r = make_renderer_with_mocks(vec![named_mock("chrome")]);
        // Must return promptly and not panic even though no real pool exists.
        r.shutdown_chrome_pool(Duration::from_millis(10)).await;
    }

    #[test]
    fn with_host_limits_builder_chains_without_panicking() {
        let cfg = base_cfg(RendererMode::None);
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default())
            .unwrap()
            .with_host_limits(2.5, 3, 1);
        assert!(!r.js_capable());
    }

    #[test]
    fn debug_fmt_lists_configured_renderer_names() {
        let r = make_renderer_with_mocks(vec![named_mock("chrome"), named_mock("lightpanda")]);
        let debug = format!("{r:?}");
        assert!(debug.contains("chrome"));
        assert!(debug.contains("lightpanda"));
        assert!(debug.contains("http"));
    }

    // =====================================================================
    // 429 status: must fall through the ladder rather than hard-failing.
    // =====================================================================

    #[tokio::test]
    async fn js_tier_429_with_no_next_tier_falls_through_instead_of_erroring() {
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::OkStatus(429, rich_html("RATE-LIMITED-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![chrome]);

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                None,
                tdl(),
            )
            .await
            .expect("a 429 with content must fall through, not error");
        let warning = result
            .warning
            .expect("expected a warning explaining the 429");
        assert!(
            warning.contains("chrome") && warning.contains("429"),
            "warning should name renderer + status: {warning}"
        );
    }

    #[tokio::test]
    async fn js_tier_escalates_on_429_status() {
        let lp = Arc::new(MockFetcher {
            name: "lightpanda",
            behavior: MockBehavior::OkStatus(429, rich_html("RATE-LIMITED-")),
        }) as Arc<dyn PageFetcher>;
        let chrome = Arc::new(MockFetcher {
            name: "chrome",
            behavior: MockBehavior::Ok(rich_html("CHROME-")),
        }) as Arc<dyn PageFetcher>;
        let r = make_renderer_with_mocks(vec![lp, chrome]);

        let result = r
            .fetch(
                "https://example.com",
                &HashMap::new(),
                Some(true),
                None,
                Some("auto"),
                tdl(),
            )
            .await
            .unwrap();
        assert!(
            result.html.contains("CHROME-"),
            "expected chrome output after lightpanda 429"
        );
        assert_eq!(result.status_code, 200);
    }

    // =====================================================================
    // html_body_text_len: additional boundary / malformed-input coverage.
    // =====================================================================

    #[test]
    fn body_text_len_counts_unicode_chars_and_collapses_whitespace() {
        let html = "<html><body><p>h\u{e9}llo w\u{f6}rld \u{1f389}\u{1f389}</p></body></html>";
        assert_eq!(html_body_text_len(html), 14);
    }

    #[test]
    fn body_text_len_handles_very_long_input_without_panicking() {
        let html = format!("<html><body>{}</body></html>", "a".repeat(100_000));
        assert_eq!(html_body_text_len(&html), 100_000);
    }

    #[test]
    fn body_text_len_ignores_nesting_depth() {
        let html = format!(
            "<html><body>{}text{}</body></html>",
            "<div>".repeat(500),
            "</div>".repeat(500)
        );
        assert_eq!(html_body_text_len(&html), 4);
    }

    #[test]
    fn body_text_len_stops_at_an_unterminated_tag() {
        // A tag opened but never closed (truncated response) must not panic;
        // everything from the stray `<` onward is silently dropped as "in tag".
        let html = "<html><body>hello world<sp";
        assert_eq!(html_body_text_len(html), 11);
    }

    // =====================================================================
    // Second pass: remaining status-code enumeration, real vendor-block
    // fixtures reused from the escalation tests above, and a few more
    // pure-function boundaries.
    // =====================================================================

    #[test]
    fn classify_js_attempt_401_is_hard_block_like_403() {
        let r = make_renderer_with_mocks(vec![]);
        let class = r.classify_js_attempt(&fr(401, &rich_html("PAGE-")));
        assert!(class.is_status_blocked);
        assert!(class.hard_block);
        assert!(!class.acceptable);
    }

    #[test]
    fn classify_js_attempt_soft_block_codes_outside_hard_block_subset() {
        // 404/405/406/410/412/451/500 are soft-block (`is_status_blocked`) but
        // NOT in the narrower hard_block subset (401|403|429|503|520-530), and
        // plain content at these codes doesn't trip the antibot classifier
        // either — the two escalation signals genuinely diverge here.
        let r = make_renderer_with_mocks(vec![]);
        for code in [404u16, 405, 406, 410, 412, 451, 500] {
            let class = r.classify_js_attempt(&fr(code, &rich_html("PAGE-")));
            assert!(class.is_status_blocked, "{code} should be status-blocked");
            assert!(!class.hard_block, "{code} should not be hard_block");
            assert!(!class.acceptable, "{code} should not be acceptable");
            assert_eq!(
                class.antibot.signal,
                AntibotSignal::None,
                "{code} with plain content should not trip antibot"
            );
        }
    }

    #[test]
    fn classify_js_attempt_empty_body_is_placeholder_and_hard_block() {
        let r = make_renderer_with_mocks(vec![]);
        let class = r.classify_js_attempt(&fr(200, "<html><body></body></html>"));
        assert!(class.is_placeholder);
        assert!(!class.acceptable);
        assert!(
            class.hard_block,
            "near-empty 200 content trips antibot's StructuralFailure near-empty rule"
        );
        assert_eq!(class.antibot.signal, AntibotSignal::StructuralFailure);
    }

    #[test]
    fn classify_js_attempt_loading_marker_is_placeholder() {
        let r = make_renderer_with_mocks(vec![]);
        let class = r.classify_js_attempt(&fr(200, "<html><body>Loading...</body></html>"));
        assert!(class.is_placeholder);
        assert!(!class.acceptable);
    }

    #[test]
    fn classify_js_attempt_text_len_matches_html_body_text_len() {
        let html = rich_html("PAGE-");
        let r = make_renderer_with_mocks(vec![]);
        let class = r.classify_js_attempt(&fr(200, &html));
        assert_eq!(class.text_len, html_body_text_len(&html));
        assert_eq!(class.text_len, "PAGE-".len() + 200);
    }

    #[test]
    fn classify_js_attempt_perimeterx_text_only_wall_is_unrecoverable() {
        // Reuses the exact fixture proven in
        // `chrome_proxy_suppressed_on_antibot_only_vendor_wall`: no
        // `window._pxAppId` SDK marker, so the lighter `looks_like_vendor_block`
        // detector misses it entirely — only `antibot::classify`'s text-based
        // TIER2 pattern catches it, and it must be treated as an unrecoverable
        // fingerprint wall (needs the stealth tier, not residential egress).
        let px = format!(
            "<html><body><h1>Access to This Page Has Been Blocked</h1>{}</body></html>",
            "x".repeat(200)
        );
        let r = make_renderer_with_mocks(vec![]);
        let class = r.classify_js_attempt(&fr(403, &px));
        assert!(class.vendor_block.is_none());
        assert_eq!(class.antibot.signal, AntibotSignal::PerimeterX);
        assert!(class.antibot_blocked);
        assert!(class.hard_block);
        assert!(class.unrecoverable_wall);
    }

    // -- host_of: a few more boundary shapes ---------------------------------

    #[test]
    fn host_of_double_slash_path_does_not_confuse_host() {
        assert_eq!(host_of("https://example.com//a//b"), "example.com");
    }

    #[test]
    fn host_of_whitespace_only_string_returns_empty() {
        assert_eq!(host_of("   "), "");
    }

    #[test]
    fn host_of_unaffected_by_a_very_long_path() {
        let url = format!("https://example.com/{}", "a".repeat(5_000));
        assert_eq!(host_of(&url), "example.com");
    }

    // -- is_origin_navigation_failure: casing ---------------------------------

    #[test]
    fn origin_nav_failure_navigation_failed_is_case_insensitive() {
        assert!(is_origin_navigation_failure(&CrwError::RendererError(
            "NAVIGATION FAILED: could not reach host".into()
        )));
    }

    #[test]
    fn origin_nav_failure_net_err_substring_mid_sentence_matches() {
        assert!(is_origin_navigation_failure(&CrwError::RendererError(
            "chrome reported net::ERR_NAME_NOT_RESOLVED while loading".into()
        )));
    }

    // -- is_html_like_content_type: casing ------------------------------------

    #[test]
    fn html_like_uppercase_xml_variants() {
        assert!(is_html_like_content_type(Some("TEXT/XML")));
        assert!(is_html_like_content_type(Some("Application/XHTML+XML")));
    }

    #[test]
    fn html_like_multipart_form_data_is_not_eligible() {
        assert!(!is_html_like_content_type(Some("multipart/form-data")));
    }

    // -- tier_timeouts_from: partial override --------------------------------

    #[test]
    fn tier_timeouts_from_partial_override_leaves_rest_on_page_timeout() {
        let cfg = RendererConfig {
            chrome_timeout_ms: Some(9_999),
            ..Default::default()
        };
        let m = tier_timeouts_from(&cfg);
        assert_eq!(m[&RendererKind::Chrome], Duration::from_millis(9_999));
        // Unset timeouts fall back to page_timeout_ms (http/lightpanda) or
        // their own documented default (chrome_proxy = chrome + 15s).
        assert_eq!(
            m[&RendererKind::Http],
            Duration::from_millis(cfg.page_timeout_ms)
        );
        assert_eq!(
            m[&RendererKind::Lightpanda],
            Duration::from_millis(cfg.page_timeout_ms)
        );
        assert_eq!(
            m[&RendererKind::ChromeProxy],
            Duration::from_millis(cfg.chrome_proxy_timeout())
        );
    }

    // -- stamp_http_decision: case sensitivity --------------------------------

    #[test]
    fn stamp_http_decision_pin_match_is_case_sensitive() {
        let mut result = fr(200, "<html></html>");
        stamp_http_decision(&mut result, Some("HTTP"));
        assert_eq!(
            result.render_decision,
            Some(RenderDecision::AutoDefault {
                chosen: RendererKind::Http
            }),
            "\"HTTP\" must not match the lowercase \"http\" pin"
        );
    }

    // -- has_recovery_tier: order independence --------------------------------

    #[test]
    fn has_recovery_tier_finds_chrome_proxy_regardless_of_position() {
        let cfg = RendererConfig::default();
        let renderers = [
            named_mock("lightpanda"),
            named_mock("chrome"),
            named_mock("chrome_proxy"),
        ];
        assert!(has_recovery_tier(&cfg, &renderers, false, false));
    }

    // -- FallbackRenderer misc: a few more ------------------------------------

    #[test]
    fn supports_screenshot_false_with_no_js_renderers() {
        let cfg = base_cfg(RendererMode::None);
        let r = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap();
        assert!(!r.supports_screenshot());
    }

    #[test]
    fn js_renderer_names_preserves_construction_order() {
        let r = make_renderer_with_mocks(vec![
            named_mock("lightpanda"),
            named_mock("chrome"),
            named_mock("chrome_proxy"),
        ]);
        assert_eq!(
            r.js_renderer_names(),
            vec!["lightpanda", "chrome", "chrome_proxy"]
        );
    }

    #[tokio::test]
    async fn check_health_with_zero_js_renderers_still_reports_http() {
        let r = make_renderer_with_mocks(vec![]);
        let health = r.check_health().await;
        assert_eq!(health.len(), 1);
        assert_eq!(health.get("http"), Some(&true));
    }
}

#[cfg(all(test, feature = "camoufox", feature = "cloak"))]
mod camoufox_reserve_tests {
    use super::camoufox_challenge_reserve_ms;
    use crw_core::config::{
        CLOAK_ARM_FLOOR_MS, CamoufoxEndpoint, CloakEndpoint, RendererConfig, RendererMode,
    };

    fn cfg(
        mode: RendererMode,
        camoufox_in_auto: bool,
        cloak_base_url: &str,
        recover_on_cf: bool,
    ) -> RendererConfig {
        RendererConfig {
            mode,
            camoufox: Some(CamoufoxEndpoint {
                base_url: "http://camoufox:9377".to_string(),
                include_in_auto: camoufox_in_auto,
                ..Default::default()
            }),
            cloak: Some(CloakEndpoint {
                base_url: cloak_base_url.to_string(),
                ..Default::default()
            }),
            cloak_recover_on_cf: recover_on_cf,
            ..Default::default()
        }
    }

    /// The reserve exists only where the arm can fire after camoufox: auto
    /// mode with camoufox in the ladder, a cloak endpoint held as the floor
    /// gated recovery arm. A camoufox held out of auto, a camoufox mode, an
    /// arm that fires on its own budget, or no cloak at all: no reserve.
    #[test]
    fn reserve_tracks_the_cloak_arm_floor_gate() {
        let auto_in_ladder = cfg(RendererMode::Auto, true, "http://cloak:8000", false);
        assert_eq!(
            camoufox_challenge_reserve_ms(&auto_in_ladder),
            Some(CLOAK_ARM_FLOOR_MS + crate::camoufox::POLL_HANDOFF_MS)
        );
        let recover_on_cf = cfg(RendererMode::Auto, true, "http://cloak:8000", true);
        assert_eq!(camoufox_challenge_reserve_ms(&recover_on_cf), None);
        let held_out_of_auto = cfg(RendererMode::Auto, false, "http://cloak:8000", false);
        assert_eq!(camoufox_challenge_reserve_ms(&held_out_of_auto), None);
        let camoufox_mode = cfg(RendererMode::Camoufox, true, "http://cloak:8000", false);
        assert_eq!(camoufox_challenge_reserve_ms(&camoufox_mode), None);
        let no_cloak = cfg(RendererMode::Auto, true, "  ", false);
        assert_eq!(camoufox_challenge_reserve_ms(&no_cloak), None);
        assert_eq!(
            camoufox_challenge_reserve_ms(&RendererConfig::default()),
            None
        );
    }
}
