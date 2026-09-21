use async_trait::async_trait;
use crw_core::error::{CrwError, CrwResult};
use crw_core::types::{CapturedNetworkResponse, FetchResult};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore, broadcast};
use tokio_tungstenite::connect_async;

use crate::blocklist::{BlockReason, Blocklist};
use crate::cdp_conn::{CdpConnection, CdpEvent};
use crate::traits::PageFetcher;

/// Timeout for WebSocket connect handshake.
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Extra overhead budget for the overall fetch timeout (on top of page_timeout + wait_for).
/// Covers WS connect, target create, navigate commit, snapshot eval, and cleanup.
pub const FETCH_OVERHEAD: Duration = Duration::from_secs(5);
/// Timeout for the Target.closeTarget cleanup command.
const TARGET_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum number of challenge retry attempts.
pub const CHALLENGE_MAX_RETRIES: u32 = 3;
/// Delay between challenge retry polls (ms).
pub const CHALLENGE_POLL_INTERVAL_MS: u64 = 3000;

/// Wall-clock the post-navigate challenge loop can actually consume, for the
/// retry count THIS renderer was configured with.
///
/// Sized off the configured value, never `CHALLENGE_MAX_RETRIES`: the loop runs
/// `retries` iterations, so reserving the constant inflated the CDP outer
/// timeout (and through it the auto-extended request deadline) by every retry
/// the deployment had already turned off. Prod runs 1, so the constant was
/// reserving two poll intervals per CDP tier that no code path could spend.
fn challenge_retry_budget(retries: u32) -> Duration {
    Duration::from_millis(CHALLENGE_POLL_INTERVAL_MS * u64::from(retries))
}
/// Maximum time to poll for content stability when a loading placeholder
/// is detected after the initial wait.
pub const CONTENT_STABILITY_MAX_MS: u64 = 6000;
/// Interval between content-stability polls.
const CONTENT_STABILITY_TICK_MS: u64 = 500;
/// Max time to poll for a content selector before giving up and proceeding
/// with whatever HTML is currently rendered. Covers SPAs that hydrate after
/// `loadEventFired` (njcourts.gov, in-n-out.com, hangzhou customs, apploi).
pub const SPA_SELECTOR_MAX_MS: u64 = 8000;

/// Sum of per-tier CDP overhead in milliseconds — the difference between
/// `internal_timeout` (set by [`fetch_with_deadline`]) and the configured
/// per-tier `page_timeout`. Mirrored by `crw_core::config::CDP_TIER_OVERHEAD_MS`;
/// drift between the two is regression-tested in
/// `crates/crw-server/tests/cdp_constants_test.rs`.
pub const fn cdp_tier_overhead_ms() -> u64 {
    SPA_SELECTOR_MAX_MS
        + (CHALLENGE_MAX_RETRIES as u64) * CHALLENGE_POLL_INTERVAL_MS
        + CONTENT_STABILITY_MAX_MS
        + (FETCH_OVERHEAD.as_millis() as u64)
}
/// Interval between SPA selector polls.
const SPA_SELECTOR_TICK_MS: u64 = 200;
/// Body innerText length required before the SPA poll exits. Selectors mount
/// before content hydrates on apps like ticktick / happyhotel / qyxmbt /
/// smzdm — by waiting for body text to also pass this threshold, we avoid
/// snapshotting an empty shell that satisfies the selector check alone.
/// Pages with mostly-static content (nav, header, footer chrome) clear this
/// in the first poll tick; SPAs keep polling until hydration fills the body
/// or the budget elapses.
const SPA_BODY_TEXT_MIN_CHARS: u64 = 800;
/// Quiet-period for network-idle: number of ms with zero in-flight requests
/// before we consider the page "settled enough to snapshot". Mirrors
/// Playwright's `networkidle` (no requests for 500ms). XHR-driven SPAs that
/// finish their data fetch before innerText hits the threshold get an early
/// exit via this signal — recall lift on lazy-fetch pages.
const NETWORK_IDLE_QUIET_MS: i64 = 500;
/// latency-qn fast-ready: mandatory content floor (non-whitespace body chars)
/// below which we NEVER early-exit — prevents snapshotting an empty CSR shell
/// (the quality red line; CSR SPAs go load+idle with body still empty). Matches
/// detector::looks_like_thin_html's 200-char bar. NOTE: tried 64 to let short
/// pages (example.com ~167) early-exit, but under load it let large progressive
/// pages exit on a network lull at partial content (mtkxjs 180KB→46KB) — the
/// floor is the partial-content guard, keep it at 200. Short complete pages that
/// wait the ceiling still SUCCEED (just slower); that's latency, not a red line.
const SPA_CONTENT_FLOOR_CHARS: i64 = 200;
/// latency-qn fast-ready: networkAlmostIdle threshold (≤2 in-flight), the
/// Lighthouse/Puppeteer `networkidle2` signal. networkIdle (0) rarely fires on
/// chatty pages (analytics/polling keep ≥1) so the old ≤0 idle-exit never
/// triggered → 8s ceiling burns. ≤2 is the correct "settled" signal.
const ALMOST_IDLE_MAX_INFLIGHT: i64 = 2;
/// Max iterations for the auto-scroll lazy-load pass. 12 viewports usually
/// covers infinite-scroll feeds without making it a crawl.
const AUTO_SCROLL_MAX_STEPS: u32 = 12;
/// Wait between each scroll step (ms). 250 balances giving lazy images
/// time to fire against total cost.
const AUTO_SCROLL_STEP_DELAY_MS: u64 = 250;
/// Hard ceiling on the entire auto-scroll phase. If we hit this, we
/// snapshot whatever's there and move on.
const AUTO_SCROLL_BUDGET_MS: u64 = 2500;
/// HTML size threshold above which auto-scroll is skipped. Pages this
/// big almost always have all their content already; the scroll pass
/// just adds latency and risks pushing us over the deadline.
const AUTO_SCROLL_HTML_SIZE_LIMIT: usize = 200_000;
/// Hard cap on the click-to-reveal pass. After this many clicks, stop —
/// any further reveals are diminishing returns and risk navigating away.
const AUTO_CLICK_MAX_CLICKS: u32 = 5;
/// Wait between clicks so each reveal can layout / hydrate.
const AUTO_CLICK_DELAY_MS: u64 = 250;
/// Hard ceiling on the entire click-to-reveal phase.
const AUTO_CLICK_BUDGET_MS: u64 = 1500;
/// Selector list checked when the caller didn't pass `wait_for_ms` — typical
/// SPA root containers. The first match wins.
const SPA_CONTENT_SELECTORS: &str = "main, article, [role=main], #content, #root > *, #app > *";

/// Maximum number of XHR/fetch responses captured for fallback extraction.
const NET_CAPTURE_MAX_BODIES: usize = 30;
/// Hard cap on cumulative body bytes captured per page.
const NET_CAPTURE_MAX_TOTAL_BYTES: usize = 2_000_000;
/// Minimum body size (Content-Length when known) to bother fetching.
const NET_CAPTURE_MIN_BODY_SIZE: usize = 512;
/// Per-getResponseBody command timeout.
const NET_CAPTURE_GETBODY_TIMEOUT: Duration = Duration::from_millis(800);

/// Hard ceiling (CSS px) on full-page screenshot height. `captureBeyondViewport`
/// makes Chrome rasterize the ENTIRE scroll height into one bitmap; a very tall
/// page (endless feed, huge article) spikes memory and OOMs a 2GB Chrome (#161).
/// Above this we clip instead of relying on captureBeyondViewport. Same spirit as
/// NET_CAPTURE_MAX_TOTAL_BYTES. Override: CRW_RENDERER__SCREENSHOT_MAX_HEIGHT_PX.
const SCREENSHOT_MAX_HEIGHT_PX: f64 = 15_000.0;

fn screenshot_max_height_px() -> f64 {
    std::env::var("CRW_RENDERER__SCREENSHOT_MAX_HEIGHT_PX")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(SCREENSHOT_MAX_HEIGHT_PX)
}

/// JavaScript injected via `Page.addScriptToEvaluateOnNewDocument` before every
/// navigation to prevent headless browser detection by anti-bot systems.
const STEALTH_JS: &str = r#"
// 1. Hide navigator.webdriver (primary headless signal for Cloudflare)
Object.defineProperty(navigator, 'webdriver', { get: () => false });

// 2. Fake chrome runtime object (missing in headless)
if (!window.chrome) {
    window.chrome = { runtime: {}, loadTimes: function(){}, csi: function(){} };
}

// 3. Spoof plugins array (headless has 0 plugins)
Object.defineProperty(navigator, 'plugins', {
    get: () => {
        const arr = [
            { name: 'Chrome PDF Plugin', filename: 'internal-pdf-viewer' },
            { name: 'Chrome PDF Viewer', filename: 'mhjfbmdgcfjbbpaeojofohoefgiehjai' },
            { name: 'Native Client', filename: 'internal-nacl-plugin' },
        ];
        arr.item = (i) => arr[i];
        arr.namedItem = (n) => arr.find(p => p.name === n);
        arr.refresh = () => {};
        return arr;
    }
});

// 4. Spoof languages (headless sometimes returns empty)
Object.defineProperty(navigator, 'languages', { get: () => ['en-US', 'en'] });

// 5. Override permissions query to hide "denied" for notifications
const originalQuery = window.navigator.permissions.query.bind(window.navigator.permissions);
window.navigator.permissions.query = (params) =>
    params.name === 'notifications'
        ? Promise.resolve({ state: Notification.permission })
        : originalQuery(params);

// 6. Prevent detection via iframe contentWindow
const origHTMLElement = HTMLIFrameElement.prototype.__lookupGetter__('contentWindow');
if (origHTMLElement) {
    Object.defineProperty(HTMLIFrameElement.prototype, 'contentWindow', {
        get: function() {
            const w = origHTMLElement.call(this);
            if (w && !w.chrome) w.chrome = window.chrome;
            return w;
        }
    });
}

// 7. Fix broken toString for overridden functions (anti-detection fingerprinting)
const nativeToString = Function.prototype.toString;
const overrides = new Map();
const proxy = new Proxy(nativeToString, {
    apply(target, thisArg, args) {
        const override = overrides.get(thisArg);
        return override || nativeToString.call(thisArg);
    }
});
Function.prototype.toString = proxy;
overrides.set(Function.prototype.toString, 'function toString() { [native code] }');

// 8. WebGL vendor/renderer spoof — anti-bot scripts inspect UNMASKED_VENDOR_WEBGL
// (37445) and UNMASKED_RENDERER_WEBGL (37446) to detect headless software rendering.
// Returning real GPU strings makes the browser look like a normal Windows desktop.
try {
    const getParameter = WebGLRenderingContext.prototype.getParameter;
    WebGLRenderingContext.prototype.getParameter = function(parameter) {
        if (parameter === 37445) return 'Intel Inc.';
        if (parameter === 37446) return 'Intel Iris OpenGL Engine';
        return getParameter.call(this, parameter);
    };
    if (typeof WebGL2RenderingContext !== 'undefined') {
        const getParameter2 = WebGL2RenderingContext.prototype.getParameter;
        WebGL2RenderingContext.prototype.getParameter = function(parameter) {
            if (parameter === 37445) return 'Intel Inc.';
            if (parameter === 37446) return 'Intel Iris OpenGL Engine';
            return getParameter2.call(this, parameter);
        };
    }
} catch (_) {}
"#;

/// One-shot consent banner / CMP dismissal. Runs once after page load,
/// before the SPA readiness poll, so the body innerText threshold doesn't
/// trip on banner text and the actual page content has a chance to hydrate
/// without an overlay swallowing focus. Ported subset of crawl4ai's
/// `js_snippet/remove_consent_popups.js`, restricted to the CMPs with
/// meaningful traffic share (OneTrust/CookiePro, Cookiebot, Usercentrics,
/// Sourcepoint, Quantcast, TrustArc, ConsentManager, TermsFeed) plus a
/// generic text-pattern fallback. Pierces open shadow roots and same-origin
/// iframes. Best-effort: every step is wrapped in try/catch and the snippet
/// returns the count of clicks made for telemetry only.
const CMP_DISMISS_JS: &str = r#"
(() => {
    let clicks = 0;
    const isVisible = (el) => {
        if (!el || !el.getBoundingClientRect) return false;
        const r = el.getBoundingClientRect();
        if (r.width === 0 || r.height === 0) return false;
        const s = window.getComputedStyle(el);
        return s.display !== 'none' && s.visibility !== 'hidden' && s.opacity !== '0';
    };
    const click = (el) => {
        try {
            if (!isVisible(el)) return false;
            el.click();
            clicks++;
            return true;
        } catch (_) { return false; }
    };

    // CMP-specific accept selectors, ordered by deployment share. Only the
    // first match in each (hopefully exclusive) set is clicked.
    const SELECTORS = [
        '#onetrust-accept-btn-handler',
        '.ot-accept-all',
        '#CybotCookiebotDialogBodyButtonAccept',
        '#CybotCookiebotDialogBodyLevelButtonAccept',
        '[data-testid="uc-accept-all-button"]',
        '[data-cy="uc-accept-all-button"]',
        '.sp_choice_type_11',
        'button.message-component[title*="Accept" i]',
        '.qc-cmp2-summary-buttons button[mode="primary"]',
        '#qc-cmp2-ui button[mode="primary"]',
        '#truste-consent-button',
        '.cc-btn.cc-allow',
        '.cc-btn.cc-dismiss',
        'button[data-cmp-action="accept"]',
        'button[data-accept-action="all"]',
        'button[aria-label*="Accept all" i]',
        'button[aria-label*="Allow all" i]',
        '[id*="accept-cookies" i]',
        '[class*="accept-cookies" i]:not(input):not(textarea)',
    ];

    const tryRoot = (root) => {
        for (const sel of SELECTORS) {
            try {
                const el = root.querySelector(sel);
                if (el && click(el)) return;
            } catch (_) {}
        }
        // Generic text-match fallback: scan visible buttons for accept-ish copy.
        try {
            const buttons = root.querySelectorAll('button, [role="button"], input[type="button"], input[type="submit"]');
            const PATTERNS = /^(accept all|allow all|accept cookies|i accept|agree|got it|ok|tümünü kabul et|tout accepter|alle akzeptieren|aceptar todo)$/i;
            for (const b of buttons) {
                const t = (b.innerText || b.value || b.textContent || '').trim();
                if (PATTERNS.test(t) && click(b)) return;
            }
        } catch (_) {}
    };

    // Pass 1: light DOM
    tryRoot(document);

    // Pass 2: pierce open shadow roots one level deep (most CMPs flat).
    try {
        const all = document.querySelectorAll('*');
        for (const host of all) {
            if (host.shadowRoot) tryRoot(host.shadowRoot);
        }
    } catch (_) {}

    // Pass 3: same-origin iframes (Sourcepoint mounts inside iframe).
    try {
        for (const f of document.querySelectorAll('iframe')) {
            try {
                const doc = f.contentDocument || (f.contentWindow && f.contentWindow.document);
                if (doc) tryRoot(doc);
            } catch (_) {}
        }
    } catch (_) {}

    // Pass 4: IAB TCF v2 — programmatic opt-in if API present.
    try {
        if (typeof window.__tcfapi === 'function') {
            window.__tcfapi('ping', 2, (data, ok) => {
                if (ok && data && data.cmpStatus !== 'error') {
                    try { window.__tcfapi('addEventListener', 2, () => {}); } catch (_) {}
                }
            });
        }
    } catch (_) {}

    return clicks;
})()
"#;

/// HTML snapshot expression. Fast path returns `document.documentElement
/// .outerHTML` directly. When any element exposes an open shadow root, we
/// switch to a recursive serializer that resolves `<slot>` projections into
/// the light DOM and skips shadow-scoped `<style>` (those are only
/// meaningful inside the shadow tree). Ported from crawl4ai's
/// `js_snippet/flatten_shadow_dom.js` so web-component-driven sites
/// (Shoelace, Material Web, custom-element CMSes) surface their content in
/// the markdown extractor instead of producing an empty shell.
const HTML_SNAPSHOT_JS: &str = r#"
(() => {
    const VOID = new Set([
        'area','base','br','col','embed','hr','img','input',
        'link','meta','param','source','track','wbr'
    ]);
    let hasShadow = false;
    try {
        const all = document.querySelectorAll('*');
        for (let i = 0; i < all.length; i++) {
            if (all[i].shadowRoot) { hasShadow = true; break; }
        }
    } catch (_) {}
    if (!hasShadow) return document.documentElement.outerHTML;

    const escAttr = (v) => String(v).replace(/&/g, '&amp;').replace(/"/g, '&quot;');
    const serializeAttrs = (node) => {
        let s = '';
        for (const a of node.attributes || []) {
            s += ` ${a.name}="${escAttr(a.value)}"`;
        }
        return s;
    };

    const serialize = (node) => {
        if (node.nodeType === Node.TEXT_NODE) return node.textContent;
        if (node.nodeType === Node.COMMENT_NODE) return '';
        if (node.nodeType !== Node.ELEMENT_NODE) return '';
        const tag = node.tagName.toLowerCase();
        const attrs = serializeAttrs(node);
        let inner = '';
        if (node.shadowRoot) {
            inner = serializeShadowRoot(node);
        } else {
            for (const child of node.childNodes) inner += serialize(child);
        }
        if (VOID.has(tag)) return `<${tag}${attrs}>`;
        return `<${tag}${attrs}>${inner}</${tag}>`;
    };

    const serializeShadowRoot = (host) => {
        let result = '';
        for (const child of host.shadowRoot.childNodes) {
            result += serializeShadowChild(child, host);
        }
        return result;
    };

    const serializeShadowChild = (node, host) => {
        if (node.nodeType === Node.TEXT_NODE) return node.textContent;
        if (node.nodeType === Node.COMMENT_NODE) return '';
        if (node.nodeType !== Node.ELEMENT_NODE) return '';
        const tag = node.tagName.toLowerCase();
        if (tag === 'style') return '';
        if (tag === 'slot') {
            const assigned = node.assignedNodes({ flatten: true });
            if (assigned.length > 0) {
                let out = '';
                for (const a of assigned) out += serialize(a);
                return out;
            }
            let fallback = '';
            for (const child of node.childNodes) {
                fallback += serializeShadowChild(child, host);
            }
            return fallback;
        }
        const attrs = serializeAttrs(node);
        let inner = '';
        if (node.shadowRoot) {
            inner = serializeShadowRoot(node);
        } else {
            for (const child of node.childNodes) {
                inner += serializeShadowChild(child, host);
            }
        }
        if (VOID.has(tag)) return `<${tag}${attrs}>`;
        return `<${tag}${attrs}>${inner}</${tag}>`;
    };

    return serialize(document.documentElement);
})()
"#;

/// Lightweight CDP client that talks directly to any CDP-compatible browser
/// (LightPanda, Chrome, Playwright) via WebSocket.
///
/// Uses a semaphore to limit concurrent connections to `pool_size`,
/// preventing connection storms under heavy concurrent crawl loads.
pub struct CdpRenderer {
    name: String,
    /// Base WS URL from config (e.g. "ws://chrome:9222/").
    /// For Chrome/Chromium, the actual browser WS URL includes a dynamic ID
    /// (e.g. "ws://chrome:9222/devtools/browser/<uuid>") and must be discovered
    /// at runtime via the /json/version HTTP endpoint.
    configured_ws_url: String,
    /// Lazily resolved browser-level WS URL (discovered from /json/version).
    /// Wrapped in `Mutex<Option<...>>` rather than `OnceCell` so we can
    /// invalidate on CDP connect failure: chrome restarts mint a new
    /// `/devtools/browser/<uuid>` path, and a stale cached value would dial
    /// a dead URL forever until process restart. See `invalidate_resolved_ws_url`.
    resolved_ws_url: Arc<StdMutex<Option<String>>>,
    page_timeout: Duration,
    /// Hard ceiling on the post-navigate wait+snapshot+stability+challenge
    /// phase. Wraps the work in a budget race; on hit the renderer snapshots
    /// whatever DOM is present and flags `truncated = true`.
    nav_budget: Duration,
    /// Whether to enable `Fetch.requestPaused` interception for chrome tier.
    /// When true, the pump runs alongside navigate and blocks requests per
    /// `blocklist`. Off-by-default per Phase 2 plan; flipped via config.
    intercept_enabled: bool,
    blocklist: Blocklist,
    /// Host substrings (case-insensitive) for which interception is force-disabled
    /// even when `intercept_enabled = true`.
    host_intercept_disable: Vec<String>,
    conn_semaphore: Arc<Semaphore>,
    /// Batch reserved-lane gate in front of `conn_semaphore` (the B lane, legacy
    /// `fetch_with_ws` path): batch renders take a gate permit first, interactive
    /// go straight to the pool, so interactive always finds reserved render slots.
    conn_batch_gate: crw_core::BatchGate,
    /// Browser context pool. `Some` when `[renderer.chrome.pool] enabled = true`
    /// AND backend is vanilla chrome (not browserless v2 — gated off in v1
    /// per plan §"Out of scope"). When `Some`, `fetch` dispatches through
    /// `fetch_with_pool`; when `None`, legacy `fetch_with_ws` is used.
    pool: Option<Arc<crate::browser_pool::BrowserContextPool<CdpConnection>>>,
    /// Batch reserved-lane gate in front of `pool` (the B lane, pooled
    /// `fetch_with_pool` path). Held OUTSIDE the pool so `BrowserContextPool`'s
    /// permit-as-checkout-source-of-truth invariant stays untouched: batch takes
    /// a gate permit before `pool.acquire()`, interactive skips it. `Some` iff
    /// `pool` is `Some`, sized from the pool size.
    pool_batch_gate: Option<crw_core::BatchGate>,
    /// DataImpulse base credentials (username without country suffix, password).
    /// When `Some`, the renderer drives Chrome's proxy auth via CDP
    /// `Fetch.authRequired`, composing the country-suffixed username per request.
    /// Only the chrome_proxy tier sets this; plain chrome leaves it `None`.
    proxy_auth_base: Option<(String, String)>,
    /// Country code used when a `ScrapeRequest.country` is not supplied.
    /// `None` means "no suffix" → DataImpulse global pool.
    default_country: Option<String>,
    /// Phase (latency-qn): max Cloudflare/anti-bot challenge-clear retries in the
    /// post-navigate loop. Default `CHALLENGE_MAX_RETRIES` (3 × 3s = 9s). Measured
    /// as 28% of render time, mostly on shells that never clear (→ fail anyway);
    /// neither Firecrawl nor Spider runs such a loop (they route anti-bot to a
    /// stealth/unblocker tier). Lower it to trim the tail; 0 disables the loop.
    challenge_max_retries: u32,
    /// latency-qn: post-navigate SPA-readiness poll budget (default
    /// `SPA_SELECTOR_MAX_MS` = 8s). Measured as 67% of render time — the ceiling
    /// hit when the content selector never mounts. Lower to trim the dominant
    /// wait (the p90 branch validated 3000ms holds recall within noise).
    spa_selector_max: Duration,
    /// latency-qn: event-driven earliest-ready exit. When true, the post-navigate
    /// poll exits as soon as the page is genuinely settled — body innerText ≥
    /// content-floor AND (networkAlmostIdle≤2 OR substantial text) — instead of
    /// requiring a specific content selector to mount + networkIdle(0). Keeps the
    /// mandatory content gate (never snapshots an empty CSR shell). Default off.
    fast_ready: bool,
    /// UA for `Network.setUserAgentOverride`; empty = no override (browser default).
    user_agent: String,
}

impl CdpRenderer {
    pub fn new(name: &str, ws_url: &str, page_timeout_ms: u64, pool_size: usize) -> Self {
        let pool_size = pool_size.max(1);
        let page_timeout = Duration::from_millis(page_timeout_ms);
        Self {
            name: name.to_string(),
            configured_ws_url: ws_url.to_string(),
            resolved_ws_url: Arc::new(StdMutex::new(None)),
            page_timeout,
            nav_budget: page_timeout,
            intercept_enabled: false,
            blocklist: Blocklist::defaults(),
            host_intercept_disable: Vec::new(),
            conn_semaphore: Arc::new(Semaphore::new(pool_size)),
            // Conn/no-context-pool path (per-request-proxy, self-host). Keeps the
            // default reserve; the configurable override lives only on the pool
            // gate (the path with the 12-30s render hold). `pool_size` here may be
            // the small default (4), so an absolute override must NOT reach it.
            conn_batch_gate: crw_core::BatchGate::new(
                pool_size,
                crw_core::config::resolve_interactive_reserve(None, pool_size),
                "render",
            ),
            pool: None,
            pool_batch_gate: None,
            proxy_auth_base: None,
            default_country: None,
            challenge_max_retries: CHALLENGE_MAX_RETRIES,
            spa_selector_max: Duration::from_millis(SPA_SELECTOR_MAX_MS),
            fast_ready: false,
            user_agent: String::new(),
        }
    }

    /// Enable the event-driven earliest-ready exit (latency-qn). Quality-safe:
    /// keeps a mandatory body-text content floor so it never snapshots an empty
    /// shell; only changes WHEN a ready page is detected (sooner).
    pub fn with_fast_ready(mut self, on: bool) -> Self {
        self.fast_ready = on;
        self
    }

    /// Set the User-Agent the CDP renderer presents (via
    /// `Network.setUserAgentOverride`). Pass the same `effective_ua` the HTTP
    /// fetcher uses so a JS-rendered page sees a modern UA, not the browser's
    /// default — and HTTP/CDP UAs match (a mismatch is itself a bot tell).
    pub fn with_user_agent(mut self, ua: &str) -> Self {
        self.user_agent = ua.to_string();
        self
    }

    /// Override the post-navigate challenge-clear retry count (default 3).
    /// Set lower (or 0) to trim the anti-bot tail; anti-bot recovery is then the
    /// stealth/auto-egress tier's job (the Firecrawl/Spider approach).
    pub fn with_challenge_retries(mut self, retries: u32) -> Self {
        self.challenge_max_retries = retries;
        self
    }

    /// Override the SPA-readiness poll budget (default 8s). The poll still exits
    /// early on content-ready / network-idle; this is only the ceiling when the
    /// selector never mounts. Lower to trim the dominant render wait.
    pub fn with_spa_selector_max(mut self, ms: u64) -> Self {
        self.spa_selector_max = Duration::from_millis(ms);
        self
    }

    /// Configure DataImpulse base proxy credentials. The `Fetch.authRequired`
    /// pump composes the per-request username as `{base_user}__cr.{country}`,
    /// resolved from `RequestContext::country` (set via `REQUEST_COUNTRY` task-local)
    /// with `default_country` as the fallback when the request omits it.
    pub fn with_proxy_auth_base(
        mut self,
        base_user: String,
        base_pass: String,
        default_country: Option<String>,
    ) -> Self {
        self.proxy_auth_base = Some((base_user, base_pass));
        self.default_country = default_country;
        self
    }

    /// Enable the browser-context pool. Builds an `Arc<BrowserContextPool>`
    /// whose factory calls back into the same connect-with-retry path as the
    /// legacy `fetch_with_ws` (preserves the cached-WS-URL invalidation from
    /// commit `b5f7bec`).
    pub fn with_pool(mut self, cfg: crate::browser_pool::PoolCfg) -> Self {
        let name = self.name.clone();
        let configured = self.configured_ws_url.clone();
        let resolved_cache = self.resolved_ws_url.clone();
        let page_timeout = self.page_timeout;
        let factory: crate::browser_pool::ConnFactory<CdpConnection> = Arc::new(move || {
            let name = name.clone();
            let configured = configured.clone();
            let resolved_cache = resolved_cache.clone();
            Box::pin(async move {
                let conn =
                    connect_chrome_with_retry(&name, &configured, &resolved_cache, page_timeout)
                        .await?;
                Ok(Arc::new(conn))
            })
        });
        crw_core::metrics::metrics()
            .chrome_pool_size
            .set(cfg.size as i64);
        self.pool_batch_gate = Some(crw_core::BatchGate::new(
            cfg.size,
            crw_core::config::resolve_interactive_reserve(
                cfg.reserved_interactive_renders,
                cfg.size,
            ),
            "render",
        ));
        self.pool = Some(crate::browser_pool::BrowserContextPool::new(cfg, factory));
        self
    }

    pub fn pool(&self) -> Option<Arc<crate::browser_pool::BrowserContextPool<CdpConnection>>> {
        self.pool.clone()
    }

    /// Override the post-navigate budget. Default equals `page_timeout_ms`.
    /// Set from `RendererConfig::chrome_nav_budget_ms` for the chrome tier.
    pub fn with_nav_budget(mut self, nav_budget_ms: u64) -> Self {
        self.nav_budget = Duration::from_millis(nav_budget_ms);
        self
    }

    /// Enable `Fetch.requestPaused` interception driven by `blocklist`.
    /// `host_disable` is a list of host substrings that opt out per-request.
    pub fn with_interception(
        mut self,
        enabled: bool,
        blocklist: Blocklist,
        host_disable: Vec<String>,
    ) -> Self {
        self.intercept_enabled = enabled;
        self.blocklist = blocklist;
        self.host_intercept_disable = host_disable.iter().map(|s| s.to_lowercase()).collect();
        self
    }

    /// `true` if interception is configured-on AND the URL's host is not on
    /// the per-host opt-out list.
    fn intercept_active_for(&self, url: &str) -> bool {
        if !self.intercept_enabled {
            return false;
        }
        if self.host_intercept_disable.is_empty() {
            return true;
        }
        let host = match url::Url::parse(url) {
            Ok(u) => u.host_str().map(|s| s.to_lowercase()).unwrap_or_default(),
            Err(_) => return true,
        };
        !self.host_intercept_disable.iter().any(|h| host.contains(h))
    }
}

/// Resolve the actual browser WebSocket URL, caching the result. Free
/// function so both `CdpRenderer::resolve_ws_url` and the pool's connect
/// factory share a single implementation with shared cache invalidation
/// semantics.
async fn resolve_ws_url_with_cache(
    configured: &str,
    cache: &StdMutex<Option<String>>,
    _page_timeout: Duration,
) -> CrwResult<String> {
    if let Some(cached) = cache.lock().unwrap().clone() {
        return Ok(cached);
    }

    let resolved = if configured.contains("/devtools/") || is_browserless_direct_ws(configured) {
        // Already-resolved /devtools/ URL OR browserless v2 / commercial CDP
        // endpoint that serves a WS directly (no /json/version).
        configured.to_string()
    } else if let Ok(Ok((ws, _))) =
        tokio::time::timeout(Duration::from_secs(3), connect_async(configured)).await
    {
        drop(ws);
        configured.to_string()
    } else {
        let http_url = configured
            .replace("ws://", "http://")
            .replace("wss://", "https://")
            .trim_end_matches('/')
            .to_string()
            + "/json/version";

        tracing::info!("Discovering browser WS URL from {http_url}");

        let resp = reqwest::Client::new()
            .get(&http_url)
            .header("Host", "localhost")
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| {
                // The endpoint is already in the `Discovering browser WS URL` line
                // above; it is internal infrastructure and must not reach the caller.
                CrwError::RendererError(format!(
                    "CDP discovery failed: {}",
                    crw_core::error::reqwest_message(e)
                ))
            })?;

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            CrwError::RendererError(format!(
                "CDP discovery parse error: {}",
                crw_core::error::reqwest_message(e)
            ))
        })?;

        let ws_url = body
            .get("webSocketDebuggerUrl")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                CrwError::RendererError("No webSocketDebuggerUrl in /json/version".into())
            })?;

        let rewritten = rewrite_ws_host(ws_url, configured);
        tracing::info!(ws_url = %rewritten, "Discovered browser WS URL");
        rewritten
    };

    *cache.lock().unwrap() = Some(resolved.clone());
    Ok(resolved)
}

impl CdpRenderer {
    /// Resolve the WS URL and open a CDP connection. On failure, invalidate
    /// the cached URL and retry once — covers the chrome-restart case where
    /// the cached `/devtools/browser/<uuid>` is stale.
    async fn connect_with_retry(&self) -> CrwResult<CdpConnection> {
        connect_chrome_with_retry(
            &self.name,
            &self.configured_ws_url,
            &self.resolved_ws_url,
            self.page_timeout,
        )
        .await
    }
}

/// Connect to a Chrome CDP endpoint with cached-WS-URL invalidation on first
/// failure. Shared between `CdpRenderer::connect_with_retry` (legacy path)
/// and `BrowserContextPool`'s factory (preserves the b5f7bec invalidation
/// guarantee — chrome restarts mint a fresh `/devtools/browser/<uuid>`,
/// and the cache must be drop-and-rebuild on connect failure).
async fn connect_chrome_with_retry(
    name: &str,
    configured_ws_url: &str,
    resolved_cache: &StdMutex<Option<String>>,
    page_timeout: Duration,
) -> CrwResult<CdpConnection> {
    let t0 = Instant::now();
    let result =
        connect_chrome_with_retry_inner(name, configured_ws_url, resolved_cache, page_timeout)
            .await;
    let outcome = classify_connect_outcome(&result);
    crw_core::metrics::metrics()
        .chrome_connect_seconds
        .with_label_values(&[outcome])
        .observe(t0.elapsed().as_secs_f64());
    result
}

/// Strip "Mozilla/5.0 " so lightpanda accepts the UA — its `validateUserAgent`
/// rejects any "Mozilla" (`error.Reserved`). The "...Chrome/<ver>..." token survives.
fn lightpanda_safe_ua(ua: &str) -> &str {
    ua.strip_prefix("Mozilla/5.0 ").unwrap_or(ua)
}

/// Split a caller-supplied header map into the UA override value and the rest.
///
/// On CDP a User-Agent is set via `Network.setUserAgentOverride`, not through
/// `Network.setExtraHTTPHeaders`, so a `User-Agent` header (matched
/// case-insensitively) is pulled out and returned separately; every other
/// header becomes the extra-headers payload.
///
/// A blank `User-Agent` is treated as absent: it neither overrides the tier
/// default nor lands in the extra headers, so the render keeps sending the
/// tier's modern UA rather than falling back to the browser's own (stale) one.
fn split_caller_headers(
    headers: &HashMap<String, String>,
) -> (Option<String>, serde_json::Map<String, serde_json::Value>) {
    let mut ua = None;
    let mut extra = serde_json::Map::new();
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("user-agent") {
            if !v.trim().is_empty() {
                ua = Some(v.clone());
            }
        } else {
            extra.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
    }
    (ua, extra)
}

/// The exact `navigator.languages` array literal inside [`STEALTH_JS`]. When a
/// request pins an exit country we swap this for the country's language list,
/// so the spoofed navigator agrees with the `Accept-Language` we send. A unit
/// test asserts the literal is still present, so editing the stealth script
/// cannot silently turn the swap into a no-op.
const STEALTH_DEFAULT_LANGUAGES: &str = "['en-US', 'en']";

/// The `Accept-Language` to advertise on the CDP path for the pinned exit
/// country, or `None` to leave the browser default alone.
///
/// A caller-supplied `Accept-Language` wins: it already reaches the page
/// through `Network.setExtraHTTPHeaders`, so adding a derived value on top
/// would only contradict it.
fn language_locale(
    geo_locale: Option<crate::locale::Locale>,
    extra_headers: &serde_json::Map<String, serde_json::Value>,
) -> Option<crate::locale::Locale> {
    let locale = geo_locale?;
    if extra_headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("accept-language"))
    {
        return None;
    }
    Some(locale)
}

/// The `acceptLanguage` for `Network.setUserAgentOverride`, in the plain tag
/// form Chromium expects (it adds the quality weights itself).
#[cfg(test)]
fn geo_accept_language(
    geo_locale: Option<crate::locale::Locale>,
    extra_headers: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    language_locale(geo_locale, extra_headers).map(|l| l.cdp_accept_language())
}

/// Payload for `Network.setUserAgentOverride`. Without a country-derived
/// language the payload is exactly what it has always been, so the default
/// render sends byte-identical CDP traffic.
fn ua_override_params(user_agent: &str, accept_language: Option<&str>) -> serde_json::Value {
    let mut params = serde_json::json!({ "userAgent": user_agent });
    if let Some(al) = accept_language {
        params["acceptLanguage"] = serde_json::Value::String(al.to_string());
    }
    params
}

async fn connect_chrome_with_retry_inner(
    name: &str,
    configured_ws_url: &str,
    resolved_cache: &StdMutex<Option<String>>,
    page_timeout: Duration,
) -> CrwResult<CdpConnection> {
    let ws_url = resolve_ws_url_with_cache(configured_ws_url, resolved_cache, page_timeout).await?;
    match CdpConnection::connect(&ws_url, WS_CONNECT_TIMEOUT).await {
        Ok(conn) => Ok(conn),
        Err(e) => {
            tracing::warn!(
                renderer = name,
                error = %e,
                "CDP connect failed; invalidating cached ws_url and retrying once"
            );
            *resolved_cache.lock().unwrap() = None;
            let ws_url =
                resolve_ws_url_with_cache(configured_ws_url, resolved_cache, page_timeout).await?;
            CdpConnection::connect(&ws_url, WS_CONNECT_TIMEOUT).await
        }
    }
}

/// Bucket a `connect_with_retry` result into one of the Tier 0 outcome labels:
/// `ok`, `ws_handshake_timeout`, `version_probe_fail`, `ws_dial_fail`.
fn classify_connect_outcome(r: &CrwResult<CdpConnection>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(CrwError::Timeout(_)) => "ws_handshake_timeout",
        Err(CrwError::RendererError(msg)) if msg.contains("CDP discovery") => "version_probe_fail",
        Err(_) => "ws_dial_fail",
    }
}

/// Recognise commercial / browserless-style CDP endpoints that serve a
/// WebSocket directly and don't expose `/json/version`. Such URLs
/// either carry a `token=` query parameter or use a browser-named path
/// (`/chromium`, `/firefox`, `/webkit`).
fn is_browserless_direct_ws(url: &str) -> bool {
    if url.contains("token=") {
        return true;
    }
    url.contains("/chromium") || url.contains("/firefox") || url.contains("/webkit")
}

/// Rewrite the host:port of a WS URL to match the configured endpoint.
/// Chrome's /json/version returns "ws://127.0.0.1:9222/devtools/browser/..." but
/// from another container we need "ws://chrome:9222/devtools/browser/...".
fn rewrite_ws_host(discovered: &str, configured: &str) -> String {
    let conf_stripped = configured
        .trim_start_matches("ws://")
        .trim_start_matches("wss://");
    let conf_host_port = conf_stripped.split('/').next().unwrap_or(conf_stripped);

    let disc_stripped = discovered
        .trim_start_matches("ws://")
        .trim_start_matches("wss://");
    let disc_path = disc_stripped
        .find('/')
        .map(|i| &disc_stripped[i..])
        .unwrap_or("/");

    let scheme = if configured.starts_with("wss://") {
        "wss://"
    } else {
        "ws://"
    };
    format!("{scheme}{conf_host_port}{disc_path}")
}

/// Build the `Fetch.continueWithAuth` payload. Pure fn — testable without a
/// live CDP connection. When `creds` is `Some`, replies with
/// `ProvideCredentials`; when `None`, replies with `CancelAuth` (NOT `Default`,
/// which is ambiguous in headless and may pop a dialog in headed mode).
fn build_auth_response(request_id: &str, creds: Option<(&str, &str)>) -> serde_json::Value {
    match creds {
        Some((user, pass)) => serde_json::json!({
            "requestId": request_id,
            "authChallengeResponse": {
                "response": "ProvideCredentials",
                "username": user,
                "password": pass,
            },
        }),
        None => serde_json::json!({
            "requestId": request_id,
            "authChallengeResponse": { "response": "CancelAuth" },
        }),
    }
}

/// Drive `Fetch.authRequired` events to supply DataImpulse credentials per
/// request. Mirrors `run_intercept_pump`'s shape (borrow `&CdpConnection`,
/// filter by `session_id`, exit on `Closed`).
///
/// `creds` is composed *per `fetch_inner` call* with the request's country
/// suffix already applied, captured by move into this future so concurrent
/// pool slots cannot cross-contaminate credentials.
///
/// This pump answers `Fetch.authRequired` only. `Fetch.requestPaused` is owned
/// by [`run_intercept_pump`], which now runs on every navigation, so answering
/// paused requests here too would double-continue them.
async fn run_auth_pump(
    conn: &CdpConnection,
    mut rx: broadcast::Receiver<CdpEvent>,
    creds: Option<(String, String)>,
    session_id: &str,
) {
    let cmd_timeout = Duration::from_secs(2);
    loop {
        let ev = match rx.recv().await {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if ev.session_id.as_deref() != Some(session_id) {
            continue;
        }
        if ev.method != "Fetch.authRequired" {
            continue;
        }
        let request_id = ev
            .params
            .get("requestId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if request_id.is_empty() {
            continue;
        }
        let creds_ref = creds.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
        let payload = build_auth_response(&request_id, creds_ref);
        let _ = conn
            .send_recv(
                "Fetch.continueWithAuth",
                payload,
                Some(session_id),
                cmd_timeout,
            )
            .await;
    }
}

/// Why the browser may not make this request, or `None` to allow it.
///
/// The route layer validates only the URL the caller supplied. Everything the
/// page does afterwards — server redirects, `<meta refresh>`, JS navigation,
/// same-process iframes, XHR and `fetch` — is issued by the browser's own
/// network stack, so an interception handler is the only point where those
/// destinations can be checked. This is the CDP equivalent of what
/// [`crw_core::url_safety::safe_redirect_policy`] already does for the
/// HTTP-only tier.
async fn outbound_block_label(req_url: &str, ctx: &OutboundCtx) -> Option<&'static str> {
    let Ok(parsed) = url::Url::parse(req_url) else {
        // Chrome sends absolute, normalised URLs; anything we cannot parse is
        // anomalous, so fail closed.
        return Some(BLOCK_LABEL_POLICY);
    };
    // Schemes that never reach a socket. Failing them would break ordinary
    // pages for no gain. An allowlist, not "anything that is not http(s)":
    // `validate_safe_host` deliberately dropped the scheme rule, so this is the
    // only scheme gate left on this path.
    if matches!(parsed.scheme(), "data" | "blob" | "about") {
        return None;
    }
    // Cheap first: literal addresses and the hostname deny list, no DNS.
    //
    // `validate_safe_host`, not `validate_safe_url`: the URL-shape rules
    // (2048-char cap, scheme) belong at the route layer, where the caller chose
    // the URL. A signed CDN subresource past the cap is not an SSRF, and
    // failing it here would silently drop a legitimate request.
    if !matches!(parsed.scheme(), "http" | "https")
        || crw_core::url_safety::validate_safe_host(&parsed).is_err()
    {
        return Some(BLOCK_LABEL_POLICY);
    }
    let Some(port) = parsed.port_or_known_default() else {
        return Some(BLOCK_LABEL_POLICY);
    };
    let key = format!("{}:{port}", parsed.host_str().unwrap_or_default());
    // Per-render memo, not a process-wide cache with a TTL. One page emits many
    // requests to a handful of hosts (a github.com navigation pauses 14 requests
    // across 2 hosts), and re-resolving each one queues behind `RESOLVE_LIMIT`
    // for no benefit — the browser is reusing its own cached address anyway. The
    // rebinding window this opens is one page load, not a cache lifetime, which
    // is why a shared TTL cache was rejected.
    //
    // Only allow verdicts are memoised. A denial can come from a transient
    // resolver failure (SERVFAIL under load is the common failure mode, not a
    // stall), and latching that would block a legitimate host for the rest of
    // the render with no retry.
    if ctx.memo.lock().ok().and_then(|m| m.get(&key).copied()) == Some(true) {
        return None;
    }
    // Bounded end to end. `RESOLVE_LIMIT` is process-wide and FIFO-fair, so an
    // unbounded wait here would be a hang one layer below the pump: the handler
    // never answers its paused request and the page stalls to the nav budget.
    // Fails closed on expiry, which costs one subresource rather than the page.
    //
    // The budget is the tier's own navigation timeout, not a fixed constant. A
    // verdict that arrives after `Page.navigate` has already timed out cannot
    // help anyone: on the LightPanda tier that ceiling is 2.5s, so a longer
    // budget only converts renders that would have succeeded into failures.
    let classified = tokio::time::timeout(ctx.budget, async {
        // Two gates: a per-render one so a single page with hundreds of unique
        // hosts cannot occupy the whole process-wide pool and force fail-closed
        // denials in every other concurrent render, and the global one that
        // bounds total resolver load.
        let _render_permit = ctx.render_limit.acquire().await;
        let _permit = RESOLVE_LIMIT.acquire().await;
        crw_core::url_safety::classify_safe_host_resolved(&parsed).await
    })
    .await;
    match classified {
        Ok(Ok(())) => {
            if let Ok(mut m) = ctx.memo.lock() {
                m.insert(key, true);
            }
            None
        }
        // A destination that resolved into a blocked range is a policy call and
        // stays labelled as one. Reporting it as "we could not check" would hide
        // the thing this guard exists to surface — a rebinding attempt is public
        // at the route layer and internal by the time the browser asks — and
        // would route it into the caller-refund path below.
        Ok(Err(crw_core::url_safety::HostRejection::Policy(_))) => Some(BLOCK_LABEL_POLICY),
        // We could not establish where this points: resolver error, or the check
        // ran out of budget. That is our failure, not the caller's, so record it
        // and let the caller-facing error say so rather than blaming the origin:
        // both tiers share one resolver, so a brown-out otherwise reads as "the
        // target is unreachable", gets a 422 and a refund, and never trips the
        // 5xx watchdog.
        //
        // Scoped to the navigated origin. A parked ad or analytics host that no
        // longer resolves is common, and letting it relabel the render would
        // deny a refund for an origin the caller really could not reach.
        Ok(Err(crw_core::url_safety::HostRejection::Unresolved(_))) | Err(_) => {
            if parsed.host_str().is_some_and(|h| h == ctx.doc_host) {
                ctx.unresolved
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            Some(BLOCK_LABEL_UNRESOLVED)
        }
    }
}

/// Per-render state the destination check needs.
struct OutboundCtx {
    memo: ResolveMemo,
    /// One render's share of [`RESOLVE_LIMIT`].
    render_limit: tokio::sync::Semaphore,
    /// Ceiling on one check, taken from the tier's navigation timeout.
    budget: Duration,
    /// Host of the URL being navigated. Only this host's failures may relabel
    /// the render as our own failure.
    doc_host: String,
    /// Set when a check failed because our own resolver could not answer.
    unresolved: std::sync::atomic::AtomicBool,
}

/// Host verdicts already decided during one render. See [`outbound_allowed`].
type ResolveMemo = Arc<StdMutex<HashMap<String, bool>>>;

/// Paused requests the pump has not answered yet, as `(session id, request id)`.
type Outstanding = Arc<StdMutex<std::collections::HashSet<(String, String)>>>;

/// The destination is inside a range or on a host we refuse to reach.
const BLOCK_LABEL_POLICY: &str = "outbound";
/// The destination could not be proved safe: resolver error or budget expiry.
const BLOCK_LABEL_UNRESOLVED: &str = "outbound_unresolved";

/// One render's share of [`RESOLVE_LIMIT`]. Small enough that a page with
/// hundreds of unique hosts cannot park the whole global pool, large enough
/// that an ordinary page's handful of distinct hosts never queues.
const PER_RENDER_RESOLVE_LIMIT: usize = 8;

/// Process-wide ceiling on concurrent destination lookups.
///
/// Every render on every tier now pauses every request, so without a global
/// bound a few hundred concurrent scrapes would put an unbounded number of
/// `getaddrinfo` calls on the shared blocking pool, which HTML extraction and
/// PDF parsing also use. Bounding here rather than per pump keeps the receiver
/// loop free to keep draining CDP events: a pump that stops reading its
/// broadcast channel starts dropping `Fetch.requestPaused` events, and a
/// dropped one is never answered.
static RESOLVE_LIMIT: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(256));

/// Answer every `Fetch.requestPaused` event: validate the destination, apply
/// the ad/resource blocklist when one is configured, then continue or fail
/// (`BlockedByClient`). Runs forever until cancelled (the future is dropped
/// when the work future completes inside `tokio::select!`).
///
/// `blocklist` is `None` when resource interception is configured off for this
/// URL; the destination check still runs, because it is what keeps the browser
/// from being driven into the operator's own network.
///
/// Concurrency: handlers run in a bounded `FuturesUnordered` rather than inline.
/// The destination check can cost a DNS lookup, and the event receiver must not
/// block on it: the broadcast ring is shared with every other CDP event, and a
/// `Lagged` drop of a `Fetch.requestPaused` is unrecoverable — that request is
/// never answered and hangs until the navigation budget expires. Saturation
/// applies backpressure instead of failing requests; answering our own
/// saturation with `failRequest` would drop real subresources and surface as a
/// badly rendered page rather than as an error.
async fn run_intercept_pump(
    conn: &CdpConnection,
    mut rx: broadcast::Receiver<CdpEvent>,
    blocklist: Option<&Blocklist>,
    session_id: &str,
    ctx: &OutboundCtx,
    outstanding: &Outstanding,
) {
    use futures::future::{BoxFuture, FutureExt};
    use futures::stream::{FuturesUnordered, StreamExt};

    let cmd_timeout = Duration::from_secs(2);
    // Boxed because two differently-shaped handlers share the set: enabling
    // interception on a newly attached child target, and answering a paused
    // request.
    let mut in_flight: FuturesUnordered<BoxFuture<'_, ()>> = FuturesUnordered::new();
    // Child sessions auto-attached under our page target (out-of-process
    // iframes, workers). Their requests carry their own session id, so they
    // have to be recognised as ours, and only ours: answering a session we did
    // not attach risks double-continuing a request another consumer owns.
    let mut child_sessions: std::collections::HashSet<String> = std::collections::HashSet::new();

    loop {
        // No hard in-flight cap: a cap that makes this loop stop calling
        // `rx.recv()` is how `Fetch.requestPaused` events get dropped, and a
        // dropped one hangs its request until the navigation budget expires.
        // Resolver load is bounded globally by `RESOLVE_LIMIT` instead.
        //
        // Deliberately not `biased`: preferring the receiver would let a busy
        // event stream starve the handlers, and a handler that never runs is a
        // request that is never answered. Fair polling keeps both moving. An
        // empty set yields `None`, which disables that branch.
        let received = tokio::select! {
            ev = rx.recv() => ev,
            Some(()) = in_flight.next() => continue,
        };
        let ev = match received {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Lagged(dropped)) => {
                // Neither loss is recoverable here. A missed
                // `Fetch.requestPaused` leaves that request paused until the
                // target closes, which is fail-closed but costs the subresource;
                // a missed `Target.attachedToTarget` leaves that child paused for
                // the rest of the render, because nothing else sends
                // `Runtime.runIfWaitingForDebugger`. Make it visible rather than
                // silent.
                tracing::warn!(
                    dropped,
                    "CDP event backlog overflowed; a paused request or child target may stall"
                );
                crw_core::metrics::metrics()
                    .chrome_blocked_requests_total
                    .with_label_values(&["event_lag"])
                    .inc();
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        // A child target attached under our page: enable interception on it too,
        // otherwise its requests never pause. This is what covers an
        // out-of-process iframe, whose navigation belongs to its own target and
        // would otherwise render into a screenshot unchecked. Browser-scope
        // targets (service workers) still do not attach to a page session and
        // remain outside this; on LightPanda they are covered by
        // `--block-private-networks` instead.
        let attach_parent_is_ours = ev
            .session_id
            .as_deref()
            .is_some_and(|s| s == session_id || child_sessions.contains(s));
        if ev.method == "Target.attachedToTarget" && attach_parent_is_ours {
            if let Some(child) = ev.params.get("sessionId").and_then(|v| v.as_str()) {
                child_sessions.insert(child.to_string());
                let child = child.to_string();
                let child_target = ev
                    .params
                    .get("targetInfo")
                    .and_then(|t| t.get("targetId"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                in_flight.push(
                    async move {
                        // The child is attached paused, so its FIRST request —
                        // the one that matters, an out-of-process iframe's own
                        // navigation — cannot outrun this. It is resumed only
                        // once interception is on; if interception could not be
                        // turned on, the target is closed instead of resumed,
                        // because resuming it would run an unvalidated frame and
                        // leaving it paused would stall the render.
                        let enabled = conn
                            .send_recv(
                                "Fetch.enable",
                                serde_json::json!({ "patterns": [{ "urlPattern": "*" }] }),
                                Some(&child),
                                cmd_timeout,
                            )
                            .await
                            .is_ok();
                        if !enabled {
                            crw_core::metrics::metrics()
                                .chrome_blocked_requests_total
                                .with_label_values(&["child_unguarded"])
                                .inc();
                            tracing::warn!(
                                target_id = %child_target,
                                "could not intercept a child target; closing it"
                            );
                            let closed = !child_target.is_empty()
                                && conn
                                    .send_recv(
                                        "Target.closeTarget",
                                        serde_json::json!({ "targetId": child_target }),
                                        None,
                                        cmd_timeout,
                                    )
                                    .await
                                    .is_ok();
                            if !closed {
                                // Deliberately still not resumed: that would run
                                // an unchecked target. The child stays frozen,
                                // which costs this frame and at worst burns the
                                // nav budget. Counted so the trade-off is visible
                                // rather than silent.
                                crw_core::metrics::metrics()
                                    .chrome_blocked_requests_total
                                    .with_label_values(&["child_stuck"])
                                    .inc();
                            }
                            return;
                        }
                        // Auto-attach does not cascade, so ask the child to
                        // attach ITS children too; otherwise an iframe nested
                        // inside an out-of-process iframe never surfaces.
                        let _ = conn
                            .send_recv(
                                "Target.setAutoAttach",
                                serde_json::json!({
                                    "autoAttach": true,
                                    "waitForDebuggerOnStart": true,
                                    "flatten": true,
                                }),
                                Some(&child),
                                cmd_timeout,
                            )
                            .await;
                        let _ = conn
                            .send_recv(
                                "Runtime.runIfWaitingForDebugger",
                                serde_json::json!({}),
                                Some(&child),
                                cmd_timeout,
                            )
                            .await;
                    }
                    .boxed(),
                );
            }
            continue;
        }
        if ev.method != "Fetch.requestPaused" {
            continue;
        }
        let event_session = ev.session_id.clone().unwrap_or_default();
        if event_session != session_id && !child_sessions.contains(&event_session) {
            continue;
        }
        let request_id = ev
            .params
            .get("requestId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if request_id.is_empty() {
            continue;
        }
        let resource_type = ev
            .params
            .get("resourceType")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let req_url = ev
            .params
            .get("request")
            .and_then(|r| r.get("url"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Every paused request is recorded before a handler takes it, and
        // removed once answered. Teardown fails whatever is left: dropping a
        // handler mid-check would otherwise leave the request paused, and
        // `Fetch.disable` auto-continues paused requests, turning an unfinished
        // check into an allow.
        if let Ok(mut o) = outstanding.lock() {
            o.insert((event_session.clone(), request_id.clone()));
        }

        // Blocklist first: a blocked request never leaves the browser, so there
        // is no destination to validate and no lookup to pay for.
        let blocked_by_list = blocklist
            .and_then(|list| list.should_block(resource_type, &req_url))
            .map(|reason| match reason {
                BlockReason::ResourceType => "resource_type",
                BlockReason::Host => "host",
            });

        in_flight.push(
            async move {
                let block_label = match blocked_by_list {
                    Some(label) => Some(label),
                    None => outbound_block_label(&req_url, ctx).await,
                };
                let (method, params) = match block_label {
                    Some(label) => {
                        crw_core::metrics::metrics()
                            .chrome_blocked_requests_total
                            .with_label_values(&[label])
                            .inc();
                        (
                            "Fetch.failRequest",
                            serde_json::json!({
                                "requestId": request_id,
                                "errorReason": "BlockedByClient",
                            }),
                        )
                    }
                    None => (
                        "Fetch.continueRequest",
                        serde_json::json!({ "requestId": request_id }),
                    ),
                };
                // Answer on the session the event arrived on, which for a child
                // target is not ours.
                // Forget it only once the browser actually took the answer. A
                // `failRequest` that timed out or hit a closed socket leaves the
                // request paused, and dropping the record here would hand it to
                // `Fetch.disable`, which continues paused requests: the same
                // fail-open one layer down. Teardown re-answering an already
                // answered id is harmless, CDP just reports an unknown
                // interception id.
                let answered = conn
                    .send_recv(method, params, Some(&event_session), cmd_timeout)
                    .await
                    .is_ok();
                if answered && let Ok(mut o) = outstanding.lock() {
                    o.remove(&(event_session, request_id));
                }
            }
            .boxed(),
        );
    }
}

/// Capture XHR/fetch JSON responses for fallback extraction.
///
/// Subscribes to CDP events and, for every `Network.responseReceived` whose
/// MIME and status look like API content, calls `Network.getResponseBody` and
/// appends the result. Bounded by `NET_CAPTURE_MAX_BODIES` and
/// `NET_CAPTURE_MAX_TOTAL_BYTES` so a chatty page can't blow memory.
///
/// Never returns under normal operation; the broadcast `Closed` arm exits.
/// Designed to run in `tokio::select!` alongside the main work future.
async fn run_network_capture_pump(
    conn: &CdpConnection,
    mut rx: broadcast::Receiver<CdpEvent>,
    sink: Arc<Mutex<Vec<CapturedNetworkResponse>>>,
    session_id: &str,
) {
    let mut total_bytes = 0usize;
    loop {
        let ev = match rx.recv().await {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if ev.method != "Network.responseReceived" {
            continue;
        }
        if ev.session_id.as_deref() != Some(session_id) {
            continue;
        }
        // Skip the main document — already in `html` field.
        let resource_type = ev.params.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if !matches!(resource_type, "XHR" | "Fetch") {
            continue;
        }
        let response = match ev.params.get("response") {
            Some(v) => v,
            None => continue,
        };
        let status = response
            .get("status")
            .and_then(|s| s.as_f64())
            .map(|s| s as u16)
            .unwrap_or(0);
        if !(200..300).contains(&status) {
            continue;
        }
        let mime = response
            .get("mimeType")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !is_capturable_mime(mime) {
            continue;
        }
        // Drop tiny payloads early using Content-Length when available.
        let advertised_len = response
            .get("headers")
            .and_then(|h| h.get("Content-Length").or_else(|| h.get("content-length")))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<usize>().ok());
        if let Some(len) = advertised_len
            && len < NET_CAPTURE_MIN_BODY_SIZE
        {
            continue;
        }
        // Caps before issuing the round-trip.
        {
            let cur = sink.lock().await;
            if cur.len() >= NET_CAPTURE_MAX_BODIES {
                continue;
            }
        }
        if total_bytes >= NET_CAPTURE_MAX_TOTAL_BYTES {
            continue;
        }
        let request_id = ev
            .params
            .get("requestId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if request_id.is_empty() {
            continue;
        }
        let url = response
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let body_resp = match conn
            .send_recv(
                "Network.getResponseBody",
                serde_json::json!({ "requestId": request_id }),
                Some(session_id),
                NET_CAPTURE_GETBODY_TIMEOUT,
            )
            .await
        {
            Ok(v) => v,
            Err(_) => continue,
        };
        let body = body_resp.get("body").and_then(|v| v.as_str()).unwrap_or("");
        let base64 = body_resp
            .get("base64Encoded")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if base64 || body.len() < NET_CAPTURE_MIN_BODY_SIZE {
            continue;
        }
        let captured = CapturedNetworkResponse {
            url,
            request_id,
            status,
            mime_type: Some(mime.to_string()),
            body_size_bytes: body.len(),
            body: Some(body.to_string()),
        };
        total_bytes += captured.body_size_bytes;
        sink.lock().await.push(captured);
    }
}

/// Cheap tracker for in-flight network requests. Updated from CDP
/// `Network.requestWillBeSent` / `loadingFinished` / `loadingFailed` events.
/// Used by the SPA poll as an alternate "page settled" signal.
#[derive(Debug)]
struct NetworkActivityTracker {
    /// Net in-flight count. Saturated at 0 in `is_idle` because event ordering
    /// can briefly drive the counter negative (a `loadingFinished` for a
    /// request whose `requestWillBeSent` was missed during pump startup).
    in_flight: AtomicI64,
    /// Wall-clock ms of the last request start/end. Used to gate idle on a
    /// quiet-period — `in_flight == 0` alone fires too early on SPAs that
    /// haven't kicked off their first XHR yet.
    last_change_ms: AtomicI64,
}

impl NetworkActivityTracker {
    fn new() -> Self {
        Self {
            in_flight: AtomicI64::new(0),
            last_change_ms: AtomicI64::new(now_unix_ms()),
        }
    }

    fn record_request_start(&self) {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        self.last_change_ms.store(now_unix_ms(), Ordering::Relaxed);
    }

    fn record_request_end(&self) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        self.last_change_ms.store(now_unix_ms(), Ordering::Relaxed);
    }

    /// Page is "network-idle" once the in-flight count hit zero and stayed
    /// there for at least `quiet_ms`.
    fn is_idle(&self, quiet_ms: i64) -> bool {
        if self.in_flight.load(Ordering::Relaxed) > 0 {
            return false;
        }
        let elapsed = now_unix_ms() - self.last_change_ms.load(Ordering::Relaxed);
        elapsed >= quiet_ms
    }

    /// networkAlmostIdle: ≤ `max_inflight` in-flight requests sustained for
    /// `quiet_ms`. `is_idle` is this with max_inflight=0 (networkIdle). The
    /// quiet-period guard means a request that JUST started keeps us non-settled,
    /// so a freshly-navigated SPA isn't declared settled before its XHRs fire.
    fn is_settled(&self, quiet_ms: i64, max_inflight: i64) -> bool {
        if self.in_flight.load(Ordering::Relaxed) > max_inflight {
            return false;
        }
        let elapsed = now_unix_ms() - self.last_change_ms.load(Ordering::Relaxed);
        elapsed >= quiet_ms
    }
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Drains CDP events to maintain `tracker`'s in-flight counter. Long-lived
/// like the other pumps; exits when the broadcast closes.
async fn run_network_idle_pump(
    mut rx: broadcast::Receiver<CdpEvent>,
    tracker: Arc<NetworkActivityTracker>,
    session_id: &str,
) {
    // Count only MAIN-FRAME requests toward in-flight. A chatty ad/analytics
    // iframe holding ≥3 persistent requests otherwise keeps the global counter
    // above the networkAlmostIdle threshold forever, burning the full ceiling.
    // NOTE: a tighter FIRST-PARTY (eTLD+1) filter was tried to also ignore
    // in-main-frame third-party tracker/widget chatter (columbusjack-class
    // ceiling burns) but REVERTED — it truncated pages whose content loads from a
    // third-party CDN/API (bench: +2 failures, content drops every run). Like
    // DOM-stable/DCL, exiting before ALL the page's requests settle truncates
    // late-arriving content. `loadingFinished/Failed` carry only requestId, so
    // track which requestIds belong to the main frame.
    let mut main_frame: Option<String> = None;
    let mut main_reqs: std::collections::HashSet<String> = std::collections::HashSet::new();
    loop {
        let ev = match rx.recv().await {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if ev.session_id.as_deref() != Some(session_id) {
            continue;
        }
        match ev.method.as_str() {
            "Network.requestWillBeSent" => {
                let fid = ev.params.get("frameId").and_then(|v| v.as_str());
                // Adopt the first top-level Document request's frame as the main frame.
                if main_frame.is_none()
                    && ev.params.get("type").and_then(|v| v.as_str()) == Some("Document")
                {
                    main_frame = fid.map(str::to_string);
                }
                if let (Some(rid), Some(mf)) = (
                    ev.params.get("requestId").and_then(|v| v.as_str()),
                    main_frame.as_deref(),
                ) && fid == Some(mf)
                {
                    main_reqs.insert(rid.to_string());
                    tracker.record_request_start();
                }
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                if let Some(rid) = ev.params.get("requestId").and_then(|v| v.as_str())
                    && main_reqs.remove(rid)
                {
                    tracker.record_request_end();
                }
            }
            _ => {}
        }
    }
}

/// Whether a MIME type is interesting for content-extraction fallback.
fn is_capturable_mime(mime: &str) -> bool {
    let m = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        m.as_str(),
        "application/json"
            | "application/ld+json"
            | "application/vnd.api+json"
            | "text/json"
            | "text/plain"
    )
}

/// Reaps a legacy-path target when the render is cancelled before its normal
/// cleanup runs.
///
/// Targets created with `Target.createTarget` are owned by the browser, so
/// dropping the connection orphans them. It also detaches the session, and a
/// detach releases whatever the outbound guard had left paused, which is the
/// one case the guard cannot answer for. Mirrors the pooled path's `Drop` reap.
///
/// No `armed` flag: the normal path takes the id out of `tid_slot`, so after a
/// completed render there is nothing here to do.
struct LegacyTargetReaper {
    conn: Arc<CdpConnection>,
    tid_slot: std::sync::Arc<StdMutex<Option<String>>>,
    proxy_ctx: Option<String>,
    renderer: String,
}

impl Drop for LegacyTargetReaper {
    fn drop(&mut self) {
        let Some(tid) = self.tid_slot.lock().ok().and_then(|mut slot| slot.take()) else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let conn = self.conn.clone();
        let proxy_ctx = self.proxy_ctx.clone();
        let renderer = self.renderer.clone();
        tokio::spawn(async move {
            close_target(&conn, &tid, &renderer).await;
            if let Some(ctx) = proxy_ctx {
                let _ = conn
                    .send_recv(
                        "Target.disposeBrowserContext",
                        serde_json::json!({ "browserContextId": ctx }),
                        None,
                        Duration::from_secs(2),
                    )
                    .await;
            }
            conn.close().await;
        });
    }
}

async fn close_target(conn: &CdpConnection, target_id: &str, renderer: &str) {
    let m = crw_core::metrics::metrics();
    match conn
        .send_recv(
            "Target.closeTarget",
            serde_json::json!({ "targetId": target_id }),
            None,
            TARGET_CLOSE_TIMEOUT,
        )
        .await
    {
        Ok(_) => {
            m.target_lifecycle_total
                .with_label_values(&[renderer, "closed"])
                .inc();
        }
        Err(e) => {
            // closeTarget timed out or returned an error. Page likely
            // still alive in chrome — that's a leak, but we have to
            // move on. Surface as warn so it shows up in operator logs.
            m.target_lifecycle_total
                .with_label_values(&[renderer, "leaked"])
                .inc();
            tracing::warn!(
                renderer,
                target_id,
                error = %e,
                "Target.closeTarget did not complete cleanly; treating as leaked"
            );
        }
    }
}

/// Consume events from `events` until `Page.loadEventFired` (returns the main
/// document status) or a fatal event arrives. Uses `main_document_status`
/// captured from `Network.responseReceived` when available.
///
/// NOTE: proceeding on networkAlmostIdle here (instead of loadEventFired) was
/// tried and REVERTED — under CPU load the network has natural lulls mid-load,
/// so ≤2-in-flight fired during a large page's progressive load and returned
/// early, truncating it (mtkxjs 180KB→46KB, astrazeneca 4.3KB→212B). almost-idle
/// is only a safe POST-load readiness signal (where the load event already
/// guaranteed the main content), not a "page fully loaded" signal. Keep the wait
/// on the real `load` event.
async fn wait_for_page_ready(
    mut events: broadcast::Receiver<CdpEvent>,
    session_id: &str,
    timeout: Duration,
) -> CrwResult<u16> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut main_document_status: Option<u16> = None;
    let mut main_frame: Option<String> = None;

    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Err(_) => return Err(CrwError::Timeout(timeout.as_millis() as u64)),
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                return Err(CrwError::RendererError(
                    "CDP event channel closed before load".into(),
                ));
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Ok(ev)) => {
                if ev.session_id.as_deref() != Some(session_id) {
                    continue;
                }
                match ev.method.as_str() {
                    "Network.responseReceived" => {
                        let is_document = ev
                            .params
                            .get("type")
                            .and_then(|v| v.as_str())
                            .is_some_and(|v| v == "Document");
                        // An iframe is a Document too. Without the frame check a
                        // 404 ad/widget frame inside a healthy page stamps the
                        // page 404 — harmless while nothing read the status, but
                        // `ScrapeData::http_error` now fails the page on it.
                        // First Document response wins the main frame; later ones
                        // on that frame are redirects, and last-wins is right.
                        let frame_id = ev.params.get("frameId").and_then(|v| v.as_str());
                        // Adopt only a frame we can actually name. Adopting a
                        // `None` would leave the slot unfilled and let the NEXT
                        // Document response — an iframe — claim it.
                        if is_document && main_frame.is_none() && frame_id.is_some() {
                            main_frame = frame_id.map(str::to_string);
                        }
                        // Until a main frame is known, keep the old behaviour of
                        // taking any Document status rather than reporting none.
                        if is_document
                            && (main_frame.is_none() || frame_id == main_frame.as_deref())
                        {
                            main_document_status = ev
                                .params
                                .get("response")
                                .and_then(|r| r.get("status"))
                                .and_then(|s| s.as_f64())
                                .map(|s| s as u16)
                                .or(main_document_status);
                        }
                    }
                    "Page.loadEventFired" => {
                        return Ok(main_document_status.unwrap_or(200));
                    }
                    // NOTE: proceeding at Page.domContentEventFired (DCL) instead of
                    // the full `load` was tried (fast_ready) and REVERTED — it
                    // truncated large progressive pages (mtkxjs 180KB→107KB, the
                    // content kept arriving via post-DCL subresources) and gave no
                    // p90 gain (doomed pages fire load late/never anyway). The
                    // `load`-wait is what lets the page fully render before the
                    // readiness poll; keep it. fast_ready only affects the poll.
                    "Inspector.targetCrashed" => {
                        return Err(CrwError::RendererError(
                            "Target crashed during render".into(),
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
}

#[async_trait]
impl PageFetcher for CdpRenderer {
    async fn fetch(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        wait_for_ms: Option<u64>,
        deadline: crw_core::Deadline,
    ) -> CrwResult<FetchResult> {
        // Overall hard timeout: page_timeout + wait_for + challenge retry budget
        // + content-stability budget (auto-mode only) + overhead. Challenge retries
        // can add up to CHALLENGE_MAX_RETRIES * CHALLENGE_POLL_INTERVAL_MS.
        // When the caller didn't supply `wait_for_ms`, fetch_with_ws uses the
        // SPA selector poll instead of a fixed sleep — size the budget for
        // its worst-case SPA_SELECTOR_MAX_MS rather than the old 2s default.
        let wait_dur = Duration::from_millis(wait_for_ms.unwrap_or(SPA_SELECTOR_MAX_MS));
        let challenge_budget = challenge_retry_budget(self.challenge_max_retries);
        let stability_budget = if wait_for_ms.is_none() {
            Duration::from_millis(CONTENT_STABILITY_MAX_MS)
        } else {
            Duration::ZERO
        };
        let internal_timeout =
            self.page_timeout + wait_dur + challenge_budget + stability_budget + FETCH_OVERHEAD;
        // Clamp internal timeout against the caller's remaining budget so the
        // CDP fetch never exceeds the end-to-end deadline. Snapshot remaining
        // once so the diagnostic log and the actual timeout can't disagree
        // across consecutive monotonic reads.
        let remaining = deadline.remaining();
        let overall_timeout = if internal_timeout > remaining {
            tracing::debug!(
                renderer = %self.name,
                internal_ms = internal_timeout.as_millis() as u64,
                remaining_ms = remaining.as_millis() as u64,
                "CDP outer timeout shrunk to fit remaining request deadline. \
                 If the caller supplied an explicit `deadlineMs`, this clamp \
                 is intentional — the request asked for a tighter cap. \
                 Otherwise (issue #35) raise request.deadline_ms_default or \
                 enable request.auto_extend_deadline_for_ladder so per-tier \
                 timeouts get their full configured allowance."
            );
            remaining
        } else {
            internal_timeout
        };
        if overall_timeout.is_zero() {
            // Caller's deadline is already past. Report the budget they were
            // given: the overrun is measured the instant `remaining()` hits
            // zero, so it is always a few milliseconds and reads as a nonsense
            // "Timeout after 1ms".
            return Err(CrwError::Timeout(deadline.requested_ms()));
        }

        let first = self
            .fetch_once(url, headers, wait_for_ms, deadline, overall_timeout)
            .await;

        // On-failure country fallback (residential chrome_proxy tier only).
        //
        // The chrome_proxy tier applies the per-request country as a DataImpulse
        // `{base_user}__cr.{cc}` credential suffix (see `fetch_inner`). When the
        // requested country has no working exit, DataImpulse rejects the HTTPS
        // CONNECT tunnel (503 NO_RAY) and Chrome surfaces it as a
        // `net::ERR_TUNNEL_CONNECTION_FAILED`-class `Page.navigate` error — the
        // target is NEVER reached, so retrying carries no double-scrape / no
        // extra-credit risk (credits are assigned per-tier by the caller on the
        // returned result, not per inner attempt). Retry exactly ONCE with the
        // tier's default country. Strictly gated (see the two helpers) to:
        //   - the country-proxy path (base creds set, no BYOP proxy override),
        //   - a *non-default* country having actually been requested,
        //   - a proxy-CONNECT-class error only (never a target 4xx/5xx, never a
        //     timeout — those don't imply a dead country exit).
        if let Err(e) = &first
            && is_proxy_tunnel_error(e)
            && self.should_retry_with_default_country()
        {
            let remaining = deadline.remaining();
            if !remaining.is_zero() {
                let retry_timeout = internal_timeout.min(remaining);
                tracing::info!(
                    renderer = %self.name,
                    url,
                    default_country = ?self.default_country,
                    error = %e,
                    "country proxy CONNECT tunnel failed; retrying once with default country"
                );
                // Re-run the SAME attempt with the country forced to the tier
                // default. `should_retry_with_default_country` guarantees the
                // requested country differs from the default, so this cannot
                // loop: the retry runs under the default and is not retried again.
                return crate::REQUEST_COUNTRY
                    .scope(
                        self.default_country.clone(),
                        self.fetch_once(url, headers, wait_for_ms, deadline, retry_timeout),
                    )
                    .await;
            }
        }
        first
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn supports_js(&self) -> bool {
        true
    }

    async fn is_available(&self) -> bool {
        let conn = match self.connect_with_retry().await {
            Ok(conn) => conn,
            Err(_) => return false,
        };
        let check = conn
            .send_recv(
                "Browser.getVersion",
                serde_json::json!({}),
                None,
                Duration::from_secs(5),
            )
            .await;
        conn.close().await;
        check.is_ok()
    }
}

/// Check if HTML looks like a Cloudflare/anti-bot challenge page.
fn is_challenge_page(html: &str) -> bool {
    if html.len() > 50_000 {
        return false;
    }
    let lower = html.to_lowercase();
    lower.contains("just a moment")
        || lower.contains("cf-browser-verification")
        || lower.contains("cf-challenge-running")
        || lower.contains("challenge-platform")
        || (lower.contains("challenge") && lower.contains("cloudflare"))
        || lower.contains("attention required")
}

/// Detect LightPanda/Chrome navigation error pages.
fn detect_navigation_error(html: &str) -> Option<String> {
    if html.len() > 2000 {
        return None;
    }
    let lower = html.to_lowercase();
    if lower.contains("navigation failed") || lower.contains("navigationerror") {
        if let Some(start) = lower.find("reason:") {
            let after = &lower[start + 7..];
            let reason = after
                .split(&['<', '\n'][..])
                .next()
                .unwrap_or("unknown")
                .trim();
            return Some(reason.to_string());
        }
        return Some("unknown".to_string());
    }
    None
}

/// True for a proxy-egress CONNECT/tunnel failure — the class of error Chrome
/// raises when the upstream proxy refuses to establish the HTTPS tunnel (e.g.
/// DataImpulse returning `503 NO_RAY` on CONNECT for a dead country exit).
///
/// Deliberately narrow: matches only the two `net::ERR_*` proxy-connect codes
/// where a *different egress* (a different country exit) could plausibly
/// succeed. It never matches target HTTP statuses, DNS/name errors, or
/// timeouts — those don't indicate a dead country exit, so retrying the country
/// would be pointless (or, worse, mask a genuine target failure).
fn is_proxy_tunnel_error(err: &CrwError) -> bool {
    let msg = match err {
        CrwError::RendererError(m) => m.as_str(),
        _ => return false,
    };
    msg.contains("ERR_TUNNEL_CONNECTION_FAILED") || msg.contains("ERR_PROXY_CONNECTION_FAILED")
}

impl CdpRenderer {
    /// Run a single CDP fetch attempt bounded by `overall_timeout`. Factored out
    /// of the `PageFetcher::fetch` entry point so the country-fallback retry can
    /// invoke the exact same dispatch a second time under a different
    /// `REQUEST_COUNTRY` scope.
    async fn fetch_once(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        wait_for_ms: Option<u64>,
        deadline: crw_core::Deadline,
        overall_timeout: Duration,
    ) -> CrwResult<FetchResult> {
        // When a per-request proxy is active, bypass the context pool: each
        // proxied request needs a fresh browser context created with its own
        // `proxyServer`, which `fetch_with_ws` builds and disposes. The pool is
        // reserved for the (common) no-proxy path where contexts are reused.
        let proxy_active = crate::REQUEST_PROXY
            .try_with(|p| p.is_some())
            .unwrap_or(false);
        let fut = async {
            if let Some(pool) = self.pool.as_ref().filter(|_| !proxy_active) {
                self.fetch_with_pool(pool, url, headers, wait_for_ms, deadline)
                    .await
            } else {
                self.fetch_with_ws(url, headers, wait_for_ms, deadline)
                    .await
            }
        };
        tokio::time::timeout(overall_timeout, fut)
            .await
            .map_err(|_| CrwError::Timeout(overall_timeout.as_millis() as u64))?
    }

    /// Whether a failed attempt on this tier should be retried once with the
    /// tier's `default_country`. Requires ALL of:
    ///   1. base DataImpulse creds set — i.e. this IS the chrome_proxy tier that
    ///      composes `__cr.<cc>` credentials (plain `chrome` leaves this `None`);
    ///   2. no per-request BYOP/rotated proxy — a `REQUEST_PROXY` supplies its
    ///      own auth + egress (it takes precedence over the country credential in
    ///      `fetch_inner`), so its CONNECT failure is not a country problem;
    ///   3. a *valid, non-default* country was actually requested — otherwise the
    ///      first attempt already egressed through the default exit and the retry
    ///      would be byte-for-byte identical.
    ///
    /// Country normalization mirrors `fetch_inner` exactly (trim, lowercase,
    /// 2-alpha) so an invalid `REQUEST_COUNTRY` (which `fetch_inner` already
    /// drops to the default) does not trigger a pointless retry.
    fn should_retry_with_default_country(&self) -> bool {
        if self.proxy_auth_base.is_none() {
            return false;
        }
        let byop_active = crate::REQUEST_PROXY
            .try_with(|p| p.is_some())
            .unwrap_or(false);
        if byop_active {
            return false;
        }
        let norm = |c: &str| -> Option<String> {
            let c = c.trim().to_lowercase();
            (c.len() == 2 && c.chars().all(|ch| ch.is_ascii_alphabetic())).then_some(c)
        };
        let requested = crate::REQUEST_COUNTRY
            .try_with(|c| c.clone())
            .ok()
            .flatten()
            .and_then(|c| norm(&c));
        let default = self.default_country.as_deref().and_then(norm);
        match requested {
            Some(req) => Some(req) != default,
            // No (valid) country requested → first attempt used the default.
            None => false,
        }
    }

    /// Pool-backed fetch path. Acquires a checked-out browser context from
    /// the pool, runs `fetch_inner` with the slot's ctx_id + a recorder that
    /// writes the new target_id into the slot, then `release()`s the slot
    /// (which owns `closeTarget` + dispose + recreate-ctx).
    async fn fetch_with_pool(
        &self,
        pool: &Arc<crate::browser_pool::BrowserContextPool<CdpConnection>>,
        url: &str,
        headers: &HashMap<String, String>,
        wait_for_ms: Option<u64>,
        deadline: crw_core::Deadline,
    ) -> CrwResult<FetchResult> {
        let start = Instant::now();
        let handshake_t0 = Instant::now();
        // Reserved lane (B, pooled path): batch takes the gate before checking
        // out a pool slot so interactive always finds reserved slots free; held
        // for the whole fetch alongside the pool guard. Class read on the async
        // side. `pool_batch_gate` is `Some` whenever `pool` is set.
        let _render_gate = match &self.pool_batch_gate {
            Some(gate) => gate.enter(crw_core::current_scrape_class()).await,
            None => None,
        };
        let acquire_t0 = Instant::now();
        let guard = pool.acquire().await?;
        let acquire_elapsed = acquire_t0.elapsed();
        crw_core::metrics::metrics()
            .chrome_pool_acquire_seconds
            .observe(acquire_elapsed.as_secs_f64());
        // Best-effort acquire-source label: we currently don't surface from
        // `acquire()` whether it hit idle or created new — record under a
        // generic bucket. Plumbing the precise label is a follow-up.
        crw_core::metrics::metrics()
            .chrome_pool_acquires_total
            .with_label_values(&["hit_idle"])
            .inc();

        // Recorder writes the new target_id into the slot synchronously
        // (inside fetch_inner, immediately after createTarget Ok).
        let guard_for_rec = &guard;
        let recorder = |tid: &str| guard_for_rec.record_target(tid.to_string());

        let ctx_id = guard.ctx_id.clone();
        let res = self
            .fetch_inner(
                &guard.conn,
                Some(&ctx_id),
                &recorder,
                url,
                headers,
                wait_for_ms,
                deadline,
            )
            .await;

        // Record total handshake-overhead for this request (acquire + create
        // target + attach happen inside fetch_inner). B2 gate metric.
        crw_core::metrics::metrics()
            .chrome_request_handshake_seconds
            .with_label_values(&["on", "hit_idle"])
            .observe(handshake_t0.elapsed().as_secs_f64());

        // Always release — swallow recycle error per plan's error-precedence
        // policy (fetch success/failure is what the caller cares about).
        if let Err(e) = guard.release().await {
            tracing::warn!(error = %e, "pool: release returned error (slot recycled as Dead)");
        }

        let (html, status_code, truncated, final_href, captured_responses, screenshot, _tid) = res?;

        if html.is_empty() {
            return Err(CrwError::RendererError(
                "Empty HTML from CDP renderer".into(),
            ));
        }
        if let Some(reason) = detect_navigation_error(&html) {
            return Err(CrwError::RendererError(format!(
                "Navigation failed: {reason}"
            )));
        }

        let final_url = final_href.filter(|h| *h != url);
        Ok(FetchResult {
            url: url.to_string(),
            final_url,
            status_code,
            html,
            content_type: None,
            raw_bytes: None,
            rendered_with: Some(self.name.clone()),
            elapsed_ms: start.elapsed().as_millis() as u64,
            warning: if truncated {
                Some(self.budget_truncated_warning())
            } else {
                None
            },
            render_decision: None,
            credit_cost: 0,
            warnings: if truncated {
                vec![self.budget_truncated_warning()]
            } else {
                Vec::new()
            },
            truncated,
            deadline_exceeded: deadline.remaining().is_zero(),
            captured_responses,
            screenshot,
            block: None,
        })
    }

    /// Name the tier that actually ran out of budget. This struct drives every
    /// CDP-speaking renderer, so the hardcoded `chrome_budget_truncated` blamed
    /// Chrome for LightPanda's much smaller budget — prod, 2026-07-24: every
    /// truncation sampled was LightPanda's, reported as Chrome's, sending anyone
    /// debugging it to the wrong tier. Chrome's string is unchanged, so existing
    /// consumers see no difference.
    fn budget_truncated_warning(&self) -> String {
        format!("{}_budget_truncated", self.name)
    }

    /// Inner fetch with WebSocket lifecycle management.
    async fn fetch_with_ws(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        wait_for_ms: Option<u64>,
        deadline: crw_core::Deadline,
    ) -> CrwResult<FetchResult> {
        let start = Instant::now();
        let handshake_t0 = Instant::now();

        // Limit concurrent WebSocket connections to pool_size, with a reserved
        // lane for interactive: batch takes the gate first (bounding batch's
        // share of the pool); interactive skips it. Both held for the whole
        // fetch. Class read on the async side (before any spawn_blocking).
        let _render_gate = self
            .conn_batch_gate
            .enter(crw_core::current_scrape_class())
            .await;
        let _permit = self
            .conn_semaphore
            .acquire()
            .await
            .map_err(|_| CrwError::RendererError("Connection pool closed".into()))?;

        let conn = Arc::new(self.connect_with_retry().await?);

        // Per-request proxy: create a dedicated browser context whose egress is
        // routed through `proxyServer` (credentials, if any, are supplied by the
        // `Fetch.authRequired` pump in `fetch_inner`). Disposed after the target
        // closes. `None` keeps the prior browser-level behaviour unchanged.
        let proxy_ctx: Option<String> =
            match crate::REQUEST_PROXY.try_with(|p| p.clone()).ok().flatten() {
                Some(entry) => {
                    // Chrome cannot authenticate SOCKS proxies (no Fetch.authRequired
                    // for SOCKS). Reject socks+auth on the CDP path with a clear error
                    // rather than hanging the auth pump on an event that never fires.
                    if !entry.supports_cdp_auth() {
                        return Err(CrwError::RendererError(
                            "SOCKS5 proxy authentication is not supported on the \
                             Chrome/JS renderer; use an HTTP/HTTPS proxy for JS rendering \
                             or a credential-less SOCKS proxy"
                                .into(),
                        ));
                    }
                    let v = conn
                        .send_recv(
                            "Target.createBrowserContext",
                            crate::cdp_conn::browser_ctx_params(Some(entry.chrome_proxy_server())),
                            None,
                            Duration::from_secs(2),
                        )
                        .await?;
                    let ctx = v
                        .get("browserContextId")
                        .and_then(|x| x.as_str())
                        .ok_or_else(|| {
                            CrwError::RendererError(
                                "createBrowserContext: missing browserContextId".into(),
                            )
                        })?
                        .to_string();
                    Some(ctx)
                }
                None => None,
            };

        // Legacy path: `tid_slot` is the SOLE authoritative source for target
        // close on both Ok and Err branches. fetch_inner no longer closes
        // targets itself — we own that here. `Arc<Mutex<...>>` (not `Cell`)
        // so the recorder closure satisfies `Send + Sync` across the await.
        let tid_slot: std::sync::Arc<std::sync::Mutex<Option<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let tid_slot_rec = tid_slot.clone();
        let recorder = move |tid: &str| {
            *tid_slot_rec.lock().unwrap() = Some(tid.to_string());
        };
        // `fetch()` wraps this whole call in a timeout, so on expiry the future
        // is dropped mid-`fetch_inner` and the cleanup below never runs. The
        // pooled path already reaps from `Drop` for that reason; without the
        // same here the target survives with `Fetch` still enabled, and the
        // session detaching behind it releases every request the guard had
        // paused. Disarms itself: the normal path takes the target id out of the
        // slot, so a completed render leaves this a no-op.
        let _reaper = LegacyTargetReaper {
            conn: conn.clone(),
            tid_slot: tid_slot.clone(),
            proxy_ctx: proxy_ctx.clone(),
            renderer: self.name.clone(),
        };
        let result = self
            .fetch_inner(
                &conn,
                proxy_ctx.as_deref(),
                &recorder,
                url,
                headers,
                wait_for_ms,
                deadline,
            )
            .await;

        // B2 gate metric: pre-navigation overhead (connect + createTarget +
        // attach). Pool=off arm; pool=on arm is recorded in fetch_with_pool.
        crw_core::metrics::metrics()
            .chrome_request_handshake_seconds
            .with_label_values(&["off", "n/a"])
            .observe(handshake_t0.elapsed().as_secs_f64());
        let captured_tid = tid_slot.lock().unwrap().take();
        if let Some(tid) = captured_tid {
            close_target(&conn, &tid, &self.name).await;
        }

        // Dispose the per-request proxy context (best-effort) before closing the
        // connection, so a proxied request doesn't leak browser contexts.
        if let Some(ctx) = &proxy_ctx {
            let _ = conn
                .send_recv(
                    "Target.disposeBrowserContext",
                    serde_json::json!({ "browserContextId": ctx }),
                    None,
                    Duration::from_secs(1),
                )
                .await;
        }

        conn.close().await;

        let (
            html,
            status_code,
            truncated,
            final_href,
            captured_responses,
            screenshot,
            _tid_ignored,
        ) = result?;

        if html.is_empty() {
            return Err(CrwError::RendererError(
                "Empty HTML from CDP renderer".into(),
            ));
        }

        if let Some(reason) = detect_navigation_error(&html) {
            return Err(CrwError::RendererError(format!(
                "Navigation failed: {reason}"
            )));
        }

        let final_url = final_href.filter(|h| *h != url);

        Ok(FetchResult {
            url: url.to_string(),
            final_url,
            status_code,
            html,
            content_type: None,
            raw_bytes: None,
            rendered_with: Some(self.name.clone()),
            elapsed_ms: start.elapsed().as_millis() as u64,
            warning: if truncated {
                Some(self.budget_truncated_warning())
            } else {
                None
            },
            render_decision: None,
            credit_cost: 0,
            warnings: if truncated {
                vec![self.budget_truncated_warning()]
            } else {
                Vec::new()
            },
            truncated,
            deadline_exceeded: deadline.remaining().is_zero(),
            captured_responses,
            screenshot,
            block: None,
        })
    }

    /// Evaluate `window.location.href` to capture the URL after redirects.
    /// Returns `None` on any failure (caller treats this as "no redirect known").
    async fn eval_href(
        conn: &CdpConnection,
        session_id: &str,
        timeout: Duration,
    ) -> Option<String> {
        let eval_result = conn
            .send_recv(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": "window.location.href",
                    "returnByValue": true
                }),
                Some(session_id),
                timeout,
            )
            .await
            .ok()?;
        eval_result
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// Read the page's clearance cookies (`cf_clearance` / `__cf_bm`) for `url`
    /// via CDP `Network.getCookies`, filtered to the clearance pair. Best-effort:
    /// any error yields an empty vec (→ `ClearanceCache::put` no-ops).
    async fn capture_clearance_cookies(
        conn: &CdpConnection,
        session_id: &str,
        url: &str,
    ) -> Vec<crate::clearance::ClearanceCookie> {
        let Ok(resp) = conn
            .send_recv(
                "Network.getCookies",
                serde_json::json!({ "urls": [url] }),
                Some(session_id),
                Duration::from_secs(2),
            )
            .await
        else {
            return Vec::new();
        };
        let Some(arr) = resp.get("cookies").and_then(|c| c.as_array()) else {
            return Vec::new();
        };
        // `Network.getCookies` can return the same clearance name under two
        // domain scopes (host `www.x` + domain `.x`). Dedupe by name, preferring
        // a domain-wide (leading-dot) scope so the re-injected cookie matches the
        // broadest path Cloudflare set — otherwise injected + server copies
        // accumulate across a crawl.
        let mut by_name: std::collections::HashMap<String, crate::clearance::ClearanceCookie> =
            std::collections::HashMap::new();
        for c in arr {
            let Some(name) = c.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            if !crate::clearance::is_clearance_cookie(name) {
                continue;
            }
            let (Some(value), Some(domain)) = (
                c.get("value").and_then(|v| v.as_str()),
                c.get("domain").and_then(|d| d.as_str()),
            ) else {
                continue;
            };
            let cookie = crate::clearance::ClearanceCookie {
                name: name.to_string(),
                value: value.to_string(),
                domain: domain.to_string(),
                path: c
                    .get("path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("/")
                    .to_string(),
                secure: c.get("secure").and_then(|s| s.as_bool()).unwrap_or(true),
                http_only: c.get("httpOnly").and_then(|s| s.as_bool()).unwrap_or(false),
            };
            // Keep the existing entry only if it's domain-wide and the new one isn't.
            match by_name.get(name) {
                Some(existing) if existing.domain.starts_with('.') && !domain.starts_with('.') => {}
                _ => {
                    by_name.insert(name.to_string(), cookie);
                }
            }
        }
        by_name.into_values().collect()
    }

    /// Scroll the page viewport-by-viewport until `document.body.scrollHeight`
    /// stops growing or `AUTO_SCROLL_MAX_STEPS` is reached. Triggers lazy-loaded
    /// images, infinite-scroll feeds, and below-the-fold hydration.
    ///
    /// Best-effort: failures swallowed (debug log) so a flaky evaluate doesn't
    /// abort the whole render.
    async fn auto_scroll(conn: &CdpConnection, session_id: &str, timeout: Duration) {
        let script = format!(
            r#"
            (async () => {{
                const sleep = (ms) => new Promise(r => setTimeout(r, ms));
                const max_steps = {max_steps};
                const step_delay = {delay};
                let last_h = 0;
                let stable = 0;
                let steps = 0;
                for (let i = 0; i < max_steps; i++) {{
                    steps++;
                    window.scrollBy(0, window.innerHeight || 800);
                    await sleep(step_delay);
                    const h = document.body ? document.body.scrollHeight : 0;
                    if (h <= last_h) {{ stable++; if (stable >= 2) break; }} else {{ stable = 0; }}
                    last_h = h;
                }}
                window.scrollTo(0, 0);
                return {{ steps, final_height: last_h }};
            }})()
            "#,
            max_steps = AUTO_SCROLL_MAX_STEPS,
            delay = AUTO_SCROLL_STEP_DELAY_MS,
        );
        let result = conn
            .send_recv(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": script,
                    "awaitPromise": true,
                    "returnByValue": true,
                }),
                Some(session_id),
                timeout,
            )
            .await;
        match result {
            Ok(v) => tracing::debug!(?v, "auto_scroll completed"),
            Err(e) => tracing::debug!(error = %e, "auto_scroll failed (non-fatal)"),
        }
    }

    /// Click `[aria-expanded=false]` toggles and "load more" / "show full"
    /// buttons that hide content behind a click. Bounded by
    /// `AUTO_CLICK_MAX_CLICKS` and `AUTO_CLICK_BUDGET_MS`. Skips submit /
    /// link / external-nav buttons by checking element types and `<a>` tags.
    /// Best-effort — failures swallowed so a flaky evaluate doesn't abort
    /// the whole render.
    async fn auto_click_reveal(conn: &CdpConnection, session_id: &str, timeout: Duration) {
        let script = format!(
            r#"
            (async () => {{
                const sleep = (ms) => new Promise(r => setTimeout(r, ms));
                const max_clicks = {max_clicks};
                const delay = {delay};
                const REVEAL_RE = /^(load|show|read|view|see|expand)\s*(more|full|all|details?)?\b|^more\b|^expand\b/i;
                const candidates = new Set();
                // aria-expanded toggles
                document.querySelectorAll('[aria-expanded="false"]').forEach(el => {{
                    if (el.tagName !== 'A') candidates.add(el);
                }});
                // text-matching buttons / clickable divs
                document.querySelectorAll('button, [role="button"], summary').forEach(el => {{
                    const text = (el.innerText || el.textContent || '').trim();
                    if (text && text.length < 40 && REVEAL_RE.test(text)) candidates.add(el);
                }});
                let clicks = 0;
                for (const el of candidates) {{
                    if (clicks >= max_clicks) break;
                    if (!el.isConnected) continue;
                    // Skip elements outside the viewport range — we don't
                    // want to scroll-to-element on nav drawers.
                    const rect = el.getBoundingClientRect();
                    if (rect.bottom < -2000 || rect.top > 20000) continue;
                    try {{ el.click(); clicks++; await sleep(delay); }} catch (e) {{ /* ignore */ }}
                }}
                return {{ clicks }};
            }})()
            "#,
            max_clicks = AUTO_CLICK_MAX_CLICKS,
            delay = AUTO_CLICK_DELAY_MS,
        );
        let result = conn
            .send_recv(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": script,
                    "awaitPromise": true,
                    "returnByValue": true,
                }),
                Some(session_id),
                timeout,
            )
            .await;
        match result {
            Ok(v) => tracing::debug!(?v, "auto_click_reveal completed"),
            Err(e) => tracing::debug!(error = %e, "auto_click_reveal failed (non-fatal)"),
        }
    }

    /// Best-effort consent banner dismissal. Errors are swallowed — a missing
    /// banner, sandboxed iframe, or unsupported `__tcfapi` shouldn't fail the
    /// fetch. The script returns the click count for telemetry but we don't
    /// surface it on the FetchResult yet (would need a new field).
    async fn dismiss_consent(conn: &CdpConnection, session_id: &str) {
        let res = conn
            .send_recv(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": CMP_DISMISS_JS,
                    "returnByValue": true,
                }),
                Some(session_id),
                Duration::from_secs(2),
            )
            .await;
        match res {
            Ok(v) => {
                let clicks = v
                    .get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                if clicks > 0 {
                    tracing::debug!(clicks, "consent banner dismissed");
                }
            }
            Err(e) => tracing::debug!("CMP dismiss eval failed: {e}"),
        }
    }

    async fn eval_html(
        conn: &CdpConnection,
        session_id: &str,
        timeout: Duration,
    ) -> CrwResult<String> {
        // Tier 0 metric M3: HTML snapshot round-trip. Observed on every call
        // (post-navigate, SPA poll, scroll re-snapshot) so we can see how the
        // snapshot mix behaves under different page types.
        let snap_t0 = Instant::now();
        let eval_result = conn
            .send_recv(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": HTML_SNAPSHOT_JS,
                    "returnByValue": true
                }),
                Some(session_id),
                timeout,
            )
            .await?;
        crw_core::metrics::metrics()
            .chrome_snapshot_seconds
            .observe(snap_t0.elapsed().as_secs_f64());

        Ok(eval_result
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// Poll for SPA readiness up to [`SPA_SELECTOR_MAX_MS`]. Three exit
    /// conditions, any of which counts as "ready":
    ///   * selector mounted AND body innerText ≥ [`SPA_BODY_TEXT_MIN_CHARS`]
    ///     — covers static + already-hydrated pages (one tick, fast path)
    ///   * selector mounted AND network has been idle for ≥
    ///     [`NETWORK_IDLE_QUIET_MS`] — covers SPAs whose XHR fetches finished
    ///     but body text is still under the threshold (the page is done; the
    ///     content just isn't bulky)
    ///   * budget elapses — caller proceeds with whatever's there
    ///
    /// Single eval per tick returns the body text length (or -1 when the
    /// selector is missing). Healthy pages with text already present clear
    /// on the first poll. The network-idle gate requires the selector to be
    /// mounted first so we don't exit on the pre-navigate-fetch idle window.
    async fn wait_for_spa_selector(
        conn: &CdpConnection,
        session_id: &str,
        timeout: Duration,
        net: &NetworkActivityTracker,
        spa_max: Duration,
        fast_ready: bool,
    ) -> bool {
        // NOTE: readiness v2 (quality-gated content-ready probe, `eval_content_ready`)
        // was tried here and REVERTED — it hit the SAME fundamental tradeoff as
        // every prior lever: the content-quality threshold either caps link-heavy
        // pages (MIN=250 → columbusjack no speedup) or truncates progressive pages
        // (MIN=120 → abcnews 14605→281, both runs). A generic signal can't tell
        // "complete at 243 chars" (columbusjack) from "281 now → 14605 soon"
        // (abcnews). No p90 gain + truncation → kept v1 fast_ready below.
        let deadline = Instant::now() + spa_max;
        // fast-ready uses body innerText (selector-independent) so content pages
        // that don't use the main/article/#root containers still detect content;
        // the legacy path gates on the specific selectors (returns -1 if absent).
        let expr = if fast_ready {
            r#"(() => { const t = (document.body && document.body.innerText) || ""; return t.trim().length; })()"#.to_string()
        } else {
            format!(
                r#"(() => {{
                    if (!document.querySelector({sel:?})) return -1;
                    const t = (document.body && document.body.innerText) || "";
                    return t.trim().length;
                }})()"#,
                sel = SPA_CONTENT_SELECTORS
            )
        };
        while Instant::now() < deadline {
            match conn
                .send_recv(
                    "Runtime.evaluate",
                    serde_json::json!({ "expression": expr, "returnByValue": true }),
                    Some(session_id),
                    timeout,
                )
                .await
            {
                Ok(v) => {
                    let len = v
                        .get("result")
                        .and_then(|r| r.get("value"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(-1);
                    if fast_ready {
                        // Substantial body text → ready immediately.
                        if len >= SPA_BODY_TEXT_MIN_CHARS as i64 {
                            return true;
                        }
                        // Content present (≥ floor) AND network settled
                        // (networkAlmostIdle ≤2, the signal that actually fires on
                        // chatty pages). The content floor is the mandatory gate
                        // that prevents snapshotting an empty CSR shell.
                        if len >= SPA_CONTENT_FLOOR_CHARS
                            && net.is_settled(NETWORK_IDLE_QUIET_MS, ALMOST_IDLE_MAX_INFLIGHT)
                        {
                            tracing::debug!(
                                text_len = len,
                                "fast-ready: content present + networkAlmostIdle, snapshotting"
                            );
                            return true;
                        }
                        // NOTE: a DOM-stable (text-length unchanged for N ms)
                        // early-exit was tried here and REVERTED — it truncated
                        // pages whose text was momentarily stable then grew (bench:
                        // success 90%→85%, +4 new failures across runs). Late-burst
                        // content arrives after a stable gap; only the real `load`
                        // event / networkAlmostIdle are safe. Keep the ceiling.
                    } else {
                        let selector_mounted = len >= 0;
                        if is_spa_text_ready(len) {
                            return true;
                        }
                        if selector_mounted && net.is_idle(NETWORK_IDLE_QUIET_MS) {
                            tracing::debug!(
                                text_len = len,
                                "SPA poll exiting on network-idle (selector mounted, text below threshold)"
                            );
                            return true;
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!("SPA selector poll eval failed: {e}");
                    return false;
                }
            }
            tokio::time::sleep(Duration::from_millis(SPA_SELECTOR_TICK_MS)).await;
        }
        false
    }

    /// Poll `document.documentElement.outerHTML` at a fixed interval until the
    /// rendered HTML stabilises and no longer looks like a loading placeholder,
    /// or until the stability budget is exhausted.
    async fn poll_until_content_stable(
        conn: &CdpConnection,
        session_id: &str,
        timeout: Duration,
    ) -> CrwResult<String> {
        let deadline = Instant::now() + Duration::from_millis(CONTENT_STABILITY_MAX_MS);
        let mut prev_len: u64 = 0;
        let mut stable_ticks: u32 = 0;
        let mut last_html = String::new();

        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(CONTENT_STABILITY_TICK_MS)).await;
            let html = Self::eval_html(conn, session_id, timeout).await?;
            let len = html.len() as u64;
            let placeholder_gone = !crate::detector::looks_like_loading_placeholder(&html);
            if is_content_stable(prev_len, len, placeholder_gone) {
                stable_ticks += 1;
                if stable_ticks >= 2 {
                    return Ok(html);
                }
            } else {
                stable_ticks = 0;
            }
            prev_len = len;
            last_html = html;
        }
        Ok(last_html)
    }

    #[allow(clippy::too_many_arguments)]
    async fn fetch_inner(
        &self,
        conn: &CdpConnection,
        browser_context_id: Option<&str>,
        target_recorder: &(dyn Fn(&str) + Send + Sync),
        url: &str,
        headers: &HashMap<String, String>,
        wait_for_ms: Option<u64>,
        deadline: crw_core::Deadline,
    ) -> CrwResult<(
        String,
        u16,
        bool,
        Option<String>,
        Vec<CapturedNetworkResponse>,
        Option<String>,
        String,
    )> {
        // 1. Create a blank target so navigation events can be observed reliably.
        // When `browser_context_id` is Some, the target is bound to that
        // context — load-bearing for pool isolation (cookies/storage do not
        // leak across contexts).
        let create_t0 = Instant::now();
        let mut create_params = serde_json::json!({ "url": "about:blank" });
        if let Some(ctx) = browser_context_id {
            create_params["browserContextId"] = serde_json::Value::String(ctx.to_string());
        }
        let create_result = conn
            .send_recv(
                "Target.createTarget",
                create_params,
                None,
                self.page_timeout,
            )
            .await?;
        crw_core::metrics::metrics()
            .chrome_target_create_seconds
            .observe(create_t0.elapsed().as_secs_f64());

        let target_id = create_result
            .get("targetId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CrwError::RendererError(format!("No targetId: {create_result}")))?
            .to_string();
        // CRITICAL — synchronous handoff to caller BEFORE any subsequent
        // `.await`. This is the only sync point that closes the cancellation
        // window between createTarget returning Ok and the next await. The
        // pooled caller writes the id into the slot's `CheckedOut.target_id`
        // here; the legacy caller writes into a stack-local `Cell`. Either way
        // the caller owns `closeTarget` from this point on — fetch_inner does
        // NOT call closeTarget on any branch.
        target_recorder(&target_id);
        crw_core::metrics::metrics()
            .target_lifecycle_total
            .with_label_values(&[&self.name, "created"])
            .inc();

        // 2. Attach to target
        let attach_result = conn
            .send_recv(
                "Target.attachToTarget",
                serde_json::json!({ "targetId": &target_id, "flatten": true }),
                None,
                self.page_timeout,
            )
            .await?;

        let session_id = attach_result
            .get("sessionId")
            .and_then(|value| value.as_str())
            .ok_or_else(|| CrwError::RendererError("CDP attach did not return sessionId".into()))?
            .to_string();

        for method in ["Page.enable", "Network.enable", "Runtime.enable"] {
            conn.send_recv(
                method,
                serde_json::json!({}),
                Some(&session_id),
                self.page_timeout,
            )
            .await?;
        }

        // Locale for the pinned proxy exit country. `None` (the default path,
        // and any request without a `country`) leaves every CDP call below
        // exactly as it was.
        //
        // Only the tier whose egress actually leaves from the pinned country
        // composes a country-suffixed proxy login (`proxy_auth_base`), and that
        // is the only tier where a country-derived locale is coherent with the
        // IP. On the direct chrome tier it would pair a foreign locale with the
        // box's own IP, and `Emulation.setLocaleOverride` is browser-wide in
        // Chromium, so issuing it in the shared direct browser could leak into
        // renders that never asked for a country. This also keeps lightpanda
        // out, whose partial CDP surface may not answer `Emulation.*` at all.
        // A scoped `REQUEST_PROXY` takes the egress away from the country
        // credential (it wins in `fetch_inner`), so it also takes the locale
        // away: the same rule `should_retry_with_default_country` applies.
        let byop_active = crate::REQUEST_PROXY
            .try_with(|p| p.is_some())
            .unwrap_or(false);
        let geo_locale = if self.proxy_auth_base.is_some() && !byop_active {
            crate::locale::request_locale()
        } else {
            None
        };
        let (caller_ua, extra_headers) = split_caller_headers(headers);
        // A caller-supplied `Accept-Language` wins over every language signal
        // (header, `navigator.languages`, locale override); the clock still
        // follows the exit country.
        let lang_locale = language_locale(geo_locale, &extra_headers);

        // Inject stealth scripts before navigation so they run on every new
        // document. With a pinned country the spoofed `navigator.languages`
        // follows that country instead of announcing a US browser from a
        // foreign exit IP; without one the source is the untouched constant.
        let stealth_source: std::borrow::Cow<'_, str> = match lang_locale {
            Some(locale) => std::borrow::Cow::Owned(
                STEALTH_JS.replace(STEALTH_DEFAULT_LANGUAGES, &locale.js_languages()),
            ),
            None => std::borrow::Cow::Borrowed(STEALTH_JS),
        };
        conn.send_recv(
            "Page.addScriptToEvaluateOnNewDocument",
            serde_json::json!({ "source": stealth_source }),
            Some(&session_id),
            self.page_timeout,
        )
        .await?;

        // Split the caller headers into the UA override (CDP sets a UA via
        // `setUserAgentOverride`, not via extra headers) and everything else.
        // A caller-supplied `User-Agent` wins over the tier default, the same
        // precedence the HTTP fetcher gives it (http_only.rs applies caller
        // headers last).
        // Present a modern UA on the CDP path too (the HTTP fetcher already does,
        // but renderers otherwise send the browser's own — often stale — UA, which
        // trips "your browser is outdated" gates). Session-scoped (so pooled
        // contexts don't leak it) and best-effort: a tier that rejects the method
        // must NOT abort an otherwise-fine render — hence `.ok()`, not `?`.
        // lightpanda rejects "Mozilla" UAs (→ `lightpanda_safe_ua`); it routes
        // Network.* → Emulation.* internally, so the method name is fine. Skip if empty.
        let effective_ua = caller_ua.as_deref().unwrap_or(&self.user_agent);
        // With an empty UA the override is still sent when a country-derived
        // language exists: Chromium treats an empty `userAgent` as "keep the
        // browser's own", so the language lands without touching the UA, and
        // the header stays in step with the script and the locale override.
        let accept_language = lang_locale.map(|l| l.cdp_accept_language());
        if !effective_ua.is_empty() || accept_language.is_some() {
            let ua: &str = if self.name == "lightpanda" {
                lightpanda_safe_ua(effective_ua)
            } else {
                effective_ua
            };
            conn.send_recv(
                "Network.setUserAgentOverride",
                ua_override_params(ua, accept_language.as_deref()),
                Some(&session_id),
                self.page_timeout,
            )
            .await
            .ok();
        }

        // Match the clock and the JS locale to the exit country as well. An
        // IP/locale mismatch is a stronger bot signal than a flagged IP, so a
        // German exit reporting a UTC clock and `navigator.language = en-US`
        // was working against the reason the country was pinned in the first
        // place. Best-effort like the two calls above: a tier that does not
        // implement `Emulation.*` (lightpanda) must not fail an otherwise-fine
        // render.
        let overrides = geo_locale
            .map(|l| {
                (
                    "Emulation.setTimezoneOverride",
                    serde_json::json!({ "timezoneId": l.timezone }),
                )
            })
            .into_iter()
            .chain(lang_locale.map(|l| {
                (
                    "Emulation.setLocaleOverride",
                    serde_json::json!({ "locale": l.primary_tag() }),
                )
            }));
        for (method, params) in overrides {
            // Best-effort, but not silent: Blink backs both overrides with
            // process-wide controllers and refuses a second one in the same
            // renderer process, so a refusal here is the signal that two
            // country-pinned renders shared a process.
            if let Err(e) = conn
                .send_recv(method, params, Some(&session_id), self.page_timeout)
                .await
            {
                tracing::debug!(renderer = %self.name, method, "locale override refused: {e}");
            }
        }

        // Forward the caller's custom request headers. These were dropped on the
        // CDP path entirely (only the HTTP tier honored them), so a documented
        // `headers` field silently did nothing on any browser render. Additive:
        // skipped when the caller passed none, so the default render sends
        // byte-identical CDP traffic. Best-effort like the UA override above.
        //
        // Scope note: `setExtraHTTPHeaders` applies to EVERY request the page
        // makes, including cross-origin subresources — the standard browser
        // behaviour (Playwright/Puppeteer `setExtraHTTPHeaders` are identical),
        // but broader than the HTTP tier, which decorates only the single main
        // fetch. A caller must therefore not put cross-origin-sensitive
        // credentials (e.g. `Authorization`) here for a browser render; the docs
        // carry this warning. Scoping to the main document would require enabling
        // `Fetch` request interception on every render (it is otherwise only on
        // for proxy-auth / blocklist), a latency cost the engine deliberately
        // avoids on the hot path.
        if !extra_headers.is_empty() {
            conn.send_recv(
                "Network.setExtraHTTPHeaders",
                serde_json::json!({ "headers": extra_headers }),
                Some(&session_id),
                self.page_timeout,
            )
            .await
            .ok();
        }

        // cf_clearance reuse (chrome family only — lightpanda can't execute a
        // Cloudflare challenge, and routes Network.* through Emulation.* so
        // setCookie/getCookies aren't reliable). Compute the cache key once;
        // inject a still-valid cached clearance cookie BEFORE navigating so
        // Cloudflare serves the real page instead of a challenge.
        //
        // The cookie is bound by Cloudflare to (IP, UA, TLS/JA3), so all three
        // are pinned: the sticky-per-host proxy keeps the IP (part of the key),
        // `effective_ua` keeps the UA (also part of the key — a caller-supplied
        // UA override must not replay a cookie solved under a different one),
        // and this IS Chrome so the JA3 matches.
        let clearance_key: Option<String> = if self.name == "lightpanda" {
            None
        } else {
            url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(|h| h.to_string()))
                .map(|host| {
                    let proxy_id = crate::REQUEST_PROXY
                        .try_with(|p| p.as_ref().map(|e| e.raw().to_string()))
                        .ok()
                        .flatten();
                    crate::clearance::ClearanceCache::key(&host, proxy_id.as_deref(), effective_ua)
                })
        };
        // Single-flight: the FIRST fetch to a cold `(host,proxy)` holds the solve
        // lock across navigate+capture, so concurrent same-key fetches (a crawl
        // hammering one host) wait for that one solve instead of stampeding
        // Cloudflare with N simultaneous challenges (which gets the IP banned).
        // A waiter that finds the cookie already cached drops the lock at once and
        // proceeds in parallel — only the actual solver serialises. (Belt-and-
        // suspenders to the per-host limiter, which already serialises at
        // politeness=1.) `_solve_guard` releases when fetch_inner returns.
        let mut _solve_guard: Option<tokio::sync::OwnedMutexGuard<()>> = None;
        let inject_cookies = match &clearance_key {
            None => None,
            Some(key) => {
                let cache = crate::clearance::clearance_cache();
                match cache.get(key) {
                    hit @ Some(_) => hit,
                    None => {
                        let guard = cache.solve_lock(key).lock_owned().await;
                        match cache.get(key) {
                            // Solved while we waited → reuse, release immediately.
                            hit @ Some(_) => hit,
                            // Cold: we're the solver — hold the lock through capture.
                            None => {
                                _solve_guard = Some(guard);
                                None
                            }
                        }
                    }
                }
            }
        };
        if let Some(cookies) = &inject_cookies {
            for c in cookies {
                let _ = conn
                    .send_recv(
                        "Network.setCookie",
                        serde_json::json!({
                            "name": c.name,
                            "value": c.value,
                            "domain": c.domain,
                            "path": c.path,
                            "secure": c.secure,
                            "httpOnly": c.http_only,
                        }),
                        Some(&session_id),
                        self.page_timeout,
                    )
                    .await;
            }
            tracing::debug!(
                url,
                count = cookies.len(),
                "injected cached clearance cookies"
            );
        }

        // Subscribe to events BEFORE navigating so we don't miss loadEventFired.
        let events_rx = conn.subscribe();

        // Credentials for the `Fetch.authRequired` pump. Priority:
        //   1. The per-request rotated proxy's own embedded `user:pass`
        //      (REQUEST_PROXY) — takes precedence so a BYOP/rotated proxy
        //      authenticates correctly.
        //   2. Otherwise the chrome_proxy tier's DataImpulse base creds, with
        //      the country suffix composed from REQUEST_COUNTRY → default → none.
        let request_proxy_auth: Option<(String, String)> = crate::REQUEST_PROXY
            .try_with(|p| p.as_ref().and_then(|e| e.auth().cloned()))
            .ok()
            .flatten();
        let effective_creds: Option<(String, String)> = request_proxy_auth.or_else(|| {
            self.proxy_auth_base.as_ref().map(|(base_user, base_pass)| {
                let req_country = crate::REQUEST_COUNTRY
                    .try_with(|c| c.clone())
                    .ok()
                    .flatten();
                let cc = req_country
                    .as_deref()
                    .or(self.default_country.as_deref())
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic()));
                match cc {
                    Some(cc) => (format!("{base_user}__cr.{cc}"), base_pass.clone()),
                    None => (base_user.clone(), base_pass.clone()),
                }
            })
        });
        let auth_active = effective_creds.is_some();

        // Enable request interception. Must be done before `Page.navigate`
        // because `Fetch.enable` pauses the document request too — the pump must
        // already be consuming `Fetch.requestPaused` by then.
        //
        // Unconditional: the pump is what validates every destination the
        // browser reaches on its own (redirects, JS navigation, iframes, XHR),
        // which the route-layer check cannot see. `intercept_active` now decides
        // only whether the ad/resource blocklist runs alongside that check.
        //
        // Patterns are ALWAYS `[*]`. Chrome rejects an empty `patterns` array
        // together with `handleAuthRequests: true` (`-32602 Can't specify empty
        // patterns with handleAuth set`), which silently broke every proxy that
        // needs auth — e.g. DataImpulse — on the Chrome path. With `[*]` the
        // browser pauses every request, so the pump MUST answer them all or the
        // page hangs.
        //
        // A failure here propagates: a tier whose destinations we cannot check
        // must not be used, and the ladder falls through to the next one.
        let intercept_active = self.intercept_active_for(url);
        {
            let mut params = serde_json::Map::new();
            params.insert(
                "patterns".into(),
                serde_json::json!([{ "urlPattern": "*" }]),
            );
            if auth_active {
                params.insert("handleAuthRequests".into(), serde_json::json!(true));
            }
            conn.send_recv(
                "Fetch.enable",
                serde_json::Value::Object(params),
                Some(&session_id),
                self.page_timeout,
            )
            .await?;
        }

        // Auto-attach child targets (out-of-process iframes, workers) so the
        // pump can enable interception on them as they appear. Without this
        // their requests never surface on our session and escape the
        // destination check entirely. `waitForDebuggerOnStart: true` because the
        // request that matters is the child's own first navigation, which would
        // otherwise outrun `Fetch.enable`; the pump resumes every child it sees,
        // and a child it cannot intercept is closed rather than resumed. Best-effort — a browser without
        // the command keeps the previous behaviour rather than failing the
        // render.
        let _ = conn
            .send_recv(
                "Target.setAutoAttach",
                serde_json::json!({
                    "autoAttach": true,
                    "waitForDebuggerOnStart": true,
                    "flatten": true,
                }),
                Some(&session_id),
                self.page_timeout,
            )
            .await;
        // Not repeated at browser scope. Service workers and shared workers are
        // not children of the page target, so this does not reach them, and
        // their `fetch` is the one remaining path that can carry a response back
        // into the page. Browser-scope auto-attach delivers those events with no
        // `sessionId`, so the pump cannot tell which render they belong to
        // without tracking browser contexts. Left as a known gap; on LightPanda
        // `--block-private-networks` covers it, on chrome it does not.

        // Network-idle tracker fed by a sibling pump (spawned below in the
        // select!). Created before the `work` future because `work` borrows
        // it for the SPA poll's idle-exit gate.
        let net_tracker = Arc::new(NetworkActivityTracker::new());

        // The work future drives navigate → wait_for_load → post-navigate work.
        // It races against the interception pump via `tokio::select!`. The
        // pump never returns; when work completes, the pump future is dropped.
        let nav_budget = self.nav_budget.min(deadline.remaining());
        let work = async {
            // Tier 0 metric M2: time Page.navigate send → loadEventFired.
            let nav_t0 = Instant::now();
            let navigate_result = conn
                .send_recv(
                    "Page.navigate",
                    serde_json::json!({ "url": url }),
                    Some(&session_id),
                    self.page_timeout,
                )
                .await?;
            if let Some(error_text) = navigate_result
                .get("errorText")
                .and_then(|value| value.as_str())
            {
                return Err(CrwError::RendererError(format!(
                    "Navigation failed: {error_text}"
                )));
            }
            let status_code =
                wait_for_page_ready(events_rx, &session_id, self.page_timeout).await?;
            crw_core::metrics::metrics()
                .chrome_navigate_seconds
                .observe(nav_t0.elapsed().as_secs_f64());
            // latency-qn: log nav→load (the load-event wait) so the per-URL tail
            // breakdown shows load-wait vs post-navigate render (debug target).
            tracing::debug!(
                target: "render_phases",
                url,
                nav_to_load_ms = nav_t0.elapsed().as_millis() as u64,
                "nav timing"
            );
            // Post-navigate phase runs inside a budget race. On budget hit
            // we attempt a partial-DOM snapshot and return `truncated = true`;
            // `single.rs` decides success on md length.
            let phase = self.post_navigate_phase(conn, &session_id, url, wait_for_ms, &net_tracker);
            let (html, truncated) = match tokio::time::timeout(nav_budget, phase).await {
                Ok(Ok(html)) => (html, false),
                Ok(Err(err)) => return Err(err),
                Err(_) => {
                    // Name the tier that actually ran out, for the same reason
                    // `budget_truncated_warning()` does: this struct drives every
                    // CDP-speaking renderer, so a hardcoded "chrome" blamed Chrome
                    // for LightPanda's much smaller budget and sent anyone reading
                    // the logs to the wrong tier.
                    tracing::info!(
                        url,
                        renderer = %self.name,
                        budget_ms = nav_budget.as_millis() as u64,
                        "nav budget hit; attempting partial snapshot"
                    );
                    let _ = conn
                        .send_recv(
                            "Page.stopLoading",
                            serde_json::json!({}),
                            Some(&session_id),
                            Duration::from_secs(1),
                        )
                        .await;
                    let html = Self::eval_html(conn, &session_id, Duration::from_secs(2))
                        .await
                        .unwrap_or_default();
                    crw_core::metrics::metrics()
                        .chrome_budget_truncated_total
                        .with_label_values(&[if html.is_empty() { "empty" } else { "ok" }])
                        .inc();
                    (html, true)
                }
            };
            // Screenshot capture runs AFTER the page-load budget race resolves,
            // with its own timeout. A full-page capture of a heavy page must not
            // be cancelled by the nav budget (which closes the WS mid-capture and
            // drops the screenshot). The session is still live here — the
            // partial-snapshot branch above uses it too.
            let screenshot = match crate::current_screenshot_req() {
                Some(req) => {
                    self.capture_screenshot(conn, &session_id, req.full_page)
                        .await
                }
                None => None,
            };
            Ok::<_, CrwError>((html, status_code, truncated, screenshot))
        };

        // Always-on XHR/fetch capture. Cheap when no JSON XHRs fire (events
        // skipped by mime/type filter). Bounded by NET_CAPTURE_MAX_BODIES and
        // NET_CAPTURE_MAX_TOTAL_BYTES so a chatty page can't OOM us.
        let captured: Arc<Mutex<Vec<CapturedNetworkResponse>>> = Arc::new(Mutex::new(Vec::new()));
        let cap_pump =
            run_network_capture_pump(conn, conn.subscribe(), captured.clone(), &session_id);

        // Network-idle pump (tracker constructed earlier). Fed by `Network.*`
        // events; SPA poll consults the tracker for an early exit when XHR
        // traffic settles before body innerText hits the threshold.
        let idle_pump = run_network_idle_pump(conn.subscribe(), net_tracker.clone(), &session_id);

        // A screenshot must render images/media/fonts; the default blocklist
        // strips them (fine for markdown, but leaves broken-image placeholders
        // in the capture). Relax resource-type blocking for screenshot
        // requests; host blocks (analytics/trackers) still apply.
        let screenshot_blocklist;
        let blocklist: &Blocklist = if crate::current_screenshot_req().is_some() {
            screenshot_blocklist = self.blocklist.for_screenshot();
            &screenshot_blocklist
        } else {
            &self.blocklist
        };

        // The intercept pump always runs; only the blocklist half is optional.
        let pump_blocklist = intercept_active.then_some(blocklist);
        let outbound_ctx = OutboundCtx {
            memo: Arc::new(StdMutex::new(HashMap::new())),
            render_limit: tokio::sync::Semaphore::new(PER_RENDER_RESOLVE_LIMIT),
            // A verdict that arrives after the request is already lost helps
            // nobody, so the check is bounded by the smallest thing that can end
            // it: the caller's remaining deadline, the tier's outer timeout, and
            // the post-navigate budget. Using `page_timeout` alone would have
            // given chrome 30s, well past the point the render is abandoned.
            // Accepted cost: on a tier with a small ceiling (LightPanda is
            // 2500ms) a cold lookup that queues can expire and drop that one
            // subresource, counted as `outbound_unresolved`.
            budget: self
                .nav_budget
                .min(self.page_timeout)
                .min(deadline.remaining()),
            doc_host: url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_default(),
            unresolved: std::sync::atomic::AtomicBool::new(false),
        };
        let outstanding: Outstanding = Arc::new(StdMutex::new(std::collections::HashSet::new()));
        let intercept_pump = run_intercept_pump(
            conn,
            conn.subscribe(),
            pump_blocklist,
            &session_id,
            &outbound_ctx,
            &outstanding,
        );
        let outcome = if auth_active {
            let auth_pump =
                run_auth_pump(conn, conn.subscribe(), effective_creds.clone(), &session_id);
            tokio::select! {
                biased;
                res = work => res,
                _ = intercept_pump => Err(CrwError::RendererError(
                    "interception pump exited unexpectedly".into(),
                )),
                _ = auth_pump => Err(CrwError::RendererError(
                    "auth pump exited unexpectedly".into(),
                )),
                _ = cap_pump => Err(CrwError::RendererError(
                    "network capture pump exited unexpectedly".into(),
                )),
                _ = idle_pump => Err(CrwError::RendererError(
                    "network idle pump exited unexpectedly".into(),
                )),
            }
        } else {
            tokio::select! {
                biased;
                res = work => res,
                _ = intercept_pump => Err(CrwError::RendererError(
                    "interception pump exited unexpectedly".into(),
                )),
                _ = cap_pump => Err(CrwError::RendererError(
                    "network capture pump exited unexpectedly".into(),
                )),
                _ = idle_pump => Err(CrwError::RendererError(
                    "network idle pump exited unexpectedly".into(),
                )),
            }
        };

        // Any request whose check was still running when the pump was dropped is
        // still paused. Failing them explicitly releases the browser's own state
        // now rather than at target close, and makes the denial observable; the
        // target close behind us is the backstop, not the mechanism.
        let leftover: Vec<(String, String)> = outstanding
            .lock()
            .map(|o| o.iter().cloned().collect())
            .unwrap_or_default();
        if !leftover.is_empty() {
            use futures::stream::{FuturesUnordered, StreamExt};
            // Concurrent, under one overall cap. The pump has no in-flight limit
            // by design, so this set is unbounded, and failing them serially
            // would run past the caller's remaining deadline and let the outer
            // timeout fire in the middle of cleanup.
            let mut pending: FuturesUnordered<_> = leftover
                .into_iter()
                .map(|(sess, req_id)| async move {
                    let _ = conn
                        .send_recv(
                            "Fetch.failRequest",
                            serde_json::json!({
                                "requestId": req_id,
                                "errorReason": "BlockedByClient",
                            }),
                            Some(&sess),
                            Duration::from_secs(1),
                        )
                        .await;
                })
                .collect();
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                while pending.next().await.is_some() {}
            })
            .await;
        }

        // Deliberately no `Fetch.disable`. It auto-continues still-paused
        // requests, and the ones it would release are exactly the ones the guard
        // never got to judge: a `Fetch.requestPaused` still sitting in the
        // broadcast ring when the pump was dropped, or one lost to a `Lagged`
        // drop, is in no set we can fail explicitly. Disabling would let those
        // out unchecked, which is the hole this guard exists to close.
        //
        // Nothing leaks by leaving it on. `Fetch.enable` was sent against this
        // session, every render creates its own target and therefore its own
        // session, and the caller always closes that target: the legacy path
        // right after `fetch_inner` returns, the pooled path in `release()`,
        // which skips the close only in the cases where no target was ever
        // created. Anything still paused dies with the target, which is the
        // fail-closed outcome.
        // A denial the guard could not decide is our failure, not the origin's.
        // Both tiers resolve through the same resolver, so leaving this as a
        // navigation failure lets a DNS brown-out be reported as an unreachable
        // target: refunded, and invisible to the 5xx watchdog.
        let outcome = match outcome {
            Err(err)
                if outbound_ctx
                    .unresolved
                    .load(std::sync::atomic::Ordering::Relaxed) =>
            {
                tracing::warn!(url, tier = %self.name, "outbound destination check unavailable");
                Err(CrwError::RendererError(format!(
                    "outbound destination check unavailable ({err})"
                )))
            }
            other => other,
        };

        // Capture final URL after any redirects, before tearing down the target.
        // Best-effort: failures map to None and never propagate.
        let final_href = match outcome.as_ref() {
            Ok(_) => Self::eval_href(conn, &session_id, Duration::from_secs(2)).await,
            Err(_) => None,
        };

        // Capture clearance cookies on success, before the target is torn down.
        // `put` is a no-op when no clearance cookie is present, so a still-blocked
        // page caches nothing. This also self-heals a stale injected cookie: when
        // it was rejected, Chrome solves the fresh challenge and we capture the
        // new cookie here. Best-effort — never affects the fetch result.
        if let Some(key) = &clearance_key
            && outcome.is_ok()
        {
            let cookies = Self::capture_clearance_cookies(conn, &session_id, url).await;
            if !cookies.is_empty() {
                tracing::debug!(url, count = cookies.len(), "captured clearance cookies");
            }
            crate::clearance::clearance_cache().put(key, cookies);
        }

        // Target close is the caller's responsibility (pool's release() owns
        // it via the recorded target_id; legacy fetch_with_ws closes after
        // fetch_inner returns via its Cell-captured id).

        let (html, status_code, truncated, screenshot) = outcome?;

        if html.is_empty() && truncated {
            return Err(CrwError::Timeout(nav_budget.as_millis() as u64));
        }

        if !truncated
            && wait_for_ms.is_none()
            && crate::detector::looks_like_loading_placeholder(&html)
        {
            tracing::debug!(url, "Placeholder still present after stability poll");
        }

        let captured_drained = std::mem::take(&mut *captured.lock().await);
        Ok((
            html,
            status_code,
            truncated,
            final_href,
            captured_drained,
            screenshot,
            target_id,
        ))
    }

    /// Post-navigate work: SPA selector wait, eval HTML, placeholder
    /// stability poll, challenge retry. Lives inside a `nav_budget` race;
    /// see `fetch_inner` for the timeout/snapshot fallback path.
    async fn post_navigate_phase(
        &self,
        conn: &CdpConnection,
        session_id: &str,
        url: &str,
        wait_for_ms: Option<u64>,
        net: &NetworkActivityTracker,
    ) -> CrwResult<String> {
        // Phase-timing measurement (latency-qn): which post-navigate phase eats
        // the render time? Emitted on the `render_phases` debug target (off in
        // prod RUST_LOG=info; enable with RUST_LOG=render_phases=debug for bench).
        let _pt0 = Instant::now();
        let mut t_stability_ms: u64 = 0;
        let mut t_challenge_ms: u64 = 0;
        // 2.5. Best-effort consent / CMP dismissal. Cookie banners can both
        // hide content behind an overlay and inflate `body.innerText` past
        // the SPA-readiness threshold prematurely (the banner copy alone
        // can clear 800 chars). Auto-mode only: if the caller pinned a
        // wait, they own the timing.
        if wait_for_ms.is_none() {
            Self::dismiss_consent(conn, session_id).await;
        }

        // 3. Wait for initial JS work. Caller-supplied `wait_for_ms` wins —
        // sleep that long. Otherwise jump straight to the SPA selector poll;
        // the poll exits in ~200ms on static pages where `main`/`article`/etc.
        // are already mounted, and waits up to SPA_SELECTOR_MAX_MS for SPAs
        // that hydrate after `loadEventFired`.
        let _sel_t = Instant::now();
        if let Some(wait) = wait_for_ms {
            tokio::time::sleep(Duration::from_millis(wait)).await;
        } else if !Self::wait_for_spa_selector(
            conn,
            session_id,
            self.page_timeout,
            net,
            self.spa_selector_max,
            self.fast_ready,
        )
        .await
        {
            tracing::debug!(url, "SPA selector poll exhausted budget");
        }
        let t_selector_ms = _sel_t.elapsed().as_millis() as u64;

        // 4. Get rendered HTML.
        let mut html = Self::eval_html(conn, session_id, self.page_timeout).await?;

        // 4b. SPA loading placeholder → poll for content stability.
        if wait_for_ms.is_none() && crate::detector::looks_like_loading_placeholder(&html) {
            tracing::info!(
                url,
                "Loading placeholder detected, polling for content stability"
            );
            let _stab_t = Instant::now();
            match Self::poll_until_content_stable(conn, session_id, self.page_timeout).await {
                Ok(stable) => html = stable,
                Err(e) => tracing::warn!("Content stability polling failed: {e}"),
            }
            t_stability_ms = _stab_t.elapsed().as_millis() as u64;
        }

        // 4c. Auto-mode lazy-load pass: scroll viewport-by-viewport so
        // infinite-scroll feeds, lazy images, and below-the-fold hydration
        // appear in the snapshot. Gated:
        // - skip when caller pinned wait_for_ms (explicit budget)
        // - skip on challenge / placeholder shells (nothing to scroll)
        // - skip when HTML is already large (almost certainly fully-rendered)
        //   unless we see explicit lazy-load markers
        // Stricter gate: only scroll when explicit lazy-load markers exist.
        // Empirically, scrolling healthy pages adds latency without lift, and
        // pushes some heavy renders past the deadline.
        let has_lazy_markers = html.contains("loading=\"lazy\"")
            || html.contains("data-src=")
            || html.contains("infinite-scroll")
            || html.contains("lazy-load");
        if wait_for_ms.is_none()
            && has_lazy_markers
            && html.len() < AUTO_SCROLL_HTML_SIZE_LIMIT
            && !is_challenge_page(&html)
            && !crate::detector::looks_like_loading_placeholder(&html)
        {
            let scroll_timeout = Duration::from_millis(AUTO_SCROLL_BUDGET_MS);
            let scroll_timeout = scroll_timeout.min(self.page_timeout);
            tokio::time::timeout(
                scroll_timeout,
                Self::auto_scroll(conn, session_id, scroll_timeout),
            )
            .await
            .ok();
            // Re-snapshot after scrolling so any lazy-loaded content is captured.
            html = Self::eval_html(conn, session_id, self.page_timeout).await?;
        }

        // 4d. Click-to-reveal pass: expand collapsed accordions / "load more"
        // CTAs that hide article body behind a click. Gated to pages that
        // actually have markers — pure content pages are skipped.
        let has_reveal_markers = html.contains(r#"aria-expanded="false""#)
            || html.contains("load-more")
            || html.contains("show-more")
            || {
                let lower = html.to_ascii_lowercase();
                lower.contains(">load more<")
                    || lower.contains(">show more<")
                    || lower.contains(">read more<")
                    || lower.contains(">show full<")
                    || lower.contains(">view all<")
            };
        if wait_for_ms.is_none()
            && has_reveal_markers
            && html.len() < AUTO_SCROLL_HTML_SIZE_LIMIT
            && !is_challenge_page(&html)
            && !crate::detector::looks_like_loading_placeholder(&html)
        {
            let click_timeout = Duration::from_millis(AUTO_CLICK_BUDGET_MS);
            let click_timeout = click_timeout.min(self.page_timeout);
            tokio::time::timeout(
                click_timeout,
                Self::auto_click_reveal(conn, session_id, click_timeout),
            )
            .await
            .ok();
            html = Self::eval_html(conn, session_id, self.page_timeout).await?;
        }

        // 5. Challenge retry loop for Cloudflare/anti-bot interstitials.
        // Bounded by `self.challenge_max_retries` (config); 0 disables it so
        // anti-bot recovery falls to the stealth/auto-egress tier instead of
        // burning the deadline here (the Firecrawl/Spider approach).
        if self.challenge_max_retries > 0 && is_challenge_page(&html) {
            let _chal_t = Instant::now();
            tracing::info!(url, "Challenge page detected, waiting for auto-resolve");
            for attempt in 1..=self.challenge_max_retries {
                tokio::time::sleep(Duration::from_millis(CHALLENGE_POLL_INTERVAL_MS)).await;
                html = Self::eval_html(conn, session_id, self.page_timeout).await?;
                if !is_challenge_page(&html) {
                    tracing::info!(url, attempt, "Challenge cleared");
                    break;
                }
                tracing::debug!(url, attempt, "Challenge still active, retrying");
            }
            t_challenge_ms = _chal_t.elapsed().as_millis() as u64;
        }

        // latency-qn render-phase breakdown (debug target; off in prod).
        let total_ms = _pt0.elapsed().as_millis() as u64;
        tracing::debug!(
            target: "render_phases",
            url,
            total_ms,
            selector_ms = t_selector_ms,
            stability_ms = t_stability_ms,
            challenge_ms = t_challenge_ms,
            other_ms = total_ms.saturating_sub(t_selector_ms + t_stability_ms + t_challenge_ms),
            html_len = html.len(),
            "render phases"
        );

        Ok(html)
    }

    /// Measure the content box (Page.getLayoutMetrics) and decide whether a
    /// full-page capture must be clipped. `None` => page fits, take the plain
    /// full-page shot. `Some((w,h))` => clip to these CSS-px dims. On measure
    /// failure returns None (degrade to today's behavior).
    // ponytail: getLayoutMetrics-fail => plain full-page path (rare; unmeasurable pages behave exactly as before the fix)
    async fn full_page_clip(&self, conn: &CdpConnection, session_id: &str) -> Option<(f64, f64)> {
        let m = conn
            .send_recv(
                "Page.getLayoutMetrics",
                serde_json::json!({}),
                Some(session_id),
                self.page_timeout,
            )
            .await
            .ok()?;
        // clip coords are CSS px => prefer cssContentSize; contentSize is the older-Chrome fallback.
        let size = m.get("cssContentSize").or_else(|| m.get("contentSize"))?;
        let w = size.get("width").and_then(|v| v.as_f64())?;
        let h = size.get("height").and_then(|v| v.as_f64())?;
        let clip = screenshot_clip(w, h, screenshot_max_height_px());
        if clip.is_some() {
            tracing::warn!(
                content_height = h,
                cap = screenshot_max_height_px(),
                "full-page screenshot clipped to height cap (#161 OOM guard)"
            );
        }
        clip
    }

    /// Capture a PNG via CDP `Page.captureScreenshot` with its OWN timeout,
    /// independent of the page-load `nav_budget`. This MUST run outside the
    /// nav-budget race: a full-page capture of a heavy/tall page can take
    /// several seconds, and if it competes with (and is cancelled by) the
    /// budget the in-flight WS request dies ("WS closed") and the screenshot is
    /// silently dropped. Best-effort: returns `None` (and logs) on failure so
    /// the scrape still returns its content. Raw base64 is kept undecoded;
    /// `single.rs` wraps the `data:` URL prefix.
    async fn capture_screenshot(
        &self,
        conn: &CdpConnection,
        session_id: &str,
        full_page: bool,
    ) -> Option<String> {
        let params = if full_page {
            match self.full_page_clip(conn, session_id).await {
                Some((w, h)) => serde_json::json!({
                    "format": "png",
                    "fromSurface": true,
                    "captureBeyondViewport": true,
                    "clip": { "x": 0, "y": 0, "width": w, "height": h, "scale": 1 },
                }),
                None => serde_json::json!({
                    "format": "png",
                    "captureBeyondViewport": true,
                    "fromSurface": true,
                }),
            }
        } else {
            serde_json::json!({
                "format": "png",
                "captureBeyondViewport": false,
                "fromSurface": true,
            })
        };
        match conn
            .send_recv(
                "Page.captureScreenshot",
                params,
                Some(session_id),
                self.page_timeout,
            )
            .await
        {
            Ok(resp) => resp
                .get("data")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            Err(e) => {
                tracing::warn!("Page.captureScreenshot failed: {e}");
                None
            }
        }
    }
}

/// Pure decision: does the current poll tick indicate the rendered page has
/// stabilised? Returns `false` on the first tick (`prev_len == 0`) so that at
/// least two observations are required. `placeholder_gone` must be `true`
/// (the rendered HTML no longer looks like a loading placeholder).
///
/// Size tolerance is 5% of `prev_len` with a 500-byte floor, so noise from
/// small DOM updates (timestamps, counters) does not reset stability.
fn is_content_stable(prev_len: u64, curr_len: u64, placeholder_gone: bool) -> bool {
    if prev_len == 0 || !placeholder_gone {
        return false;
    }
    let tolerance = (prev_len / 20).max(500);
    curr_len.abs_diff(prev_len) <= tolerance
}

/// Pure decision for a full-page capture: given the page content box and the
/// height cap, return `Some((width, cap))` when the page is TALLER than the cap
/// (=> must clip to avoid OOMing Chrome) or `None` when it fits (=> keep the
/// plain captureBeyondViewport shot, byte-identical to before).
fn screenshot_clip(content_w: f64, content_h: f64, max_h: f64) -> Option<(f64, f64)> {
    if content_h > max_h {
        Some((content_w, max_h))
    } else {
        None
    }
}

/// Pure decision: does the SPA poll tick indicate the page is ready to
/// snapshot? `text_len` is the body innerText length returned from the JS
/// eval, with `-1` signaling "selector not yet mounted". Threshold matches
/// [`SPA_BODY_TEXT_MIN_CHARS`].
fn is_spa_text_ready(text_len: i64) -> bool {
    text_len >= SPA_BODY_TEXT_MIN_CHARS as i64
}

#[cfg(test)]
mod tests {
    use super::{
        CdpRenderer, CrwError, STEALTH_DEFAULT_LANGUAGES, STEALTH_JS, build_auth_response,
        geo_accept_language, is_content_stable, is_proxy_tunnel_error, language_locale,
        lightpanda_safe_ua, outbound_block_label, screenshot_clip, split_caller_headers,
        ua_override_params,
    };
    use std::collections::HashMap;
    use std::time::Duration;

    #[test]
    fn challenge_reserve_follows_the_configured_retry_count() {
        use crate::cdp::{
            CHALLENGE_MAX_RETRIES, CHALLENGE_POLL_INTERVAL_MS, CdpRenderer, challenge_retry_budget,
        };
        use std::time::Duration;
        // The outer CDP timeout used to reserve CHALLENGE_MAX_RETRIES (3) poll
        // intervals no matter what the deployment configured. Prod runs 1, so
        // every CDP tier carried 6s of budget the loop could never spend, and
        // that padding propagated into the auto-extended request deadline.
        assert_eq!(challenge_retry_budget(0), Duration::ZERO);
        assert_eq!(
            challenge_retry_budget(1),
            Duration::from_millis(CHALLENGE_POLL_INTERVAL_MS)
        );
        // A self-hoster who leaves `chrome_challenge_max_retries` unset gets
        // CHALLENGE_MAX_RETRIES at construction, so the full reserve still has
        // to be available — under-reserving there would turn a legitimately
        // slow-clearing challenge into a premature timeout.
        assert_eq!(
            challenge_retry_budget(CHALLENGE_MAX_RETRIES),
            Duration::from_millis(CHALLENGE_POLL_INTERVAL_MS * u64::from(CHALLENGE_MAX_RETRIES))
        );
        assert_eq!(
            CdpRenderer::new("chrome", "ws://x/", 1000, 1).challenge_max_retries,
            CHALLENGE_MAX_RETRIES,
            "an unconfigured renderer must still default to the full retry count"
        );
    }

    #[test]
    fn budget_truncated_warning_names_the_tier_that_ran_out() {
        // One struct drives every CDP tier, so a hardcoded string reported
        // LightPanda's truncation as Chrome's and sent debuggers to the wrong
        // renderer. Chrome's wording is unchanged for existing consumers.
        let chrome = CdpRenderer::new("chrome", "ws://x/", 1000, 1);
        let lp = CdpRenderer::new("lightpanda", "ws://x/", 1000, 1);
        assert_eq!(chrome.budget_truncated_warning(), "chrome_budget_truncated");
        assert_eq!(lp.budget_truncated_warning(), "lightpanda_budget_truncated");
    }

    #[test]
    fn pool_gate_reflects_configured_reserved_interactive_renders() {
        // Guards the FULL config→PoolCfg→with_pool threading, not just the
        // resolver arithmetic: a configured reserve must actually reach the pool
        // gate. If the `cdp.rs` resolve call regressed to `None`, the first
        // assert (expecting 2) would read 6 and fail.
        use crate::browser_pool::PoolCfg;
        // Some(6) on a size-8 pool → batch gate = 8 - 6 = 2.
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 8).with_pool(PoolCfg {
            size: 8,
            reserved_interactive_renders: Some(6),
            ..Default::default()
        });
        assert_eq!(
            r.pool_batch_gate.as_ref().unwrap().available(),
            2,
            "pool gate must use the configured reserve (8-6=2), not the default"
        );
        // None → pool/4 = 2 → batch gate = 8 - 2 = 6 (default preserved).
        let r2 = CdpRenderer::new("chrome", "ws://x/", 1000, 8).with_pool(PoolCfg {
            size: 8,
            reserved_interactive_renders: None,
            ..Default::default()
        });
        assert_eq!(r2.pool_batch_gate.as_ref().unwrap().available(), 6);
    }

    #[test]
    fn proxy_tunnel_error_matches_connect_failures() {
        // The empirical NO_RAY case: DataImpulse refuses the CONNECT and Chrome
        // surfaces ERR_TUNNEL_CONNECTION_FAILED on Page.navigate.
        assert!(is_proxy_tunnel_error(&CrwError::RendererError(
            "Navigation failed: net::ERR_TUNNEL_CONNECTION_FAILED".into()
        )));
        assert!(is_proxy_tunnel_error(&CrwError::RendererError(
            "Navigation failed: net::ERR_PROXY_CONNECTION_FAILED".into()
        )));
    }

    #[test]
    fn proxy_tunnel_error_ignores_non_connect_failures() {
        // Target/DNS/timeout errors must NOT trigger the country retry — a
        // different country exit would not fix them, and matching them would
        // mask real failures.
        assert!(!is_proxy_tunnel_error(&CrwError::RendererError(
            "Navigation failed: net::ERR_NAME_NOT_RESOLVED".into()
        )));
        assert!(!is_proxy_tunnel_error(&CrwError::RendererError(
            "Empty HTML from CDP renderer".into()
        )));
        assert!(!is_proxy_tunnel_error(&CrwError::Timeout(30_000)));
    }

    #[test]
    fn auth_response_provides_credentials_when_creds_set() {
        let v = build_auth_response("req-1", Some(("abc__cr.us", "pw")));
        assert_eq!(v["requestId"], "req-1");
        assert_eq!(v["authChallengeResponse"]["response"], "ProvideCredentials");
        assert_eq!(v["authChallengeResponse"]["username"], "abc__cr.us");
        assert_eq!(v["authChallengeResponse"]["password"], "pw");
    }

    #[test]
    fn auth_response_cancels_when_no_creds() {
        let v = build_auth_response("req-2", None);
        assert_eq!(v["authChallengeResponse"]["response"], "CancelAuth");
        assert!(v["authChallengeResponse"].get("username").is_none());
        assert!(v["authChallengeResponse"].get("password").is_none());
    }

    #[test]
    fn auth_response_no_password_leak_on_cancel() {
        // Sanity: a CancelAuth payload never carries credentials, even if the
        // caller mistakenly passes some out-of-band data. (Defense-in-depth
        // smoke test — guards against future shape changes.)
        let v = build_auth_response("req-3", None);
        let s = serde_json::to_string(&v).unwrap();
        assert!(!s.contains("\"password\""));
        assert!(!s.contains("\"username\""));
    }

    #[test]
    fn first_tick_never_stable() {
        assert!(!is_content_stable(0, 0, true));
        assert!(!is_content_stable(0, 10_000, true));
    }

    #[test]
    fn identical_sizes_are_stable_when_placeholder_gone() {
        assert!(is_content_stable(5_000, 5_000, true));
    }

    #[test]
    fn placeholder_still_present_blocks_stability() {
        assert!(!is_content_stable(5_000, 5_000, false));
    }

    #[test]
    fn small_delta_within_tolerance_is_stable() {
        assert!(is_content_stable(10_000, 10_400, true));
    }

    #[test]
    fn large_delta_outside_tolerance_is_unstable() {
        assert!(!is_content_stable(10_000, 12_000, true));
    }

    #[test]
    fn small_page_uses_500_byte_floor() {
        assert!(is_content_stable(100, 450, true));
    }

    #[test]
    fn shrink_past_tolerance_is_unstable() {
        assert!(!is_content_stable(10_000, 5_000, true));
    }

    use super::{SPA_BODY_TEXT_MIN_CHARS, is_spa_text_ready};

    #[test]
    fn spa_not_ready_when_selector_missing() {
        assert!(!is_spa_text_ready(-1));
    }

    #[test]
    fn spa_not_ready_when_text_below_threshold() {
        assert!(!is_spa_text_ready(0));
        assert!(!is_spa_text_ready(SPA_BODY_TEXT_MIN_CHARS as i64 - 1));
    }

    #[test]
    fn spa_ready_at_or_above_threshold() {
        assert!(is_spa_text_ready(SPA_BODY_TEXT_MIN_CHARS as i64));
        assert!(is_spa_text_ready(50_000));
    }

    use super::NetworkActivityTracker;

    #[test]
    fn tracker_starts_idle_after_quiet_period() {
        let t = NetworkActivityTracker::new();
        assert!(t.is_idle(0));
    }

    #[test]
    fn tracker_not_idle_with_inflight_request() {
        let t = NetworkActivityTracker::new();
        t.record_request_start();
        assert!(!t.is_idle(0));
        t.record_request_end();
        assert!(t.is_idle(0));
    }

    #[test]
    fn tracker_not_idle_during_quiet_period() {
        let t = NetworkActivityTracker::new();
        t.record_request_start();
        t.record_request_end();
        // Just-ended; quiet_ms=10_000 won't have elapsed yet.
        assert!(!t.is_idle(10_000));
    }

    #[test]
    fn tracker_treats_negative_inflight_as_idle() {
        let t = NetworkActivityTracker::new();
        // Simulate a `loadingFinished` whose `requestWillBeSent` was missed
        // during pump startup. Counter goes -1, but no real work is in
        // flight — quiet-period idle should still hold.
        t.record_request_end();
        assert!(t.is_idle(0));
    }

    use super::is_browserless_direct_ws;

    #[test]
    fn browserless_token_url_is_direct_ws() {
        assert!(is_browserless_direct_ws(
            "wss://chrome.browserless.io/chromium?token=abc"
        ));
        assert!(is_browserless_direct_ws("wss://example.com/cdp?token=xyz"));
    }

    #[test]
    fn browserless_named_path_is_direct_ws() {
        assert!(is_browserless_direct_ws("wss://x.example/chromium"));
        assert!(is_browserless_direct_ws("wss://x.example/firefox"));
        assert!(is_browserless_direct_ws("wss://x.example/webkit"));
    }

    #[test]
    fn plain_lightpanda_url_is_not_direct_ws() {
        assert!(!is_browserless_direct_ws("ws://lightpanda:9222"));
        assert!(!is_browserless_direct_ws("ws://chrome:9222"));
    }

    use super::is_capturable_mime;

    #[test]
    fn capturable_mime_recognises_json_variants() {
        assert!(is_capturable_mime("application/json"));
        assert!(is_capturable_mime("application/json; charset=utf-8"));
        assert!(is_capturable_mime("application/ld+json"));
        assert!(is_capturable_mime("application/vnd.api+json"));
        assert!(is_capturable_mime("text/json"));
        assert!(is_capturable_mime("text/plain"));
    }

    #[test]
    fn capturable_mime_rejects_uninteresting_types() {
        assert!(!is_capturable_mime("text/html"));
        assert!(!is_capturable_mime("image/png"));
        assert!(!is_capturable_mime("application/octet-stream"));
        assert!(!is_capturable_mime("text/css"));
        assert!(!is_capturable_mime("application/javascript"));
        assert!(!is_capturable_mime(""));
    }

    #[test]
    fn user_agent_default_empty_and_builder_sets_it() {
        // Default = empty → fetch_inner skips the override (browser default).
        let r = CdpRenderer::new("chrome", "ws://127.0.0.1:9222", 1000, 1);
        assert_eq!(r.user_agent, "", "default UA must be empty (no override)");
        // Builder threads the effective UA through so CDP matches the HTTP path.
        let ua = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36";
        let r = r.with_user_agent(ua);
        assert_eq!(r.user_agent, ua);
    }

    #[test]
    fn lightpanda_safe_ua_strips_mozilla_keeps_chrome() {
        let full = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                    (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36";
        let safe = lightpanda_safe_ua(full);
        // lightpanda's validateUserAgent rejects ANY "Mozilla" → must be gone.
        assert!(!safe.to_ascii_lowercase().contains("mozilla"));
        // ...but the Chrome version token that UA gates check must survive.
        assert!(safe.contains("Chrome/150"));
        // A UA without the prefix is returned unchanged (no double-strip).
        assert_eq!(lightpanda_safe_ua("Chrome/150.0.0.0"), "Chrome/150.0.0.0");
    }

    #[test]
    fn stealth_js_carries_the_default_languages_literal() {
        // The country path substitutes this exact substring. If a stealth-script
        // edit changes the spacing or quoting, the substitution silently becomes
        // a no-op, so pin it here.
        assert_eq!(
            STEALTH_JS.matches(STEALTH_DEFAULT_LANGUAGES).count(),
            1,
            "STEALTH_JS must contain exactly one {STEALTH_DEFAULT_LANGUAGES} literal"
        );
    }

    #[test]
    fn ua_override_params_without_locale() {
        // No country -> the payload is byte-identical to the pre-change one.
        let p = ua_override_params("MyAgent/1.0", None);
        assert_eq!(p, serde_json::json!({ "userAgent": "MyAgent/1.0" }));
        assert!(p.get("acceptLanguage").is_none());
    }

    #[test]
    fn ua_override_params_with_locale() {
        let p = ua_override_params("MyAgent/1.0", Some("de-DE,de,en"));
        assert_eq!(p["userAgent"], "MyAgent/1.0");
        assert_eq!(p["acceptLanguage"], "de-DE,de,en");
    }

    #[test]
    fn language_locale_yields_to_caller_header() {
        let de = crate::locale::locale_for_country("de");
        let empty = serde_json::Map::new();
        assert_eq!(language_locale(de, &empty), de);
        assert_eq!(language_locale(None, &empty), None);
        let mut caller = serde_json::Map::new();
        caller.insert(
            "Accept-Language".to_string(),
            serde_json::Value::String("fr-FR".to_string()),
        );
        assert_eq!(language_locale(de, &caller), None);
    }

    #[test]
    fn geo_accept_language_yields_to_caller_header() {
        let de = crate::locale::locale_for_country("de");
        let empty = serde_json::Map::new();

        // No locale resolved (no country, or the lightpanda tier) -> nothing
        // derived, whatever the headers are.
        assert!(geo_accept_language(None, &empty).is_none());

        assert_eq!(
            geo_accept_language(de, &empty).as_deref(),
            Some("de-DE,de,en"),
            "a pinned country must drive the header, in plain tag form"
        );

        // A caller who set the header keeps it, match is case-insensitive.
        for name in ["accept-language", "Accept-Language"] {
            let mut caller = serde_json::Map::new();
            caller.insert(
                name.to_string(),
                serde_json::Value::String("fr-FR".to_string()),
            );
            assert!(geo_accept_language(de, &caller).is_none(), "{name}");
        }

        // An unrelated caller header does not suppress it.
        let mut caller = serde_json::Map::new();
        caller.insert(
            "X-Probe".to_string(),
            serde_json::Value::String("v".to_string()),
        );
        assert_eq!(
            geo_accept_language(de, &caller).as_deref(),
            Some("de-DE,de,en")
        );
    }

    #[test]
    fn split_caller_headers_pulls_out_user_agent() {
        // A caller User-Agent must go to setUserAgentOverride, not into the
        // extra-headers payload, and the match is case-insensitive.
        let mut h = HashMap::new();
        h.insert("user-agent".to_string(), "MyAgent/1.0".to_string());
        h.insert("X-Probe".to_string(), "v".to_string());
        let (ua, extra) = split_caller_headers(&h);
        assert_eq!(ua.as_deref(), Some("MyAgent/1.0"));
        assert_eq!(extra.get("X-Probe").and_then(|v| v.as_str()), Some("v"));
        assert!(!extra.contains_key("user-agent"));
        assert_eq!(extra.len(), 1);
    }

    #[test]
    fn split_caller_headers_blank_ua_is_absent() {
        // A blank caller UA must not suppress the tier default: it becomes None
        // and does not land in the extra headers either.
        let mut h = HashMap::new();
        h.insert("User-Agent".to_string(), "   ".to_string());
        let (ua, extra) = split_caller_headers(&h);
        assert!(ua.is_none());
        assert!(extra.is_empty());
    }

    #[test]
    fn split_caller_headers_empty_and_no_ua() {
        // No headers → no UA, empty payload (the render skips both CDP calls).
        let (ua, extra) = split_caller_headers(&HashMap::new());
        assert!(ua.is_none());
        assert!(extra.is_empty());

        // Headers but no User-Agent → all of them are extra headers.
        let mut h = HashMap::new();
        h.insert("Accept-Language".to_string(), "de".to_string());
        h.insert("Cookie".to_string(), "a=b".to_string());
        let (ua, extra) = split_caller_headers(&h);
        assert!(ua.is_none());
        assert_eq!(extra.len(), 2);
        assert_eq!(
            extra.get("Accept-Language").and_then(|v| v.as_str()),
            Some("de")
        );
    }

    fn test_ctx() -> super::OutboundCtx {
        super::OutboundCtx {
            memo: Default::default(),
            render_limit: tokio::sync::Semaphore::new(super::PER_RENDER_RESOLVE_LIMIT),
            budget: std::time::Duration::from_secs(5),
            doc_host: String::new(),
            unresolved: std::sync::atomic::AtomicBool::new(false),
        }
    }

    #[tokio::test]
    async fn outbound_guard_rejects_internal_destinations() {
        let ctx = test_ctx();
        // Literal addresses the route-layer check would have rejected, arriving
        // here because a redirect or in-page navigation produced them.
        assert!(
            outbound_block_label("http://169.254.169.254/latest/meta-data/", &ctx)
                .await
                .is_some()
        );
        assert!(
            outbound_block_label("http://10.0.0.1/", &ctx)
                .await
                .is_some()
        );
        assert!(
            outbound_block_label("http://192.168.1.1/admin", &ctx)
                .await
                .is_some()
        );
        assert!(
            outbound_block_label("http://[::1]:8080/", &ctx)
                .await
                .is_some()
        );
        assert!(
            outbound_block_label("http://localhost:5432/", &ctx)
                .await
                .is_some()
        );
        // Unparseable is anomalous; fail closed.
        assert!(outbound_block_label("not a url", &ctx).await.is_some());
    }

    #[tokio::test]
    async fn outbound_guard_allows_ordinary_and_non_network_requests() {
        let ctx = test_ctx();
        // No assertion on a real public host: that path resolves DNS and the
        // check fails closed, so it would fail on an offline runner rather than
        // testing anything. The reject cases above are all literal or
        // deny-listed hosts and need no lookup.
        //
        // Never reaches a socket, and `validate_safe_url` would reject the
        // scheme, so it has to short-circuit before that check.
        assert!(
            outbound_block_label("data:image/png;base64,iVBORw0KGgo=", &ctx)
                .await
                .is_none()
        );
        assert!(
            outbound_block_label("blob:https://example.com/1234", &ctx)
                .await
                .is_none()
        );
    }

    #[test]
    fn screenshot_clip_caps_only_when_taller() {
        // Fits under the cap => no clip, plain full-page shot is byte-identical.
        assert_eq!(screenshot_clip(1280.0, 5000.0, 15000.0), None);
        // Over the cap => clip to the cap height, width preserved.
        assert_eq!(
            screenshot_clip(1280.0, 40000.0, 15000.0),
            Some((1280.0, 15000.0))
        );
    }

    #[test]
    fn screenshot_clip_exactly_at_cap_is_not_clipped() {
        // `>` not `>=`: a page exactly as tall as the cap is left alone.
        assert_eq!(screenshot_clip(1280.0, 15000.0, 15000.0), None);
    }

    #[test]
    fn screenshot_clip_one_px_over_cap_clips() {
        assert_eq!(
            screenshot_clip(1280.0, 15000.1, 15000.0),
            Some((1280.0, 15000.0))
        );
    }

    #[test]
    fn screenshot_clip_zero_height_page_is_not_clipped() {
        assert_eq!(screenshot_clip(1280.0, 0.0, 15000.0), None);
    }

    #[test]
    fn screenshot_clip_zero_cap_clips_any_positive_height() {
        assert_eq!(screenshot_clip(1280.0, 1.0, 0.0), Some((1280.0, 0.0)));
    }

    #[test]
    fn screenshot_clip_preserves_width_unchanged() {
        // Width is passed through verbatim; only height is capped.
        assert_eq!(
            screenshot_clip(3840.0, 99_999.0, 15_000.0),
            Some((3840.0, 15_000.0))
        );
    }

    // --- screenshot_max_height_px: env-var override ---------------------------
    //
    // The function reads `CRW_RENDERER__SCREENSHOT_MAX_HEIGHT_PX` fresh on every
    // call (no caching), so these tests share one process-wide env var and must
    // not run concurrently with each other. Serialize with a dedicated mutex.
    use super::{SCREENSHOT_MAX_HEIGHT_PX, screenshot_max_height_px};
    static SCREENSHOT_ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    const SCREENSHOT_ENV_VAR: &str = "CRW_RENDERER__SCREENSHOT_MAX_HEIGHT_PX";

    #[test]
    fn screenshot_max_height_unset_uses_default() {
        let _g = SCREENSHOT_ENV_GUARD.lock().unwrap();
        // SAFETY: serialized by SCREENSHOT_ENV_GUARD above; no other test in
        // this file touches this var without holding the same lock.
        unsafe { std::env::remove_var(SCREENSHOT_ENV_VAR) };
        assert_eq!(screenshot_max_height_px(), SCREENSHOT_MAX_HEIGHT_PX);
    }

    #[test]
    fn screenshot_max_height_valid_override_wins() {
        let _g = SCREENSHOT_ENV_GUARD.lock().unwrap();
        unsafe { std::env::set_var(SCREENSHOT_ENV_VAR, "8000") };
        assert_eq!(screenshot_max_height_px(), 8000.0);
        unsafe { std::env::remove_var(SCREENSHOT_ENV_VAR) };
    }

    #[test]
    fn screenshot_max_height_trims_whitespace() {
        let _g = SCREENSHOT_ENV_GUARD.lock().unwrap();
        unsafe { std::env::set_var(SCREENSHOT_ENV_VAR, "  12345.5  ") };
        assert_eq!(screenshot_max_height_px(), 12345.5);
        unsafe { std::env::remove_var(SCREENSHOT_ENV_VAR) };
    }

    #[test]
    fn screenshot_max_height_zero_falls_back_to_default() {
        // `filter(|v| *v > 0.0)` rejects 0 — a zero cap would clip every page.
        let _g = SCREENSHOT_ENV_GUARD.lock().unwrap();
        unsafe { std::env::set_var(SCREENSHOT_ENV_VAR, "0") };
        assert_eq!(screenshot_max_height_px(), SCREENSHOT_MAX_HEIGHT_PX);
        unsafe { std::env::remove_var(SCREENSHOT_ENV_VAR) };
    }

    #[test]
    fn screenshot_max_height_negative_falls_back_to_default() {
        let _g = SCREENSHOT_ENV_GUARD.lock().unwrap();
        unsafe { std::env::set_var(SCREENSHOT_ENV_VAR, "-500") };
        assert_eq!(screenshot_max_height_px(), SCREENSHOT_MAX_HEIGHT_PX);
        unsafe { std::env::remove_var(SCREENSHOT_ENV_VAR) };
    }

    #[test]
    fn screenshot_max_height_unparseable_falls_back_to_default() {
        let _g = SCREENSHOT_ENV_GUARD.lock().unwrap();
        unsafe { std::env::set_var(SCREENSHOT_ENV_VAR, "not-a-number") };
        assert_eq!(screenshot_max_height_px(), SCREENSHOT_MAX_HEIGHT_PX);
        unsafe { std::env::remove_var(SCREENSHOT_ENV_VAR) };
    }

    #[test]
    fn screenshot_max_height_empty_string_falls_back_to_default() {
        let _g = SCREENSHOT_ENV_GUARD.lock().unwrap();
        unsafe { std::env::set_var(SCREENSHOT_ENV_VAR, "") };
        assert_eq!(screenshot_max_height_px(), SCREENSHOT_MAX_HEIGHT_PX);
        unsafe { std::env::remove_var(SCREENSHOT_ENV_VAR) };
    }

    // --- challenge_retry_budget: more boundaries -------------------------------

    #[test]
    fn challenge_retry_budget_large_retry_count_does_not_overflow() {
        use super::{CHALLENGE_POLL_INTERVAL_MS, challenge_retry_budget};
        // u32::MAX retries * a u64 interval must not panic (debug build would
        // abort on integer overflow); the multiplication promotes to u64 first.
        let budget = challenge_retry_budget(u32::MAX);
        assert_eq!(
            budget,
            Duration::from_millis(CHALLENGE_POLL_INTERVAL_MS * u64::from(u32::MAX))
        );
    }

    #[test]
    fn challenge_retry_budget_two_retries() {
        use super::{CHALLENGE_POLL_INTERVAL_MS, challenge_retry_budget};
        assert_eq!(
            challenge_retry_budget(2),
            Duration::from_millis(CHALLENGE_POLL_INTERVAL_MS * 2)
        );
    }

    // --- is_content_stable: more boundaries ------------------------------------

    #[test]
    fn content_stable_exactly_at_tolerance_boundary() {
        // prev=10_000 → tolerance = max(500, 500) = 500. Exactly 500 is `<=`, so
        // it must still count as stable.
        assert!(is_content_stable(10_000, 10_500, true));
        assert!(is_content_stable(10_000, 9_500, true));
    }

    #[test]
    fn content_stable_one_byte_past_tolerance_is_unstable() {
        assert!(!is_content_stable(10_000, 10_501, true));
        assert!(!is_content_stable(10_000, 9_499, true));
    }

    #[test]
    fn content_stable_tiny_page_499_delta_still_within_500_floor() {
        // prev=100 → tolerance = max(5, 500) = 500, so a 499-byte swing on a
        // tiny page still reads as stable (the floor exists precisely so small
        // pages aren't judged unstable on noise).
        assert!(is_content_stable(100, 599, true));
    }

    #[test]
    fn content_stable_tiny_page_501_delta_exceeds_floor() {
        assert!(!is_content_stable(100, 601, true));
    }

    #[test]
    fn content_stable_huge_lengths_do_not_overflow() {
        // `abs_diff` avoids the classic `curr - prev` underflow panic on a
        // shrink; also exercise near-u64::MAX lengths.
        let prev = u64::MAX - 1000;
        let curr = u64::MAX - 1500;
        assert!(is_content_stable(prev, curr, true));
    }

    // --- is_spa_text_ready: more boundaries -------------------------------------

    #[test]
    fn spa_text_ready_i64_min_is_not_ready() {
        assert!(!is_spa_text_ready(i64::MIN));
    }

    #[test]
    fn spa_text_ready_i64_max_is_ready() {
        assert!(is_spa_text_ready(i64::MAX));
    }

    #[test]
    fn spa_text_ready_one_above_threshold() {
        assert!(is_spa_text_ready(SPA_BODY_TEXT_MIN_CHARS as i64 + 1));
    }

    // --- NetworkActivityTracker::is_settled (networkAlmostIdle) ----------------

    #[test]
    fn tracker_settled_allows_up_to_max_inflight() {
        let t = NetworkActivityTracker::new();
        t.record_request_start();
        t.record_request_start();
        // 2 in flight, max_inflight=2 → settled (once quiet period passes).
        assert!(t.is_settled(0, 2));
    }

    #[test]
    fn tracker_not_settled_above_max_inflight() {
        let t = NetworkActivityTracker::new();
        t.record_request_start();
        t.record_request_start();
        t.record_request_start();
        // 3 in flight > max_inflight=2 → not settled regardless of quiet period.
        assert!(!t.is_settled(0, 2));
    }

    #[test]
    fn tracker_not_settled_during_quiet_period_even_under_max_inflight() {
        let t = NetworkActivityTracker::new();
        t.record_request_start();
        t.record_request_end();
        assert!(!t.is_settled(10_000, 2));
    }

    #[test]
    fn tracker_settled_with_zero_inflight_and_zero_quiet() {
        let t = NetworkActivityTracker::new();
        assert!(t.is_settled(0, 0));
        assert!(t.is_settled(0, super::ALMOST_IDLE_MAX_INFLIGHT));
    }

    #[test]
    fn tracker_negative_inflight_counts_as_settled() {
        let t = NetworkActivityTracker::new();
        t.record_request_end();
        assert!(t.is_settled(0, 0));
    }

    // --- rewrite_ws_host: CDP discovered-URL rewriting --------------------------

    use super::rewrite_ws_host;

    #[test]
    fn rewrite_ws_host_replaces_discovered_host_with_configured() {
        assert_eq!(
            rewrite_ws_host(
                "ws://127.0.0.1:9222/devtools/browser/abc-123",
                "ws://chrome:9222/"
            ),
            "ws://chrome:9222/devtools/browser/abc-123"
        );
    }

    #[test]
    fn rewrite_ws_host_preserves_wss_scheme_from_configured() {
        assert_eq!(
            rewrite_ws_host(
                "ws://127.0.0.1:9222/devtools/browser/xyz",
                "wss://secure.example/"
            ),
            "wss://secure.example/devtools/browser/xyz"
        );
    }

    #[test]
    fn rewrite_ws_host_defaults_to_root_path_when_discovered_has_none() {
        assert_eq!(
            rewrite_ws_host("ws://127.0.0.1:9222", "ws://chrome:9222/"),
            "ws://chrome:9222/"
        );
    }

    #[test]
    fn rewrite_ws_host_works_without_trailing_slash_on_configured() {
        assert_eq!(
            rewrite_ws_host(
                "ws://127.0.0.1:9222/devtools/browser/abc",
                "ws://chrome:9222"
            ),
            "ws://chrome:9222/devtools/browser/abc"
        );
    }

    #[test]
    fn rewrite_ws_host_ignores_configured_path_keeps_only_host_port() {
        // Only host:port from `configured` is kept — any path on it (e.g. a
        // stale cached devtools path) must not leak into the rewritten URL.
        assert_eq!(
            rewrite_ws_host(
                "ws://127.0.0.1:9222/devtools/browser/new-id",
                "ws://chrome:9222/devtools/browser/old-stale-id"
            ),
            "ws://chrome:9222/devtools/browser/new-id"
        );
    }

    // --- classify_connect_outcome: error-arm coverage ---------------------------
    //
    // The `Ok(_)` arm needs a live `CdpConnection`, which this hermetic suite
    // cannot construct (RULES: no real Chrome). Only the `Err` arms are covered.

    use super::classify_connect_outcome;

    #[test]
    fn classify_connect_outcome_timeout_is_ws_handshake_timeout() {
        assert_eq!(
            classify_connect_outcome(&Err(CrwError::Timeout(5000))),
            "ws_handshake_timeout"
        );
    }

    #[test]
    fn classify_connect_outcome_discovery_failure_is_version_probe_fail() {
        assert_eq!(
            classify_connect_outcome(&Err(CrwError::RendererError(
                "CDP discovery failed: connection refused".into()
            ))),
            "version_probe_fail"
        );
    }

    #[test]
    fn classify_connect_outcome_other_renderer_error_is_ws_dial_fail() {
        // A RendererError NOT mentioning "CDP discovery" (e.g. the WS dial
        // itself failing) falls through to the generic bucket.
        assert_eq!(
            classify_connect_outcome(&Err(CrwError::RendererError("connection refused".into()))),
            "ws_dial_fail"
        );
    }

    #[test]
    fn classify_connect_outcome_unrelated_error_variant_is_ws_dial_fail() {
        assert_eq!(
            classify_connect_outcome(&Err(CrwError::NotFound("x".into()))),
            "ws_dial_fail"
        );
    }

    // --- is_challenge_page ------------------------------------------------------

    use super::is_challenge_page;

    #[test]
    fn challenge_page_detects_cloudflare_just_a_moment() {
        assert!(is_challenge_page(
            "<html><body>Just a moment...</body></html>"
        ));
    }

    #[test]
    fn challenge_page_detects_cf_browser_verification_marker() {
        assert!(is_challenge_page(
            "<div id=\"cf-browser-verification\">checking</div>"
        ));
    }

    #[test]
    fn challenge_page_detects_challenge_platform_marker() {
        assert!(is_challenge_page("loading challenge-platform assets"));
    }

    #[test]
    fn challenge_page_detects_attention_required() {
        assert!(is_challenge_page("<title>Attention Required!</title>"));
    }

    #[test]
    fn challenge_page_requires_both_words_for_generic_challenge_cloudflare_combo() {
        // "challenge" alone (no "cloudflare") must NOT trip the detector —
        // otherwise any page mentioning a coding challenge would false-positive.
        assert!(!is_challenge_page(
            "<p>Take our coding challenge to join!</p>"
        ));
        assert!(is_challenge_page("<p>cloudflare challenge in progress</p>"));
    }

    #[test]
    fn challenge_page_is_case_insensitive() {
        assert!(is_challenge_page("JUST A MOMENT..."));
        assert!(is_challenge_page("CF-CHALLENGE-RUNNING"));
    }

    #[test]
    fn challenge_page_ordinary_html_is_not_a_challenge() {
        assert!(!is_challenge_page(
            "<html><body><h1>Welcome</h1><p>Normal content.</p></body></html>"
        ));
    }

    #[test]
    fn challenge_page_over_50kb_is_never_flagged() {
        // Real challenge shells are small; a >50KB document containing the
        // phrase incidentally (e.g. in a blog post about Cloudflare) must not
        // be misclassified and short-circuit an otherwise-successful render.
        let mut html = "just a moment".to_string();
        html.push_str(&"x".repeat(60_000));
        assert!(!is_challenge_page(&html));
    }

    #[test]
    fn challenge_page_empty_string_is_not_a_challenge() {
        assert!(!is_challenge_page(""));
    }

    // --- detect_navigation_error -------------------------------------------------

    use super::detect_navigation_error;

    #[test]
    fn navigation_error_extracts_reason_after_colon() {
        let html = "<html>NavigationError: reason: net::ERR_NAME_NOT_RESOLVED</html>";
        assert_eq!(
            detect_navigation_error(html),
            Some("net::err_name_not_resolved".to_string())
        );
    }

    #[test]
    fn navigation_error_stops_reason_at_tag_boundary() {
        let html = "<p>navigation failed</p><p>reason: timeout</p><footer>x</footer>";
        assert_eq!(detect_navigation_error(html), Some("timeout".to_string()));
    }

    #[test]
    fn navigation_error_stops_reason_at_newline() {
        let html = "navigation failed\nreason: dns error\nmore text";
        assert_eq!(detect_navigation_error(html), Some("dns error".to_string()));
    }

    #[test]
    fn navigation_error_without_reason_marker_returns_unknown() {
        let html = "this page had a navigation failed situation";
        assert_eq!(detect_navigation_error(html), Some("unknown".to_string()));
    }

    #[test]
    fn navigation_error_is_case_insensitive() {
        assert_eq!(
            detect_navigation_error("NAVIGATIONERROR occurred"),
            Some("unknown".to_string())
        );
    }

    #[test]
    fn navigation_error_ordinary_page_returns_none() {
        assert_eq!(
            detect_navigation_error("<html><body>Hello world</body></html>"),
            None
        );
    }

    #[test]
    fn navigation_error_over_2000_bytes_returns_none() {
        // Real Chrome/LightPanda error shells are tiny; a long real page that
        // happens to contain the phrase must not be misdetected.
        let mut html = "x".repeat(2001);
        html.push_str("navigation failed");
        assert_eq!(detect_navigation_error(&html), None);
    }

    #[test]
    fn navigation_error_at_exactly_2000_bytes_is_still_checked() {
        // Boundary: `> 2000` bails, so exactly 2000 bytes still runs the check.
        let mut html = "navigation failed reason: short".to_string();
        while html.len() < 2000 {
            html.push('x');
        }
        assert_eq!(html.len(), 2000);
        assert!(detect_navigation_error(&html).is_some());
    }

    #[test]
    fn navigation_error_empty_string_returns_none() {
        assert_eq!(detect_navigation_error(""), None);
    }

    // --- CdpRenderer::intercept_active_for --------------------------------------

    #[test]
    fn intercept_inactive_when_interception_disabled() {
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 1);
        // intercept_enabled defaults to false; must be false for any URL.
        assert!(!r.intercept_active_for("https://example.com/"));
    }

    #[test]
    fn intercept_active_when_enabled_with_empty_host_disable_list() {
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 1).with_interception(
            true,
            crate::blocklist::Blocklist::defaults(),
            Vec::new(),
        );
        assert!(r.intercept_active_for("https://example.com/"));
    }

    #[test]
    fn intercept_inactive_for_host_on_the_disable_list() {
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 1).with_interception(
            true,
            crate::blocklist::Blocklist::defaults(),
            vec!["example.com".to_string()],
        );
        assert!(!r.intercept_active_for("https://sub.example.com/page"));
        assert!(!r.intercept_active_for("http://example.com/"));
    }

    #[test]
    fn intercept_active_for_host_not_on_the_disable_list() {
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 1).with_interception(
            true,
            crate::blocklist::Blocklist::defaults(),
            vec!["example.com".to_string()],
        );
        assert!(r.intercept_active_for("https://other.test/"));
    }

    #[test]
    fn intercept_host_disable_match_is_case_insensitive() {
        // `with_interception` lowercases the disable list up front; the
        // comparison side must lowercase the parsed host to match it.
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 1).with_interception(
            true,
            crate::blocklist::Blocklist::defaults(),
            vec!["EXAMPLE.com".to_string()],
        );
        assert!(!r.intercept_active_for("https://example.com/"));
    }

    #[test]
    fn intercept_active_on_unparseable_url_even_with_disable_list() {
        // `url::Url::parse` failing fails OPEN here (`Err(_) => return true`):
        // an unparseable URL can't be matched against the disable list, so
        // interception stays on rather than silently skipping protection.
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 1).with_interception(
            true,
            crate::blocklist::Blocklist::defaults(),
            vec!["example.com".to_string()],
        );
        assert!(r.intercept_active_for("not a url at all"));
    }

    // --- CdpRenderer builder field assignment -----------------------------------

    #[test]
    fn with_nav_budget_overrides_the_default_page_timeout_budget() {
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 1);
        assert_eq!(r.nav_budget, Duration::from_millis(1000));
        let r = r.with_nav_budget(4242);
        assert_eq!(r.nav_budget, Duration::from_millis(4242));
    }

    #[test]
    fn with_spa_selector_max_overrides_the_default() {
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 1);
        assert_eq!(
            r.spa_selector_max,
            Duration::from_millis(super::SPA_SELECTOR_MAX_MS)
        );
        let r = r.with_spa_selector_max(3000);
        assert_eq!(r.spa_selector_max, Duration::from_millis(3000));
    }

    #[test]
    fn with_challenge_retries_overrides_the_default_and_allows_zero() {
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 1).with_challenge_retries(0);
        assert_eq!(r.challenge_max_retries, 0);
    }

    #[test]
    fn with_fast_ready_toggles_the_flag() {
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 1);
        assert!(!r.fast_ready);
        let r = r.with_fast_ready(true);
        assert!(r.fast_ready);
        let r = r.with_fast_ready(false);
        assert!(!r.fast_ready);
    }

    #[test]
    fn with_proxy_auth_base_stores_creds_and_default_country() {
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 1);
        assert!(r.proxy_auth_base.is_none());
        assert!(r.default_country.is_none());
        let r = r.with_proxy_auth_base(
            "diuser".to_string(),
            "dipass".to_string(),
            Some("de".to_string()),
        );
        assert_eq!(
            r.proxy_auth_base,
            Some(("diuser".to_string(), "dipass".to_string()))
        );
        assert_eq!(r.default_country, Some("de".to_string()));
    }

    #[test]
    fn with_proxy_auth_base_accepts_no_default_country() {
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 1).with_proxy_auth_base(
            "u".to_string(),
            "p".to_string(),
            None,
        );
        assert!(r.default_country.is_none());
        assert!(r.proxy_auth_base.is_some());
    }

    #[test]
    fn new_clamps_zero_pool_size_to_at_least_one() {
        // `pool_size.max(1)` — a misconfigured 0 must not create a
        // zero-permit semaphore that would deadlock every fetch.
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 0);
        assert_eq!(r.conn_semaphore.available_permits(), 1);
    }

    #[test]
    fn new_pool_field_starts_empty() {
        let r = CdpRenderer::new("chrome", "ws://x/", 1000, 4);
        assert!(r.pool().is_none());
    }

    // --- CdpRenderer::should_retry_with_default_country -------------------------
    //
    // Exercises the full 3-condition gate via the real `REQUEST_COUNTRY` /
    // `REQUEST_PROXY` task-locals, scoped per test so nothing leaks across
    // async tasks.

    #[tokio::test]
    async fn retry_country_false_when_no_proxy_auth_base_configured() {
        let r = CdpRenderer::new("chrome_proxy", "ws://x/", 1000, 1);
        assert!(r.proxy_auth_base.is_none());
        assert!(!r.should_retry_with_default_country());
    }

    #[tokio::test]
    async fn retry_country_false_when_byop_proxy_is_active() {
        let r = CdpRenderer::new("chrome_proxy", "ws://x/", 1000, 1).with_proxy_auth_base(
            "u".to_string(),
            "p".to_string(),
            Some("us".to_string()),
        );
        let entry =
            std::sync::Arc::new(crw_core::ProxyEntry::parse("http://h.example:8080").unwrap());
        let result = crate::REQUEST_PROXY
            .scope(Some(entry), async {
                crate::REQUEST_COUNTRY
                    .scope(Some("de".to_string()), async {
                        r.should_retry_with_default_country()
                    })
                    .await
            })
            .await;
        // A per-request BYOP proxy takes precedence; its CONNECT failure is not
        // a "dead country exit" and retrying would be pointless.
        assert!(!result);
    }

    #[tokio::test]
    async fn retry_country_false_when_no_country_was_requested() {
        let r = CdpRenderer::new("chrome_proxy", "ws://x/", 1000, 1).with_proxy_auth_base(
            "u".to_string(),
            "p".to_string(),
            Some("us".to_string()),
        );
        let result = crate::REQUEST_PROXY
            .scope(None, async {
                crate::REQUEST_COUNTRY
                    .scope(None, async { r.should_retry_with_default_country() })
                    .await
            })
            .await;
        assert!(!result);
    }

    #[tokio::test]
    async fn retry_country_false_when_requested_matches_default() {
        let r = CdpRenderer::new("chrome_proxy", "ws://x/", 1000, 1).with_proxy_auth_base(
            "u".to_string(),
            "p".to_string(),
            Some("us".to_string()),
        );
        let result = crate::REQUEST_PROXY
            .scope(None, async {
                crate::REQUEST_COUNTRY
                    .scope(Some("us".to_string()), async {
                        r.should_retry_with_default_country()
                    })
                    .await
            })
            .await;
        assert!(!result);
    }

    #[tokio::test]
    async fn retry_country_true_when_requested_differs_from_default() {
        let r = CdpRenderer::new("chrome_proxy", "ws://x/", 1000, 1).with_proxy_auth_base(
            "u".to_string(),
            "p".to_string(),
            Some("us".to_string()),
        );
        let result = crate::REQUEST_PROXY
            .scope(None, async {
                crate::REQUEST_COUNTRY
                    .scope(Some("de".to_string()), async {
                        r.should_retry_with_default_country()
                    })
                    .await
            })
            .await;
        assert!(result);
    }

    #[tokio::test]
    async fn retry_country_normalizes_case_and_whitespace_before_comparing() {
        let r = CdpRenderer::new("chrome_proxy", "ws://x/", 1000, 1).with_proxy_auth_base(
            "u".to_string(),
            "p".to_string(),
            Some("us".to_string()),
        );
        let result = crate::REQUEST_PROXY
            .scope(None, async {
                crate::REQUEST_COUNTRY
                    .scope(Some("  US  ".to_string()), async {
                        r.should_retry_with_default_country()
                    })
                    .await
            })
            .await;
        // "  US  " normalizes to "us", same as the default → no retry.
        assert!(!result);
    }

    #[tokio::test]
    async fn retry_country_invalid_requested_country_is_treated_as_none() {
        let r = CdpRenderer::new("chrome_proxy", "ws://x/", 1000, 1).with_proxy_auth_base(
            "u".to_string(),
            "p".to_string(),
            Some("us".to_string()),
        );
        let result = crate::REQUEST_PROXY
            .scope(None, async {
                // "usa" is 3 chars — fails the 2-alpha normalization, so this
                // must behave exactly like no country was requested.
                crate::REQUEST_COUNTRY
                    .scope(Some("usa".to_string()), async {
                        r.should_retry_with_default_country()
                    })
                    .await
            })
            .await;
        assert!(!result);
    }

    #[tokio::test]
    async fn retry_country_true_when_no_default_country_configured() {
        let r = CdpRenderer::new("chrome_proxy", "ws://x/", 1000, 1).with_proxy_auth_base(
            "u".to_string(),
            "p".to_string(),
            None,
        );
        let result = crate::REQUEST_PROXY
            .scope(None, async {
                crate::REQUEST_COUNTRY
                    .scope(Some("fr".to_string()), async {
                        r.should_retry_with_default_country()
                    })
                    .await
            })
            .await;
        // Some("fr") != None (default) → still counts as a difference.
        assert!(result);
    }

    // --- is_browserless_direct_ws: additional coverage --------------------------

    #[test]
    fn browserless_named_path_matches_as_substring_anywhere() {
        assert!(is_browserless_direct_ws(
            "wss://x.example/prefix/chromium/suffix"
        ));
    }

    #[test]
    fn browserless_plain_url_with_query_but_no_token_is_not_direct_ws() {
        assert!(!is_browserless_direct_ws(
            "wss://lightpanda.example/ws?debug=1"
        ));
    }

    #[test]
    fn browserless_hostname_that_merely_starts_with_a_browser_name_is_a_false_positive() {
        // BUG (pre-existing, not introduced here, left unfixed per RULES.md #2):
        // `is_browserless_direct_ws` checks `url.contains("/chromium")` etc. on
        // the WHOLE url string, not just the path. A hostname like
        // "chromium.example.com" produces "//chromium.example.com" right after
        // the scheme, which contains the substring "/chromium" — so an ordinary
        // `/json/version`-serving endpoint on such a host is misclassified as a
        // direct-WS (browserless-style) endpoint and its discovery step is
        // skipped. Asserting current behaviour, not endorsing it.
        assert!(is_browserless_direct_ws("wss://chromium.example.com/ws"));
    }

    #[test]
    fn browserless_empty_string_is_not_direct_ws() {
        assert!(!is_browserless_direct_ws(""));
    }

    // --- is_capturable_mime: additional coverage --------------------------------

    #[test]
    fn capturable_mime_is_case_insensitive() {
        assert!(is_capturable_mime("APPLICATION/JSON"));
        assert!(is_capturable_mime("Application/Ld+Json"));
    }

    #[test]
    fn capturable_mime_trims_surrounding_whitespace_around_params() {
        assert!(is_capturable_mime("application/json ; charset=utf-8"));
    }

    #[test]
    fn capturable_mime_multiple_semicolons_only_uses_first_segment() {
        assert!(is_capturable_mime(
            "application/json; charset=utf-8; boundary=x"
        ));
    }

    #[test]
    fn capturable_mime_whitespace_only_is_rejected() {
        assert!(!is_capturable_mime("   "));
    }

    // --- build_auth_response: additional coverage -------------------------------

    #[test]
    fn auth_response_escapes_special_characters_in_credentials() {
        // Credentials can legitimately contain characters JSON must escape
        // (quotes, backslashes); the payload must round-trip through
        // serde_json without corrupting them.
        let v = build_auth_response("req-4", Some(("us\"er", "p\\ass")));
        assert_eq!(v["authChallengeResponse"]["username"], "us\"er");
        assert_eq!(v["authChallengeResponse"]["password"], "p\\ass");
        let s = serde_json::to_string(&v).unwrap();
        let round_tripped: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(round_tripped, v);
    }

    #[test]
    fn auth_response_handles_empty_credential_strings() {
        let v = build_auth_response("req-5", Some(("", "")));
        assert_eq!(v["authChallengeResponse"]["username"], "");
        assert_eq!(v["authChallengeResponse"]["password"], "");
    }

    #[test]
    fn auth_response_handles_unicode_credentials() {
        let v = build_auth_response("req-6", Some(("üser__cr.tr", "şifre")));
        assert_eq!(v["authChallengeResponse"]["username"], "üser__cr.tr");
        assert_eq!(v["authChallengeResponse"]["password"], "şifre");
    }

    #[test]
    fn auth_response_preserves_request_id_verbatim() {
        let v = build_auth_response("weird/id:with-chars_123", None);
        assert_eq!(v["requestId"], "weird/id:with-chars_123");
    }

    #[test]
    fn auth_response_empty_request_id_still_builds_a_payload() {
        let v = build_auth_response("", None);
        assert_eq!(v["requestId"], "");
        assert_eq!(v["authChallengeResponse"]["response"], "CancelAuth");
    }

    // --- is_proxy_tunnel_error: additional coverage -----------------------------

    #[test]
    fn proxy_tunnel_error_matches_when_message_has_extra_context_around_it() {
        assert!(is_proxy_tunnel_error(&CrwError::RendererError(
            "Page.navigate error at https://x/: net::ERR_TUNNEL_CONNECTION_FAILED (retry 2/3)"
                .into()
        )));
    }

    #[test]
    fn proxy_tunnel_error_ignores_other_crw_error_variants() {
        assert!(!is_proxy_tunnel_error(&CrwError::HttpError(
            "net::ERR_TUNNEL_CONNECTION_FAILED".into()
        )));
        assert!(!is_proxy_tunnel_error(&CrwError::TargetUnreachable(
            "net::ERR_TUNNEL_CONNECTION_FAILED".into()
        )));
        assert!(!is_proxy_tunnel_error(&CrwError::NotFound("x".into())));
        assert!(!is_proxy_tunnel_error(&CrwError::RateLimited));
    }

    #[test]
    fn proxy_tunnel_error_empty_message_does_not_match() {
        assert!(!is_proxy_tunnel_error(&CrwError::RendererError(
            String::new()
        )));
    }

    // --- outbound_block_label: additional coverage ------------------------------

    #[tokio::test]
    async fn outbound_guard_rejects_non_http_schemes() {
        let ctx = test_ctx();
        // Not in the {data, blob, about} allowlist and not http(s) → policy block.
        assert!(
            outbound_block_label("file:///etc/passwd", &ctx)
                .await
                .is_some()
        );
        assert!(
            outbound_block_label("ftp://example.com/file", &ctx)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn outbound_guard_allows_about_scheme() {
        let ctx = test_ctx();
        assert!(outbound_block_label("about:blank", &ctx).await.is_none());
    }

    #[tokio::test]
    async fn outbound_guard_rejects_ipv6_loopback_with_port() {
        let ctx = test_ctx();
        assert!(
            outbound_block_label("http://[::1]:9999/admin", &ctx)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn outbound_guard_rejects_link_local_metadata_service_https() {
        let ctx = test_ctx();
        assert!(
            outbound_block_label("https://169.254.169.254/latest/meta-data/", &ctx)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn outbound_guard_empty_url_is_rejected() {
        let ctx = test_ctx();
        assert!(outbound_block_label("", &ctx).await.is_some());
    }

    #[tokio::test]
    async fn outbound_guard_rejects_192_168_private_range() {
        let ctx = test_ctx();
        assert!(
            outbound_block_label("http://192.168.0.1/", &ctx)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn outbound_guard_rejects_172_16_private_range() {
        let ctx = test_ctx();
        assert!(
            outbound_block_label("http://172.16.5.5/", &ctx)
                .await
                .is_some()
        );
    }

    // --- split_caller_headers: additional coverage ------------------------------

    #[test]
    fn split_caller_headers_preserves_multiple_non_ua_headers() {
        let mut h = HashMap::new();
        h.insert("Cookie".to_string(), "a=b; c=d".to_string());
        h.insert("Referer".to_string(), "https://example.com/".to_string());
        h.insert("X-Custom-Thing".to_string(), "v1".to_string());
        let (ua, extra) = split_caller_headers(&h);
        assert!(ua.is_none());
        assert_eq!(extra.len(), 3);
        assert_eq!(
            extra.get("Cookie").and_then(|v| v.as_str()),
            Some("a=b; c=d")
        );
    }

    #[test]
    fn split_caller_headers_mixed_case_user_agent_key() {
        let mut h = HashMap::new();
        h.insert("UsEr-AgEnT".to_string(), "Weird/1.0".to_string());
        let (ua, extra) = split_caller_headers(&h);
        assert_eq!(ua.as_deref(), Some("Weird/1.0"));
        assert!(extra.is_empty());
    }

    #[test]
    fn split_caller_headers_unicode_header_value_preserved() {
        let mut h = HashMap::new();
        h.insert("X-Lang".to_string(), "türkçe".to_string());
        let (_, extra) = split_caller_headers(&h);
        assert_eq!(extra.get("X-Lang").and_then(|v| v.as_str()), Some("türkçe"));
    }

    // --- lightpanda_safe_ua: additional coverage --------------------------------

    #[test]
    fn lightpanda_safe_ua_empty_string_is_unchanged() {
        assert_eq!(lightpanda_safe_ua(""), "");
    }

    #[test]
    fn lightpanda_safe_ua_only_strips_leading_prefix_not_mid_string_occurrence() {
        // A second, later "Mozilla/5.0 " occurrence (unrealistic but possible in
        // a crafted header) must survive — only the leading prefix is stripped.
        let ua = "Mozilla/5.0 (compatible) extra Mozilla/5.0 tail";
        assert_eq!(
            lightpanda_safe_ua(ua),
            "(compatible) extra Mozilla/5.0 tail"
        );
    }
}
