use async_trait::async_trait;
use crw_core::Deadline;
use crw_core::error::{CrwError, CrwResult};
use crw_core::types::FetchResult;
use std::collections::HashMap;
use std::time::Instant;

use crate::traits::PageFetcher;

/// Maximum response body size (50 MB) to prevent memory exhaustion. The
/// previous 10 MB cap rejected legitimate large reports/PDFs (bench had a
/// ~12 MB PDF mis-flagged as 502). 50 MB is generous enough for almost any
/// document while still bounding memory use.
pub(crate) const MAX_RESPONSE_BYTES: usize = 50 * 1024 * 1024;
/// TCP connect timeout for the renderer's HTTP tier. A healthy handshake is one
/// RTT (well under a second even intercontinental); by SYN-retransmit timing a
/// connect past ~2.5s means at least two dropped SYNs, i.e. a dead, blocked, or
/// blackholing host whose content is unreliable anyway.
///
/// Lowered 5s -> 2.5s so that inside the SaaS's tight 5s scrape deadline a
/// blackhole surfaces as a connect error with budget left for the fallback-proxy
/// retry (a different egress that CAN reach the origin). At 5s the outer deadline
/// always fired first and the proxy never got a turn.
///
/// This is a single global const shared by the renderer's HTTP tier across scrape,
/// crawl and map (they share one `FallbackRenderer.http`; the SearXNG client has
/// its own separate 5s connect timeout). Crawl/map run on far larger budgets and
/// do not need the lower value, but they are unharmed: a >2.5s connect is a
/// pathological origin, and crawl already skips an unreachable page gracefully. A
/// per-caller connect timeout would need threading through the shared fetcher and
/// is deliberately out of scope here.
pub(crate) const HTTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2500);
/// Overall request timeout for HTTP requests.
const HTTP_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// One retry on transient errors. GET is idempotent so a single retry is safe;
/// origins frequently emit 502/503/504 under brief overload and connect/timeout
/// errors are often DNS or TCP races that resolve on the next attempt.
const HTTP_MAX_RETRIES: u32 = 1;
/// Backoff before the retry attempt. Short — we are inside the request path
/// and the upstream timeout is 30s, so we cannot afford long sleeps.
const HTTP_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(250);
/// Budget held back from a proxy attempt that was armed mid-loop by a 429 or a
/// header-announced challenge, so a hanging proxy can always fall back to direct.
///
/// Much smaller than [`crate::egress::DIRECT_FALLBACK_RESERVE`] (4s) on purpose:
/// that one reserves for a FIRST-CONTACT direct attempt on a latched host, while
/// this one only has to repeat a response we already received milliseconds ago.
const CHALLENGE_DIRECT_RESERVE: std::time::Duration = std::time::Duration::from_millis(1500);
/// Ceiling on a single challenge-armed proxy attempt, independent of how much
/// deadline is left.
///
/// Without a ceiling the attempt gets `remaining - CHALLENGE_DIRECT_RESERVE`,
/// which on the 15s search budget is 13.5s — so a hanging exit burns almost the
/// whole request and THEN returns the same empty body the origin gave us in 70ms.
/// That is a p90 regression on exactly the class this change exists to improve.
///
/// 6s comes from the measured distribution of the residential exit on the
/// affected hosts: p50 2.3s, with one 38.2s outlier. It covers the realistic
/// success cases with >2x headroom while capping the wasted wall-clock when the
/// exit is in its bad tail. Everything above it stays with the direct rescue.
const CHALLENGE_PROXY_MAX: std::time::Duration = std::time::Duration::from_secs(6);

/// Returns true if a `reqwest::Error` is worth retrying on the SAME egress.
/// Read-phase timeouts only (`is_timeout` without `is_connect`): the origin
/// connected and is slow, so a retry may catch a faster response. Connection-level
/// failures (refused/reset/connect-timeout) are NOT retriable here — they route to
/// the proxy fallback (`is_connection_failure`), or, when no proxy is armed, get a
/// single direct retry at the call site.
fn is_retriable_error(e: &reqwest::Error) -> bool {
    // A read-phase timeout (`is_timeout` without `is_connect`) means we DID connect
    // and the origin is slow to respond — a direct retry may help. Connection-level
    // failures are handled by the proxy arm instead (see `is_connection_failure`),
    // so they are excluded here: retrying them on the same dead egress just wastes
    // budget.
    e.is_timeout() && !e.is_connect()
}

/// True when the direct egress could not talk to the origin at all: the peer
/// refused/reset/aborted the transport, OR the TCP connect timed out (a blackhole
/// that silently drops our SYN). All are signals our egress IP is unwelcome, which
/// a different egress (the fallback proxy) often clears.
///
/// Two distinct reqwest shapes, both verified empirically against this workspace:
///   refused        -> is_connect=true,  chain io::ConnectionRefused
///   reset (RST)    -> is_connect=false, chain io::ConnectionReset  (post-handshake,
///                     hyper_util ErrorKind::SendRequest — this is why `is_connect()`
///                     alone is not enough)
///   connect-timeout-> is_connect=true  AND is_timeout=true, chain io::TimedOut
///   read-timeout   -> is_connect=false, is_timeout=true, no io chain  (EXCLUDED: the
///                     origin answered our SYN and is merely slow; a proxy won't help)
///   dns failure    -> is_connect=true,  no io in chain  (EXCLUDED: the proxy resolves
///                     the same name, so a retry through it is wasted)
fn is_connection_failure(e: &reqwest::Error) -> bool {
    // Connect-phase timeout (blackhole): distinct from a read timeout by is_connect.
    if e.is_connect() && e.is_timeout() {
        return true;
    }
    // Refused / reset / aborted, matched on the transport ErrorKind in the chain.
    let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(s) = src {
        if let Some(io) = s.downcast_ref::<std::io::Error>()
            && matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionAborted
            )
        {
            return true;
        }
        src = std::error::Error::source(s);
    }
    false
}

/// Returns true if a response status warrants one retry. Limited to the
/// canonical transient gateway/origin signals — 5xx errors that are not
/// retriable (501, 505) are excluded so we don't waste time on permanent
/// upstream misconfigurations.
fn is_retriable_status(status: u16) -> bool {
    matches!(status, 502..=504)
}

/// Returns true if a response status means the origin is rate-limiting the
/// host's egress IP — a signal that a *different* egress IP (proxy) may clear
/// it. 429 = Too Many Requests (the explicit rate-limit signal). Retried ONCE
/// through the configured proxy when armed; every other status is untouched.
fn is_ratelimit_status(status: u16) -> bool {
    matches!(status, 429)
}

/// A vendor challenge announced in a response header, independent of status and
/// body. Both variants mean "this origin refused *this egress IP*", which is
/// exactly the class a different egress can clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChallengeHeader {
    CloudflareMitigated,
    AwsWaf,
}

impl ChallengeHeader {
    /// Stable marker written to `FetchResult::warning`; `crate::fetch_inner`
    /// matches on it to decide whether to escalate.
    pub(crate) fn marker(self) -> &'static str {
        match self {
            Self::CloudflareMitigated => "cloudflare_mitigated",
            Self::AwsWaf => "waf_challenge",
        }
    }

    /// Customer-visible text. Kept next to the marker so an AWS-WAF block can
    /// never be reported to the caller as a Cloudflare one (the previous text
    /// was hardcoded and reaches the API via `crw_crawl::single`).
    pub(crate) fn warning_text(self) -> &'static str {
        match self {
            Self::CloudflareMitigated => {
                "cf-mitigated header indicates Cloudflare challenge or block"
            }
            Self::AwsWaf => "x-amzn-waf-action header indicates an AWS WAF challenge",
        }
    }
}

/// Detect a header-announced challenge on a response.
///
/// Dispatches on header NAME to its own predicate — the two value lists differ
/// (`cf-mitigated`: challenge|block, `x-amzn-waf-action`: challenge|captcha) and
/// merging them would silently change the pre-existing Cloudflare behaviour.
///
/// Factored because the same read appeared verbatim at three points in the retry
/// state machine (direct retry arm, latched-proxy rescue guard, and the final
/// `FetchResult` stamp), each against a different response value.
pub(crate) fn challenge_header(headers: &reqwest::header::HeaderMap) -> Option<ChallengeHeader> {
    let value = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if value("cf-mitigated").is_some_and(crate::detector::is_cloudflare_mitigated_header) {
        return Some(ChallengeHeader::CloudflareMitigated);
    }
    if value("x-amzn-waf-action").is_some_and(crate::detector::is_aws_waf_action_header) {
        return Some(ChallengeHeader::AwsWaf);
    }
    None
}

/// Should the fetch retry once through the fallback proxy? True on an explicit
/// rate-limit status (429) OR when a response header flags a vendor
/// challenge — the header is a positive signal, so a different egress IP may
/// clear it even when served as 200/202/403/503. Pure for unit test.
fn should_arm_proxy(status: u16, challenge: Option<ChallengeHeader>) -> bool {
    is_ratelimit_status(status) || challenge.is_some()
}

/// Is `CRW_HTTP_TLS_RELAXED_FALLBACK` enabled? When on, a fetch that fails TLS
/// certificate verification is retried ONCE with verification disabled (small
/// orgs frequently misconfigure their chain — e.g. a CA cert served as the leaf,
/// or an expired/self-signed cert — yet the content is perfectly fetchable).
/// Cert-errors-only; every other failure mode keeps strict verification.
fn tls_relaxed_fallback_enabled() -> bool {
    std::env::var("CRW_HTTP_TLS_RELAXED_FALLBACK")
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "true" || v == "1" || v == "yes"
        })
        .unwrap_or(false)
}

/// The proxy URL to retry through when an origin rate-limits the host's egress
/// IP (`CRW_HTTP_RATELIMIT_PROXY_URL`, e.g. `http://user:pass@gateway:port`).
/// When set, a fetch that returns 429 is retried ONCE through this proxy — a
/// different egress IP usually clears the limit, so the engine no longer stalls
/// behind a single shared IP when a huge proxy pool is available. Unset (or
/// empty) = behavior identical to before (no proxy retry). SSRF protection is
/// unaffected (it runs on the resolved target URL, not the proxy hop).
/// Does the environment configure a proxy that `reqwest` will pick up on its own?
///
/// `build_client` does not call `.no_proxy()`, so reqwest honours `HTTP_PROXY` /
/// `HTTPS_PROXY` / `ALL_PROXY` automatically. A fetcher built with `proxy: None`
/// therefore still egresses through a proxy when those are set — which would make
/// the egress-latch hooks below attribute a proxy-observed block to DIRECT
/// traffic, latch it, and demote every other caller's healthy direct egress.
/// Unset on the managed deployment; load-bearing for self-hosters behind a
/// corporate proxy.
fn env_proxy_configured() -> bool {
    [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ]
    .iter()
    .any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()))
}

fn ratelimit_proxy_url() -> Option<String> {
    std::env::var("CRW_HTTP_RATELIMIT_PROXY_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Returns true if a `reqwest::Error` (or anything in its source chain) is a TLS
/// certificate verification failure — the ONLY error class the relaxed-TLS
/// fallback should react to. Detected by message (rustls/openssl surface these
/// as opaque connect errors, so there is no typed predicate to match on).
fn is_cert_error(e: &reqwest::Error) -> bool {
    // Start at the source: the reqwest error's own Display is the kind string
    // plus the request URL, so matching it could only ever false-positive on a
    // target path that happens to mention certificates.
    let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(s) = src {
        let m = s.to_string().to_ascii_lowercase();
        if m.contains("certificate")
            || m.contains("peerfailedverification")
            || m.contains("sslconnecterror")
            || m.contains("invalid peer cert")
            || m.contains("certusedasend")
            || m.contains("cert verify")
            || m.contains("tls handshake")
            || (m.contains("ssl") && (m.contains("verif") || m.contains("cert")))
        {
            return true;
        }
        src = s.source();
    }
    false
}

/// Stealth headers injected when stealth mode is enabled.
/// These mimic a real browser's default request headers.
const STEALTH_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";
/// Chrome 150 client hint — kept in sync with the UA strings in BUILTIN_UA_POOL.
const STEALTH_SEC_CH_UA: &str =
    r#""Google Chrome";v="150", "Chromium";v="150", "Not_A Brand";v="24""#;

/// Build a configured reqwest client, optionally routed through `proxy`.
///
/// **Strict**: a malformed proxy URL or a client build failure is a hard error
/// — we never silently fall back to a direct (no-proxy) client, which would leak
/// the host's real IP. Reached via [`HttpFetcher::with_timeout`] (infallible —
/// callers pre-validate) and [`HttpFetcher::with_proxy`] (fail-closed per-request
/// path for config rotation + BYOP, where the error path IS reachable).
fn build_client(
    user_agent: &str,
    proxy: Option<&str>,
    request_timeout: std::time::Duration,
    relaxed_tls: bool,
) -> CrwResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(user_agent)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .timeout(request_timeout)
        .redirect(crw_core::url_safety::safe_redirect_policy());

    // Relaxed client used ONLY as a cert-error fallback (see `is_cert_error`):
    // disable cert + hostname verification so a broken chain / expired / self-
    // signed cert no longer blocks an otherwise-fetchable page. SSRF protection
    // is unaffected (it runs on the resolved URL, not the TLS layer).
    if relaxed_tls {
        builder = builder
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true);
    }

    if let Some(proxy_url) = proxy {
        let p = reqwest::Proxy::all(proxy_url).map_err(|e| {
            // NEVER interpolate `proxy_url` itself: it carries `user:pass@`.
            let redacted = crw_core::redact_proxy_url(proxy_url);
            CrwError::ConfigError(format!(
                "invalid proxy URL '{redacted}': {}",
                crw_core::error::reqwest_message(e)
            ))
        })?;
        builder = builder.proxy(p);
    }

    builder.build().map_err(|e| {
        CrwError::ConfigError(format!(
            "failed to build HTTP client: {}",
            crw_core::error::reqwest_message(e)
        ))
    })
}

/// Simple HTTP fetcher using reqwest. No JS rendering.
pub struct HttpFetcher {
    client: reqwest::Client,
    /// Cert-verification-disabled client, built only when
    /// `CRW_HTTP_TLS_RELAXED_FALLBACK` is on. Used solely to retry a fetch that
    /// failed strict TLS verification (`is_cert_error`); `None` keeps behavior
    /// identical to before.
    relaxed_client: Option<reqwest::Client>,
    /// Proxy-routed client, built only when `CRW_HTTP_RATELIMIT_PROXY_URL` is
    /// set. Used solely to retry a fetch the origin rate-limited (429) through a
    /// different egress IP (`is_ratelimit_status`); `None` keeps behavior
    /// identical to before.
    ratelimit_proxy_client: Option<reqwest::Client>,
    /// True when the PRIMARY `client` already egresses through a proxy (config
    /// rotation / BYOP via [`Self::with_proxy`]). Egress provenance is not
    /// otherwise recoverable: `use_proxy` stays `false` for such a fetcher even
    /// though every request leaves through a proxy, so without this flag the
    /// egress-latch write hooks below would record a proxy-observed block as a
    /// DIRECT one and demote every other caller's healthy direct traffic.
    has_static_proxy: bool,
    inject_stealth_headers: bool,
}

impl HttpFetcher {
    pub fn new(user_agent: &str, proxy: Option<&str>, inject_stealth_headers: bool) -> Self {
        Self::with_timeout(
            user_agent,
            proxy,
            inject_stealth_headers,
            HTTP_REQUEST_TIMEOUT,
        )
    }

    /// Did a usable fallback-proxy client actually build? Distinct from "the env
    /// var is set": a malformed `CRW_HTTP_RATELIMIT_PROXY_URL` leaves this `None`,
    /// and callers reasoning about whether a recovery egress EXISTS must not be
    /// fooled by a typo.
    pub fn has_ratelimit_proxy(&self) -> bool {
        self.ratelimit_proxy_client.is_some()
    }

    /// Same as [`Self::new`] but with a caller-supplied request timeout.
    /// Used by `FallbackRenderer` to honor `RendererConfig::http_timeout()`.
    ///
    /// Infallible: callers that pass a `proxy` must pre-validate it (the renderer
    /// does, via `ProxyEntry::parse`, so this never silently falls back for a
    /// configured proxy). The strict per-request path is [`Self::with_proxy`].
    pub fn with_timeout(
        user_agent: &str,
        proxy: Option<&str>,
        inject_stealth_headers: bool,
        request_timeout: std::time::Duration,
    ) -> Self {
        let client = build_client(user_agent, proxy, request_timeout, false).unwrap_or_else(|e| {
            tracing::error!("{e}, using default client");
            reqwest::Client::new()
        });
        let relaxed_client = if tls_relaxed_fallback_enabled() {
            build_client(user_agent, proxy, request_timeout, true).ok()
        } else {
            None
        };
        let ratelimit_proxy_client = ratelimit_proxy_url().and_then(|purl| {
            build_client(user_agent, Some(purl.as_str()), request_timeout, false).ok()
        });
        Self {
            client,
            relaxed_client,
            ratelimit_proxy_client,
            has_static_proxy: proxy.is_some() || env_proxy_configured(),
            inject_stealth_headers,
        }
    }

    /// Build a fetcher bound to a specific proxy. **Fail-closed**: a bad proxy
    /// URL or client build failure is a hard error — never a silent direct
    /// (no-proxy) client. Used for per-request proxy egress (config rotation +
    /// BYOP) so the HTTP path provably uses the selected proxy.
    pub fn with_proxy(
        user_agent: &str,
        proxy_url: &str,
        inject_stealth_headers: bool,
        request_timeout: std::time::Duration,
    ) -> CrwResult<Self> {
        let client = build_client(user_agent, Some(proxy_url), request_timeout, false)?;
        let relaxed_client = if tls_relaxed_fallback_enabled() {
            build_client(user_agent, Some(proxy_url), request_timeout, true).ok()
        } else {
            None
        };
        let ratelimit_proxy_client = ratelimit_proxy_url().and_then(|purl| {
            build_client(user_agent, Some(purl.as_str()), request_timeout, false).ok()
        });
        Ok(Self {
            client,
            relaxed_client,
            ratelimit_proxy_client,
            has_static_proxy: true,
            inject_stealth_headers,
        })
    }
}

#[async_trait]
impl PageFetcher for HttpFetcher {
    async fn fetch(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        _wait_for_ms: Option<u64>,
        deadline: Deadline,
    ) -> CrwResult<FetchResult> {
        if deadline.expired() {
            return Err(CrwError::HttpError(format!(
                "deadline expired before HTTP fetch of {url}"
            )));
        }
        let start = Instant::now();

        // Build a fresh, fully-decorated request for each attempt. Closure
        // captures `self`, `url`, and `headers`; called once per attempt so
        // every retry sends an independent (yet identical) request.
        let build_request = |client: &reqwest::Client| {
            let mut req = client.get(url);
            if self.inject_stealth_headers {
                req = req
                    .header("Accept", STEALTH_ACCEPT)
                    .header("Accept-Language", "en-US,en;q=0.9")
                    .header("Sec-Ch-Ua", STEALTH_SEC_CH_UA)
                    .header("Sec-Ch-Ua-Mobile", "?0")
                    .header("Sec-Ch-Ua-Platform", "\"Windows\"")
                    .header("Sec-Fetch-Dest", "document")
                    .header("Sec-Fetch-Mode", "navigate")
                    .header("Sec-Fetch-Site", "none")
                    .header("Sec-Fetch-User", "?1")
                    .header("Upgrade-Insecure-Requests", "1")
                    .header("Priority", "u=0, i");
            }
            for (k, v) in headers {
                req = req.header(k.as_str(), v.as_str());
            }
            req
        };

        // Single-retry loop on transient errors / 502-503-504. GET is
        // idempotent so this is safe. Each attempt is bounded by the caller's
        // remaining deadline so the request cannot exceed the overall budget.
        let mut attempt: u32 = 0;
        let mut use_relaxed = false;

        // Egress memory: if this host recently hard-blocked our direct egress,
        // start on the proxy instead of re-discovering the block. Without this
        // every URL on a blocking host repeats the whole climb
        // (direct → 429 → proxy retry → JS ladder), which is the 10-20s/URL that
        // made /map time out on sites like Hacker News.
        //
        // We only PREFER the proxy — we never forbid direct (see the rescue arm
        // below). A falsely-latched host whose proxy egress is worse must still be
        // able to succeed, or scrape success would regress.
        let host = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned));
        // The latch is inert unless a proxy exists AND the deadline can afford both
        // a real proxy attempt and a full direct rescue. Splitting a short budget
        // (the SaaS scrape deadline is 5s) would let a hanging proxy starve the
        // direct rescue, failing a request that direct alone would have served —
        // a scrape-success regression. Below the threshold we behave exactly as
        // before the latch existed.
        let proxy_first = match (&host, self.ratelimit_proxy_client.is_some()) {
            (Some(h), true) => {
                deadline.remaining() >= crate::egress::MIN_BUDGET_FOR_LATCH
                    && crate::egress::global().should_proxy(h).await
            }
            _ => false,
        };
        let mut use_proxy = proxy_first;
        let mut direct_rescue_used = false;
        let mut origin_answered = false;
        // Set when the DIRECT egress got NOTHING back from the origin: a connect-phase
        // timeout, i.e. the blackholed SYN. See where it is set for why the shape is
        // narrowed that far and why the 422 below depends on it.
        let mut direct_connect_failed = false;
        // Set when the proxy is armed MID-LOOP (by a 429 or a header-announced
        // challenge) rather than by the latch. Such an attempt needs the same
        // direct-rescue guarantee the latched path gets, or a hung proxy turns a
        // soft response into a hard Timeout.
        let mut armed_mid_loop = false;
        // Narrower: armed specifically by a challenge HEADER. Only this case takes
        // the challenge budget shaping below. The 429 arm predates this change and
        // its budget behaviour is deliberately left byte-identical — the 6s ceiling
        // is derived from the residential exit's distribution on the AWS-WAF hosts
        // in this ticket, which says nothing about the rate-limited population, and
        // capping it would newly time out a slow-but-working exit on the 15s search
        // and 92.5s crawl budgets.
        let mut armed_from_challenge = false;
        if proxy_first {
            let m = crw_core::metrics::metrics();
            m.egress_latch_hit_total.inc();
            // Refresh the gauge on the hot latched path so it tracks the live
            // latched-host count (and its TTL decay), mirroring how
            // `host_preferences_size` is set opportunistically during a fetch.
            m.egress_latched_hosts
                .set(crate::egress::global().latched_hosts() as i64);
        }

        let resp = loop {
            let remaining = deadline.remaining();
            if remaining.is_zero() {
                // Past the budget. Report the budget the caller was given, not
                // how long this call has been running: nothing was awaited here,
                // so elapsed-since-call is a couple of milliseconds and reads as
                // "Timeout after 1ms" to someone who asked for 30s.
                return Err(CrwError::Timeout(deadline.requested_ms()));
            }
            // While trying the proxy first, hold back a FULL direct-rescue budget. A
            // HANGING proxy is the dangerous case: without the reserve it would eat
            // the whole deadline and direct would never run — the very suppression
            // this design exists to avoid.
            //
            // The reserve is flat, not a share of what's left: a share would shrink
            // the rescue on short deadlines to the point where a healthy direct
            // origin no longer fits in it. `proxy_first` already guarantees the
            // budget is at least MIN_BUDGET_FOR_LATCH, so this subtraction always
            // leaves a real proxy attempt behind.
            //
            // The challenge-armed case (`armed_from_challenge`) needs a reserve
            // too — without one, a hung proxy consumes the whole deadline, the
            // rescue arm switches to direct, and the next loop iteration finds
            // zero budget and returns Timeout anyway. But it must NOT use
            // DIRECT_FALLBACK_RESERVE: on a 5s scrape that leaves ~0.9s, which
            // `attempt_budget.is_zero()` immediately hands back to direct, making
            // the whole retry inert.
            //
            // `CHALLENGE_DIRECT_RESERVE`, capped at half of what is left: the
            // direct response this arm rescues to is one we ALREADY received
            // (that is what armed the proxy), so the rescue only has to repeat a
            // request measured at ~70ms — it does not need the full 4s a latched
            // first-contact rescue does. On a 5s scrape that leaves the proxy
            // ~3.4s against a measured p50 of 2.3s; a flat `remaining / 2` left
            // only 2.4s, i.e. 165ms of headroom over p50, which would have made
            // F3a a coin flip on the tightest real budget. The `/ 2` cap keeps a
            // rescue reachable when very little time is left.
            let attempt_budget = if use_proxy && !direct_rescue_used {
                if proxy_first {
                    remaining.saturating_sub(crate::egress::DIRECT_FALLBACK_RESERVE)
                } else if armed_from_challenge {
                    let after_reserve = remaining
                        .saturating_sub(std::cmp::min(remaining / 2, CHALLENGE_DIRECT_RESERVE));
                    // Capped so a large budget does not turn a hanging exit into a
                    // 13.5s wait for the same empty body direct returned in 70ms.
                    std::cmp::min(after_reserve, CHALLENGE_PROXY_MAX)
                } else {
                    remaining
                }
            } else {
                remaining
            };
            if attempt_budget.is_zero() {
                // Not enough left to try the proxy AND still rescue with direct.
                // Spend what remains on direct, which is the attempt we know how to
                // reason about.
                if use_proxy && proxy_first && !direct_rescue_used {
                    use_proxy = false;
                    direct_rescue_used = true;
                    continue;
                }
                // Same rule as the budget check above: report the caller's
                // budget, not the near-zero time spent inside this call.
                return Err(CrwError::Timeout(deadline.requested_ms()));
            }
            let remaining = attempt_budget;
            // On the cert-error fallback path use the verification-disabled
            // client; otherwise the strict client.
            let active_client = if use_proxy {
                self.ratelimit_proxy_client.as_ref().unwrap_or(&self.client)
            } else if use_relaxed {
                self.relaxed_client.as_ref().unwrap_or(&self.client)
            } else {
                &self.client
            };
            let send_fut = build_request(active_client).send();
            let send_result = tokio::time::timeout(remaining, send_fut).await;
            // "The origin answered us at least once on this request." Recorded HERE,
            // once, rather than in the four `Ok(Ok(_))` arms below, so no future arm can
            // forget it. Direct egress only: a response relayed through the proxy could
            // be the proxy's own (407, its 502), which proves nothing about the origin.
            //
            // This is the real predicate behind `direct_connect_failed`. An earlier
            // revision used `!armed_mid_loop`, which only covers 429 and announced
            // challenges — an origin answering 502-504, then connect-timing-out on the
            // retry, still ended up reported as "no response from the origin".
            if !use_proxy && matches!(send_result, Ok(Ok(_))) {
                origin_answered = true;
            }
            match send_result {
                // The proxy-first attempt HUNG until its capped budget ran out.
                // This is the case the reserve exists for: fall back to direct with
                // the budget we held back, instead of failing the whole fetch on a
                // proxy that a latch — possibly a false one — put in front of it.
                Err(_) if use_proxy && (proxy_first || armed_mid_loop) && !direct_rescue_used => {
                    tracing::warn!(
                        "proxy attempt for {url} exhausted its budget while latched; falling back to direct"
                    );
                    use_proxy = false;
                    direct_rescue_used = true;
                }
                Err(_) => {
                    // The origin blackholed our direct SYNs and the proxy rescue then
                    // failed to overturn that, so the original finding stands: nothing
                    // answered for this URL. The verdict rests on the direct blackhole,
                    // never on the proxy hang alone.
                    //
                    // Without this the class surfaced as `Timeout` -> 504, which pages
                    // the 5xx watchdog as OUR outage, tells the caller to raise a
                    // `timeout` for a host with no listening socket, and bills them. The
                    // 422 path already refunds; it was simply unreachable once the proxy
                    // retry was armed in front of it. `Ok(Err(_))` below still keeps a
                    // fast-refusing proxy at 502 (`proxy_connect_failure_is_not_blamed_
                    // on_the_caller`), so a dead pool cannot land here either.
                    if direct_connect_failed {
                        // Logged at this specific classification (rather than relying on the
                        // generic 422 count) so a spike here is greppable on its own: this is
                        // the one 422 shape that used to be a paging 504, so if it is ever OUR
                        // fault (e.g. host-level starvation delaying both egresses' timers
                        // alike) rather than a genuinely dead target, this line is how an
                        // operator tells the two apart instead of the alert going quiet.
                        tracing::warn!(
                            url,
                            "direct connect-timeout and proxy rescue both failed; \
                             reporting target_unreachable (was: timeout)"
                        );
                        return Err(CrwError::TargetUnreachable(format!(
                            "Could not reach {url}: no response from the origin over \
                             either egress"
                        )));
                    }
                    return Err(CrwError::Timeout(remaining.as_millis() as u64));
                }
                Ok(Ok(r))
                    if attempt < HTTP_MAX_RETRIES && is_retriable_status(r.status().as_u16()) =>
                {
                    tracing::debug!(
                        "HTTP {} from {url}, retrying (attempt {})",
                        r.status(),
                        attempt + 1
                    );
                    drop(r);
                    attempt += 1;
                    let backoff = HTTP_RETRY_BACKOFF.min(deadline.remaining());
                    if !backoff.is_zero() {
                        tokio::time::sleep(backoff).await;
                    }
                }
                // Origin rate-limited our egress IP (429) and a fallback proxy
                // is armed: retry ONCE through the proxy (a different egress IP
                // usually clears the limit). Not a transient retry — does not
                // consume the retry budget. Placed before the success arm so the
                // 429 is not returned before the proxy is tried.
                Ok(Ok(r))
                    if !use_proxy
                        // `direct_rescue_used` means we already tried the proxy for
                        // this request and it failed, which is why we are on direct
                        // at all. Bouncing back to that same broken proxy would just
                        // burn the rescue budget on an attempt we know fails.
                        && !direct_rescue_used
                        && self.ratelimit_proxy_client.is_some()
                        && should_arm_proxy(r.status().as_u16(), challenge_header(r.headers())) =>
                {
                    let armed_by_header = challenge_header(r.headers()).is_some();
                    tracing::warn!(
                        "HTTP {} from {url} (origin rate-limited or header-announced challenge); retrying once via proxy (ratelimit_bypassed)",
                        r.status()
                    );
                    drop(r);
                    // The proxy was armed mid-loop, so `proxy_first` is false and
                    // the three rescue arms below would not fire for it. Arm them
                    // explicitly, or a hung proxy eats the whole deadline and turns
                    // a soft empty response into a hard Timeout.
                    armed_mid_loop = true;
                    armed_from_challenge = armed_by_header;
                    // WRITE HOOK — direct-only: this arm is gated on `!use_proxy`,
                    // AND on `!has_static_proxy` because a BYOP/config-rotation
                    // fetcher egresses through a proxy while `use_proxy` stays
                    // false. Remember it, so the next URL on this host starts on
                    // the proxy instead of paying the climb again.
                    //
                    // A block seen *through* a proxy must never land here: it would
                    // say the proxy is blocked, not direct, and would let one
                    // caller's broken proxy demote every other caller's healthy
                    // direct traffic onto paid bandwidth.
                    if let Some(h) = &host
                        && !self.has_static_proxy
                    {
                        let eg = crate::egress::global();
                        eg.note_block(h).await;
                        crw_core::metrics::metrics()
                            .egress_latched_hosts
                            .set(eg.latched_hosts() as i64);
                    }
                    use_proxy = true;
                }
                // The proxy-first attempt did not return a usable page. Direct is
                // not forbidden by a latch — only deprioritized — so spend the
                // reserved budget on one direct attempt rather than returning the
                // proxy's result. This keeps best-result behaviour when the proxy
                // is the worse egress for this host (origin blocks the proxy
                // ranges, bad geo exit, a 403/5xx wall the box's own IP clears).
                //
                // Gate on "not a usable page", which is BROADER than the 429/cf
                // signal the direct-side arm uses: the latch reorders and must
                // never SUPPRESS direct, so ANY non-2xx proxy response (403, 5xx,
                // an un-followed redirect) has to leave a direct rescue reachable,
                // or a falsely-latched host would return the proxy's failure while
                // direct would have served 200 — a scrape-success regression.
                //
                // `cf-mitigated` is folded in explicitly because a Cloudflare
                // challenge is often served as a 200: `is_success()` alone would
                // let that interstitial through and skip the rescue, silently
                // narrowing the very case (`should_arm_proxy(200, true)`) the
                // pre-existing 429 arm already treats as a block. Retriable
                // statuses still exhaust their retries on the proxy first (that arm
                // is matched above); a clean 2xx falls through to `break r` and
                // never wastes the rescue.
                Ok(Ok(r))
                    if use_proxy
                        && (proxy_first || armed_mid_loop)
                        && !direct_rescue_used
                        && (!r.status().is_success()
                            || challenge_header(r.headers()).is_some()) =>
                {
                    tracing::warn!(
                        "HTTP {} from {url} via proxy while latched; falling back to direct",
                        r.status()
                    );
                    drop(r);
                    use_proxy = false;
                    direct_rescue_used = true;
                }
                Ok(Ok(r)) => break r,
                // TLS cert verification failed and relaxed-TLS fallback is armed:
                // swap to the cert-disabled client and retry once. NOT a transient
                // retry — does not consume the retry budget or back off. Placed
                // before the generic retry arm because cert failures are
                // `is_connect()` and would otherwise be retried on the strict
                // client (pointless — the cert is still broken).
                // The proxy-first attempt could not reach the origin AT ALL (proxy
                // down, refused, DNS, misconfigured creds). The latch only expresses
                // a preference, so this must not sink the fetch: spend the reserved
                // budget on direct. Without this arm an unreachable proxy would turn
                // every scrape of a latched host into a hard failure for the whole
                // cooldown — a scrape-success regression, and precisely the case the
                // "reorder, never suppress" rule exists to prevent.
                Ok(Err(_))
                    if use_proxy && (proxy_first || armed_mid_loop) && !direct_rescue_used =>
                {
                    tracing::warn!(
                        "proxy egress failed for {url} while latched; falling back to direct"
                    );
                    use_proxy = false;
                    direct_rescue_used = true;
                }
                Ok(Err(e))
                    if !use_relaxed && self.relaxed_client.is_some() && is_cert_error(&e) =>
                {
                    tracing::warn!(
                        "TLS verification failed for {url} ({e}); retrying once with relaxed TLS (tls_unverified)"
                    );
                    use_relaxed = true;
                }
                // The direct egress could not reach the origin (refused / reset /
                // aborted / connect-timeout blackhole). A direct retry would just hit
                // the same dead path, so switch straight to the fallback proxy — a
                // different egress that often CAN reach the origin. This is the fix for
                // the production case: prod's IPv4 is blocked/blackholed by many origins
                // that the DataImpulse pool reaches in ~1.6s.
                //
                // Placed BEFORE the generic retriable arm so a connection failure spends
                // its one alternate attempt on the proxy rather than wasting the tight
                // 5s budget on a doomed direct retry. `!use_proxy` bounds it to a single
                // switch (worst case: direct, proxy, proxy-retry). Arming does not
                // consume the transient retry budget, matching the 429 arm.
                //
                // Skipped when too little budget remains for a proxy round trip to
                // plausibly complete — this is exactly why HTTP_CONNECT_TIMEOUT was
                // lowered: a blackhole must fail early enough to leave that budget.
                //
                // Note `active_client` prefers the proxy over the relaxed-TLS client, so
                // a site needing BOTH a relaxed cert and a different egress keeps the
                // strict-TLS proxy client and still fails; the 429 arm has the same
                // property and no observed origin needs both.
                Ok(Err(e))
                    if !use_proxy
                        && self.ratelimit_proxy_client.is_some()
                        && deadline.remaining() >= crate::MIN_TIER_BUDGET
                        && is_connection_failure(&e) =>
                {
                    tracing::warn!(
                        "direct egress could not reach {url} ({e}); retrying via proxy (egress_blocked)"
                    );
                    use_proxy = true;
                    // Deliberately narrower than this arm's own guard. Only a
                    // connect-phase TIMEOUT counts: we sent SYNs straight at the origin
                    // and got zero packets back, which is a first-hand observation of
                    // the ORIGIN, not an inference about the hop after it. A refused or
                    // reset connection is excluded even though it also lands here,
                    // because an RST proves a live TCP stack answered.
                    //
                    // That distinction is what makes the 422 below safe. A congested
                    // (rather than down) proxy pool HANGS exactly like a proxy waiting
                    // on a dead origin, so the hang on its own proves nothing; if the
                    // verdict rested on it, our own pool degrading would be laundered
                    // into a flood of caller-blaming refunds and the 5xx watchdog would
                    // go quiet during a real outage. Requiring the direct blackhole
                    // first means proxy congestion alone can never reach 422: a healthy
                    // origin answers the direct attempt and the proxy is never armed.
                    //
                    // `!armed_mid_loop` is the third requirement and it is not
                    // cosmetic: that flag is set only after the origin ANSWERED us
                    // with an HTTP status (a 429 or an announced challenge). If it
                    // did, "no response from the origin" is provably false, and a
                    // later direct-rescue timeout must not be laundered into a 422
                    // that blames — and refunds — a host we demonstrably reached.
                    // Narrowed here at the source rather than at the two arms that
                    // read it, so both inherit the same meaning.
                    direct_connect_failed = e.is_connect() && e.is_timeout() && !origin_answered;
                }
                // Retry once on the same egress. Read-phase timeouts qualify (the origin
                // connected and is slow — a retry may help). Connection-level failures
                // qualify ONLY when no proxy is armed: with a proxy, the arm above already
                // handled them; without one (the OSS/self-host default, where the
                // DataImpulse fallback is unset), this preserves the pre-existing safety
                // net for a transient refused/reset/DNS blip that a retry often clears.
                Ok(Err(e))
                    if attempt < HTTP_MAX_RETRIES
                        && (is_retriable_error(&e)
                            || (self.ratelimit_proxy_client.is_none()
                                && is_connection_failure(&e))) =>
                {
                    tracing::debug!(
                        "transient HTTP error to {url} ({e}), retrying (attempt {})",
                        attempt + 1
                    );
                    attempt += 1;
                    let backoff = HTTP_RETRY_BACKOFF.min(deadline.remaining());
                    if !backoff.is_zero() {
                        tokio::time::sleep(backoff).await;
                    }
                }
                Ok(Err(e)) => {
                    // A connect-phase failure to the ORIGIN (refused / DNS / TLS-handshake /
                    // connect-timeout) means the caller's target is unreachable → 422. But
                    // once we switched to the proxy, a failure may be the proxy infra's
                    // fault, not the origin's, so keep it a 502 (our side) rather than
                    // blaming the caller. A post-handshake reset likewise stays 502.
                    //
                    // `direct_connect_failed` is the one exception, and it is the same
                    // rule the `Err(_)` arm above already applies: it is set ONLY when the
                    // DIRECT egress hit a connect-phase TIMEOUT, a first-hand observation
                    // that the origin swallowed our SYNs. A proxy failure layered on top
                    // does not overturn that. Without this the class exits as `HttpError`,
                    // the ladder never short-circuits (`lib.rs` only stops on
                    // `TargetUnreachable`), and a host with no listening socket is handed
                    // lightpanda's and chrome's full budgets — measured on prod at ~38 s
                    // per request, ~1,050 requests/day. A refused or reset proxy still
                    // lands at 502 because a refusal is not a timeout, which is what
                    // `proxy_connect_failure_is_not_blamed_on_the_caller` pins.
                    if e.is_connect() && use_proxy && direct_connect_failed {
                        // Same greppable signal the `Err(_)` arm emits, and for the same
                        // reason: this is the one 422 shape that used to be a paging 5xx.
                        // If our OWN direct egress is ever blocked host-wide, every origin
                        // starts looking blackholed and this line is how an operator sees
                        // the 422s are ours, not the callers'.
                        tracing::warn!(
                            url,
                            "direct connect-timeout and proxy attempt both failed; \
                             reporting target_unreachable (was: http_error)"
                        );
                    }
                    let unreachable = e.is_connect() && (!use_proxy || direct_connect_failed);
                    // reqwest's " for url (...)" tail carries the request target, not
                    // the proxy, so dropping it here is for uniformity with every other
                    // site. `url` is the caller's own target and safe to echo, so both
                    // arms name it themselves.
                    let msg = crw_core::error::reqwest_message(e);
                    return Err(if unreachable {
                        CrwError::TargetUnreachable(format!("Could not reach {url}: {msg}"))
                    } else {
                        CrwError::HttpError(format!("{url}: {msg}"))
                    });
                }
            }
        };
        let status = resp.status().as_u16();

        // Check content-length before downloading
        if let Some(len) = resp.content_length()
            && len as usize > MAX_RESPONSE_BYTES
        {
            return Err(CrwError::HttpError(format!(
                "Response too large: {len} bytes (max {MAX_RESPONSE_BYTES})"
            )));
        }

        let content_type_header = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let challenge = challenge_header(resp.headers());

        let final_url_str = resp.url().as_str().to_string();

        // Bound the body read by the caller's remaining budget. Without this the
        // read is governed only by the client-level `HTTP_REQUEST_TIMEOUT` (30s),
        // so an origin (or proxy exit) that sends headers promptly and then stalls
        // the body blows straight through a 5-15s request deadline.
        //
        // Floored at MIN_TIER_BUDGET, NOT at ~0: `send()` resolves on HEADERS, and
        // the attempt above may legitimately have consumed almost the whole
        // deadline reaching them. A bare `remaining` would hand a slow-TTFB origin
        // a sliver of a millisecond to stream a body it would have delivered in
        // 200ms — turning "late but complete" into a hard error, which is strictly
        // worse than the 30s blow-through this bound exists to stop (`Ok` counts
        // toward scrape success; `Err` does not).
        //
        // ponytail: known residual, deliberately not fixed here. The loop exits on
        // a clean 2xx BEFORE the body is known, so a challenge-armed proxy that
        // sends 200 headers and then stalls yields an error rather than falling
        // back to direct — the same shape the pre-existing 429 arm has always had.
        // Fixing it properly means restructuring `fetch` so the body read sits
        // inside the retry loop; that is a bigger change than this ticket, and the
        // failure needs a proxy that answers headers and then dies.
        let bytes = match tokio::time::timeout(
            deadline.remaining().max(crate::MIN_TIER_BUDGET),
            read_body_capped(resp, MAX_RESPONSE_BYTES),
        )
        .await
        {
            Ok(r) => r?,
            Err(_) => {
                return Err(CrwError::Timeout(
                    (start.elapsed().as_millis().max(1)) as u64,
                ));
            }
        };

        let result = build_http_fetch_result(
            url,
            status,
            content_type_header.as_deref(),
            challenge,
            &final_url_str,
            &bytes,
            start.elapsed().as_millis() as u64,
            "http",
        )?;

        // SECOND WRITE HOOK: body-verdict blocks that carry no header and no
        // block status, which is how Wikimedia serves its datacenter-IP ban
        // (the canonical footer phrase in a `<body>`-less shell; see
        // `detector::looks_like_generic_bot_wall`). The header hook above cannot
        // see those, and the arm it sits in never fires for them, so without this
        // every URL on such a host re-climbs the whole doomed ladder.
        //
        // Runs on the assembled result rather than mid-tail on purpose: this is
        // the only point where the decoded body and the egress provenance
        // (`use_proxy` / `has_static_proxy`) are both in scope. Latching from the
        // renderer would be unable to tell a direct block from a proxied one, and
        // a proxy-observed block would re-latch on every request, so the TTL
        // would never expire, direct would never be re-probed and the host would
        // be pinned to paid egress permanently.
        //
        // Fingerprint walls are excluded: a residential IP does not clear a
        // Cloudflare managed challenge or a vendor SDK wall, so latching one only
        // burns paid bandwidth.
        //
        // ponytail: `antibot::classify` deliberately does not run on this tier, so
        // a vendor wall recognisable only from visible text (a PerimeterX/Imperva
        // page with no SDK marker) is not excluded here and can latch for the
        // 10-minute TTL. Bounded: the latch only reorders egress, never suppresses
        // direct. Upgrade path if it ever matters: thread the classifier verdict
        // down instead of adding a second classify() call to this hot path.
        if !use_proxy
            && !self.has_static_proxy
            && result.content_type.as_deref() != Some("application/pdf")
            && let Some(h) = &host
            && crate::detector::looks_like_generic_bot_wall(&result.html, false)
            && !crate::detector::looks_like_cloudflare_challenge(&result.html)
            && crate::detector::looks_like_vendor_block(&result.html).is_none()
        {
            let eg = crate::egress::global();
            eg.note_block(h).await;
            crw_core::metrics::metrics()
                .egress_latched_hosts
                .set(eg.latched_hosts() as i64);
            tracing::info!(
                url,
                status,
                "direct egress hit an IP-reputation block; latching host to proxy-first"
            );
        }

        Ok(result)
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

/// Extract the `charset` label from a `Content-Type` header value
/// (e.g. `text/html; charset=ISO-8859-1` → `ISO-8859-1`).
fn charset_from_content_type(ct: &str) -> Option<String> {
    let lower = ct.to_ascii_lowercase();
    let idx = lower.find("charset")?;
    let after = ct[idx + "charset".len()..].trim_start();
    let after = after.strip_prefix('=')?.trim_start();
    let after = after.trim_start_matches(['"', '\'']);
    let end = after
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':'))
        .unwrap_or(after.len());
    let label = after[..end].trim();
    (!label.is_empty()).then(|| label.to_string())
}

/// Read a response body, refusing to buffer past `max` bytes.
///
/// The `Content-Length` pre-check at the call site is necessary but not
/// sufficient: the header is absent on a chunked response and on a
/// transport-decompressed one, so the cap has to be applied to the decoded
/// bytes as they arrive. See [`crw_core::body::read_capped`].
pub(crate) async fn read_body_capped(resp: reqwest::Response, max: usize) -> CrwResult<Vec<u8>> {
    let (body, truncated) = crw_core::body::read_capped(resp.bytes_stream(), max)
        .await
        .map_err(|e| CrwError::HttpError(crw_core::error::reqwest_message(e)))?;
    if truncated {
        return Err(CrwError::HttpError(format!(
            "Response too large: exceeds {max} bytes"
        )));
    }
    Ok(body)
}

#[cfg(test)]
pub(crate) fn serve_raw(head: impl Into<String>, body: Vec<u8>) -> String {
    let head = head.into();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        use std::io::{Read, Write};

        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&body);
    });
    format!("http://{addr}/")
}

/// Sniff a `<meta charset>` / `<meta http-equiv=content-type … charset=…>`
/// declaration from the first ~2KB of an HTML document.
fn sniff_meta_charset(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(2048)];
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    let idx = text.find("charset")?;
    let after = text[idx + "charset".len()..].trim_start();
    let after = after.strip_prefix('=')?.trim_start();
    let after = after.trim_start_matches(['"', '\'']);
    let end = after
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(after.len());
    let label = &after[..end];
    (!label.is_empty()).then(|| label.to_string())
}

/// True when a response body is binary rather than text. A NUL byte in the
/// first 1KB is the standard heuristic (it is the one git uses): no HTML, JSON,
/// XML, CSV or plain-text document in a single-byte or UTF-8 encoding carries
/// one, while ZIP containers (.docx/.xlsx/.pptx), images and archives hit one
/// within a few bytes.
///
/// UTF-16 documents are legitimately NUL-rich, so a declared UTF-16 charset opts
/// out. The verdict comes from `encoding_rs` rather than a substring test on the
/// label, because `decode_html_bytes` resolves the HEADER label the same way and
/// the two must not disagree about it: a hand-written list misses
/// `charset=unicode` and `charset=csunicode`, both of which
/// `Encoding::for_label` maps to UTF-16LE and classic IIS still emits.
///
/// The agreement stops at the header label. `decode_html_bytes` also falls back
/// to a `<meta charset>` sniff, and lets `Encoding::decode` override the label
/// from a BOM; neither is mirrored here. Both gaps need a wide encoding under a
/// non-HTML content type to matter at all, since a declared HTML-ish type skips
/// this function outright, and `sniff_meta_charset` cannot read UTF-16 anyway.
pub(crate) fn looks_binary(bytes: &[u8], header_charset: Option<&str>) -> bool {
    // `for_label` lowercases and trims the label itself, so no normalisation here.
    if let Some(enc) = header_charset.and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        && (enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE)
    {
        return false;
    }
    bytes[..bytes.len().min(1024)].contains(&0)
}

/// Decode fetched HTML bytes to a `String` honoring the declared charset
/// (P1-1): HTTP `Content-Type` charset first, then a `<meta charset>` sniff,
/// then UTF-8. Without this, a Latin-1 / Windows-1252 page has every 0x80–0xFF
/// byte replaced with U+FFFD.
fn decode_html_bytes(bytes: &[u8], header_charset: Option<&str>) -> String {
    // Header charset wins, but a bogus/unknown header label must still fall
    // through to a <meta charset> sniff before giving up on UTF-8.
    let enc = header_charset
        .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        .or_else(|| {
            sniff_meta_charset(bytes).and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        });
    match enc {
        Some(enc) => enc.decode(bytes).0.into_owned(),
        None => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Shared response tail for the two plain-HTTP fetchers (the reqwest
/// [`HttpFetcher`] and the wreq `ImpersonatedFetcher`): size cap on the read
/// body, content-type routing (the `%PDF-` sniff and relabel, the binary-body
/// rejection), charset-aware decode, and [`FetchResult`] assembly. Each
/// transport keeps its own send/retry machinery, response-metadata extraction,
/// and tier-specific side effects (the egress latch below stays in
/// `HttpFetcher::fetch`).
///
/// `tier_name` is the `rendered_with` label for non-PDF bodies (`"http"` or
/// `"impersonated-http"`); a PDF always reports `"pdf"`, as both callers
/// always did.
// Eight scalar inputs is the honest shape here: a struct would just carry the
// same fields once per caller. Same allowance as `scrape_url_inner`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_http_fetch_result(
    url: &str,
    status: u16,
    content_type_header: Option<&str>,
    challenge: Option<ChallengeHeader>,
    final_url_str: &str,
    bytes: &[u8],
    elapsed_ms: u64,
    tier_name: &str,
) -> CrwResult<FetchResult> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(CrwError::HttpError(format!(
            "Response too large: {} bytes (max {MAX_RESPONSE_BYTES})",
            bytes.len()
        )));
    }

    let mut content_type =
        content_type_header.map(|s| s.split(';').next().unwrap_or(s).trim().to_lowercase());
    // Charset from the Content-Type header (P1-1): pages served as Latin-1 /
    // Windows-1252 would otherwise be UTF-8-lossy'd, turning each 0x80–0xFF
    // byte into U+FFFD. Kept separately since `content_type` drops it.
    let header_charset = content_type_header.and_then(charset_from_content_type);

    // Route on the actual bytes, not only on the declared type. Keying the
    // PDF branch on `content_type == "application/pdf"` and treating
    // EVERYTHING else as text means a body that is neither HTML nor a
    // correctly-labelled PDF gets UTF-8-lossy'd and handed to the HTML
    // extractor: a .docx/.xlsx/.pptx comes back as `markdown` beginning
    // "PK\u{3}\u{4}...[Content_Types].xml" under `success: true`, which a
    // caller cannot tell apart from a real scrape.
    if bytes.starts_with(b"%PDF-") {
        // A PDF served as octet-stream (or text/html). Relabel it so the
        // downstream PDF branch in crw-crawl, which gates on the same
        // content type, engages instead of extracting an empty body.
        content_type = Some("application/pdf".to_string());
    }
    // Computed AFTER the relabel, so a sniffed PDF and a declared one are
    // one case from here down rather than a disjunction repeated at every use.
    let is_pdf = content_type.as_deref() == Some("application/pdf");
    // The NUL test applies only when the origin did NOT declare an HTML-ish
    // type. A page served as `text/html` with a stray NUL in it renders
    // fine in a real browser (the HTML5 tokenizer maps NUL to U+FFFD), so
    // rejecting one would cost a page we scrape today, and would hand any
    // origin a one-byte way to shut the ladder down that costs it nothing
    // with human visitors. An undeclared or empty type stays in scope: a
    // body with no `Content-Type` at all is exactly what the sniff is for.
    // `is_html_like_content_type` answers true for an empty type as well as
    // for `None`, so the emptiness is checked here: an origin that sends a
    // bare `Content-Type:` has declared nothing, and treating that as a
    // declaration of HTML would let a .docx back through the hole this
    // exists to close.
    let declared_html = content_type
        .as_deref()
        .is_some_and(|ct| !ct.is_empty() && crate::is_html_like_content_type(Some(ct)));
    let (html, raw_bytes) = if is_pdf {
        (String::new(), Some(bytes.to_vec()))
    } else if !declared_html && looks_binary(bytes, header_charset.as_deref()) {
        // Logged rather than silent: this is the one path that returns a
        // hard error without climbing the ladder, so if a class of real
        // pages ever lands here it has to be visible in production.
        tracing::info!(
            url,
            content_type = content_type.as_deref().unwrap_or("none"),
            bytes = bytes.len(),
            "binary body, returning unsupported content type without escalating"
        );
        return Err(CrwError::UnsupportedContentType(format!(
            "{} ({} bytes): the body is binary, not HTML and not a PDF, \
             so there is nothing to extract",
            content_type.as_deref().unwrap_or("no content-type"),
            bytes.len()
        )));
    } else {
        (decode_html_bytes(bytes, header_charset.as_deref()), None)
    };

    let final_url = if final_url_str != url {
        Some(final_url_str.to_string())
    } else {
        None
    };

    Ok(FetchResult {
        url: url.to_string(),
        final_url,
        status_code: status,
        html,
        content_type,
        raw_bytes,
        rendered_with: if is_pdf {
            Some("pdf".to_string())
        } else {
            Some(tier_name.to_string())
        },
        elapsed_ms,
        warning: challenge.map(|c| c.marker().to_string()),
        render_decision: None,
        credit_cost: 0,
        warnings: challenge
            .map(|c| vec![c.warning_text().to_string()])
            .unwrap_or_default(),
        truncated: false,
        deadline_exceeded: false,
        captured_responses: Vec::new(),
        // Neither plain-HTTP tier renders or captures a screenshot.
        screenshot: None,
        block: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── looks_binary ────────────────────────────────────────────────────
    #[test]
    fn looks_binary_flags_a_zip_container() {
        // Opening bytes of any .docx/.xlsx/.pptx.
        assert!(looks_binary(b"PK\x03\x04\x14\x00\x08\x00\x00\x00", None));
    }

    #[test]
    fn looks_binary_passes_html_and_empty_bodies() {
        assert!(!looks_binary(
            b"<!doctype html><html><body>hi</body></html>",
            None
        ));
        assert!(!looks_binary(b"", None));
    }

    #[test]
    fn looks_binary_respects_a_declared_wide_charset() {
        // UTF-16LE "hi": NUL-rich, but genuinely text.
        assert!(!looks_binary(b"h\x00i\x00", Some("utf-16le")));
        assert!(looks_binary(b"h\x00i\x00", None));
    }

    #[test]
    fn looks_binary_accepts_every_utf16_label_decode_html_bytes_accepts() {
        // The two functions must agree on what counts as UTF-16, or a page one
        // of them decodes the other rejects as binary. `unicode` in particular
        // is what classic IIS emits, and a substring test on the label misses
        // it: the page came back 422 instead of its text.
        for label in [
            "utf-16",
            "UTF-16LE",
            "utf-16be",
            "ucs-2",
            "unicode",
            "csunicode",
            "unicodefeff",
            "unicodefffe",
            "iso-10646-ucs-2",
        ] {
            assert!(
                encoding_rs::Encoding::for_label(label.as_bytes()).is_some(),
                "{label} is no longer a charset label, drop it from this test"
            );
            assert!(
                !looks_binary(b"h\x00i\x00", Some(label)),
                "{label} decodes as UTF-16 but was called binary"
            );
        }
    }

    #[test]
    fn looks_binary_ignores_a_charset_that_is_not_wide() {
        // A single-byte or UTF-8 label buys no exemption: those encodings never
        // carry a NUL, so one means the header is lying about the body.
        // `utf-32` is in this list on purpose: WHATWG has no UTF-32, so
        // `for_label` rejects the label and `decode_html_bytes` cannot decode
        // such a body either. An honest refusal beats handing back mojibake.
        for label in [
            "utf-8",
            "windows-1252",
            "iso-8859-1",
            "shift_jis",
            "utf-32",
            "not-a-charset",
        ] {
            assert!(
                looks_binary(b"PK\x03\x04\x14\x00", Some(label)),
                "{label} should not exempt a ZIP container"
            );
        }
    }

    /// Guards every test below that mutates process-wide env vars
    /// (`CRW_HTTP_TLS_RELAXED_FALLBACK`, `CRW_HTTP_RATELIMIT_PROXY_URL`,
    /// `HTTP_PROXY`/etc, `CRW_ALLOW_LOOPBACK_FOR_TESTS`). `cargo test` runs
    /// tests in the same process on multiple threads, so without this two such
    /// tests can race and read each other's half-set state. Same pattern as
    /// `crw-core::config::tests::ENV_LOCK`.
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    fn clear_proxy_env() {
        for k in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            unsafe { std::env::remove_var(k) };
        }
    }

    async fn spawn_router(router: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[test]
    fn should_arm_proxy_truth_table() {
        use ChallengeHeader::{AwsWaf, CloudflareMitigated};
        assert!(should_arm_proxy(429, None), "429 arms on its own");
        assert!(
            !should_arm_proxy(403, None),
            "403 without header does not arm"
        );
        assert!(
            should_arm_proxy(403, Some(CloudflareMitigated)),
            "403 + cf-mitigated arms"
        );
        assert!(
            should_arm_proxy(200, Some(CloudflareMitigated)),
            "challenge served as 200 arms"
        );
        assert!(!should_arm_proxy(200, None), "clean 200 does not arm");
        // The AWS-WAF shape: 202 with an empty body. Nothing in the status or
        // the body says "blocked", so without the header this response is
        // indistinguishable from a legitimate empty 202.
        assert!(
            should_arm_proxy(202, Some(AwsWaf)),
            "202 + x-amzn-waf-action arms"
        );
        assert!(!should_arm_proxy(202, None), "bare 202 does not arm");
    }

    fn header_map(pairs: &[(&str, &str)]) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    /// The two header predicates must NOT share a value list: `cf-mitigated`
    /// uses challenge|block, `x-amzn-waf-action` uses challenge|captcha. Merging
    /// them would silently drop `block` from the pre-existing Cloudflare path.
    #[test]
    fn challenge_header_keeps_the_two_value_lists_separate() {
        use ChallengeHeader::{AwsWaf, CloudflareMitigated};
        assert_eq!(
            challenge_header(&header_map(&[("cf-mitigated", "block")])),
            Some(CloudflareMitigated),
            "cf-mitigated: block must still arm"
        );
        assert_eq!(
            challenge_header(&header_map(&[("x-amzn-waf-action", "captcha")])),
            Some(AwsWaf)
        );
        assert_eq!(
            challenge_header(&header_map(&[("x-amzn-waf-action", "CHALLENGE")])),
            Some(AwsWaf),
            "header values are matched case-insensitively"
        );
        assert_eq!(
            challenge_header(&header_map(&[("x-amzn-waf-action", "block")])),
            None,
            "`block` is not a documented value of this header"
        );
        assert_eq!(challenge_header(&header_map(&[])), None);
        // The customer-visible text must name the right vendor.
        assert!(AwsWaf.warning_text().contains("AWS WAF"));
        assert!(CloudflareMitigated.warning_text().contains("Cloudflare"));
    }

    /// Complete the handshake, read the request, THEN abort with RST
    /// (unix only: forcing an RST needs SO_LINGER; on other platforms `close()`
    /// sends FIN and the test would assert the wrong error class).
    /// This is the WAF-style block seen in production: the
    /// origin inspects the request before rejecting it. Reading first is what makes
    /// the test deterministic — it forces the error into hyper's send-request phase
    /// rather than the connect phase.
    #[cfg(unix)]
    fn spawn_resetting_origin() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::Read;
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                #[cfg(unix)]
                unsafe {
                    use std::os::fd::AsRawFd;
                    let l = libc::linger {
                        l_onoff: 1,
                        l_linger: 0,
                    };
                    libc::setsockopt(
                        stream.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_LINGER,
                        std::ptr::from_ref(&l).cast(),
                        std::mem::size_of::<libc::linger>() as libc::socklen_t,
                    );
                }
                drop(stream);
            }
        });
        format!("http://{addr}/")
    }

    /// A reset on an established connection must arm the proxy. `is_connect()` is
    /// false here — which is exactly why the predicate cannot be built on it. This
    /// mirrors the production trace for `sabir.com`, where the engine reported
    /// `HttpError` ("HTTP request failed") rather than `TargetUnreachable`, proving
    /// `is_connect()` was false for the real block.
    #[cfg(unix)]
    #[tokio::test]
    async fn connection_failure_catches_connection_reset() {
        let url = spawn_resetting_origin();
        let err = reqwest::Client::new().get(&url).send().await.unwrap_err();
        assert!(
            !err.is_connect(),
            "a post-handshake reset must not be is_connect(); \
             if this ever flips, the arm ordering below needs revisiting"
        );
        assert!(is_connection_failure(&err), "reset must arm the proxy");
    }

    /// A refused connection (nothing listening) must also arm the proxy.
    #[tokio::test]
    async fn connection_failure_catches_connection_refused() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let err = reqwest::Client::new()
            .get(format!("http://{addr}/"))
            .send()
            .await
            .unwrap_err();
        assert!(is_connection_failure(&err), "refused must arm the proxy");
    }

    /// A connect-phase TIMEOUT (blackhole that drops our SYN) must arm the proxy —
    /// this is the dominant production block. It is distinguished from a read timeout
    /// by `is_connect() && is_timeout()`. 192.0.2.1 is RFC 5737 TEST-NET-1, guaranteed
    /// never routed, so the SYN is blackholed and the connect times out.
    #[tokio::test]
    async fn connection_failure_catches_connect_timeout() {
        let err = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_millis(300))
            .build()
            .unwrap()
            .get("http://192.0.2.1/")
            .send()
            .await
            .unwrap_err();
        assert!(
            err.is_connect() && err.is_timeout(),
            "guard the assumption: a blackhole is a connect-phase timeout (got {err:?})"
        );
        assert!(
            is_connection_failure(&err),
            "connect-timeout blackhole must arm the proxy"
        );
    }

    /// A READ timeout (origin accepted the connection, then stalled) must NOT arm the
    /// proxy — a different egress cannot make a slow origin faster.
    #[tokio::test]
    async fn connection_failure_ignores_read_timeout() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::sleep(std::time::Duration::from_secs(30));
                drop(stream);
            }
        });
        let err = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(400))
            .build()
            .unwrap()
            .get(format!("http://{addr}/"))
            .send()
            .await
            .unwrap_err();
        assert!(
            err.is_timeout() && !err.is_connect(),
            "guard the assumption: a post-connect stall is a read timeout (got {err:?})"
        );
        assert!(
            !is_connection_failure(&err),
            "a read timeout must not arm the proxy"
        );
    }

    /// Minimal forward proxy: reads the absolute-URI request reqwest sends for a
    /// plain-HTTP proxied GET and answers 200 with a marker body.
    #[cfg(unix)]
    fn spawn_stub_proxy() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                const BODY: &str = "<html><body>served via proxy</body></html>";
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
                        BODY.len()
                    )
                    .as_bytes(),
                );
            }
        });
        format!("http://{addr}")
    }

    /// End-to-end: an origin that resets the connection is retried through the
    /// fallback proxy and succeeds. This is the production `sabir.com` case
    /// (prod egress IP refused, proxy reaches it in ~1.6s). Without the
    /// `is_connection_failure` arm the request dies with `HttpError` and the caller
    /// sees a 502.
    #[cfg(unix)]
    #[tokio::test]
    async fn connection_reset_is_retried_through_proxy() {
        let origin = spawn_resetting_origin();
        let proxy = spawn_stub_proxy();

        let fetcher = HttpFetcher {
            client: reqwest::Client::new(),
            relaxed_client: None,
            has_static_proxy: false,
            ratelimit_proxy_client: Some(
                build_client(
                    "test-ua",
                    Some(&proxy),
                    std::time::Duration::from_secs(5),
                    false,
                )
                .unwrap(),
            ),
            inject_stealth_headers: false,
        };

        let res = fetcher
            .fetch(
                &origin,
                &HashMap::new(),
                None,
                Deadline::from_request_ms(10_000),
            )
            .await
            .expect("reset origin must be recovered through the proxy");
        assert!(
            res.html.contains("served via proxy"),
            "expected the proxy's body, got: {}",
            res.html
        );
    }

    /// Without an armed proxy the same reset still fails — the arm is what fixes
    /// it, not some incidental retry.
    #[cfg(unix)]
    #[tokio::test]
    async fn connection_reset_without_proxy_still_fails() {
        let origin = spawn_resetting_origin();
        let fetcher = HttpFetcher {
            client: reqwest::Client::new(),
            relaxed_client: None,
            ratelimit_proxy_client: None,
            has_static_proxy: false,
            inject_stealth_headers: false,
        };
        let err = fetcher
            .fetch(
                &origin,
                &HashMap::new(),
                None,
                Deadline::from_request_ms(10_000),
            )
            .await
            .expect_err("no proxy armed => the reset must surface as an error");
        assert!(
            matches!(err, CrwError::HttpError(_) | CrwError::TargetUnreachable(_)),
            "unexpected error variant: {err:?}"
        );
    }

    /// Like `spawn_resetting_origin` but counts how many connections it served, so a
    /// test can prove a connection failure was retried.
    #[cfg(unix)]
    fn spawn_resetting_counter() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = counter.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::Read;
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                unsafe {
                    use std::os::fd::AsRawFd;
                    let l = libc::linger {
                        l_onoff: 1,
                        l_linger: 0,
                    };
                    libc::setsockopt(
                        stream.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_LINGER,
                        std::ptr::from_ref(&l).cast(),
                        std::mem::size_of::<libc::linger>() as libc::socklen_t,
                    );
                }
                drop(stream);
            }
        });
        (format!("http://{addr}/"), counter)
    }

    /// No proxy armed: a connection failure must still get one direct retry, so the
    /// self-host path keeps its pre-existing resilience to a transient blip.
    #[cfg(unix)]
    #[tokio::test]
    async fn connection_failure_without_proxy_retries_once() {
        let (origin, hits) = spawn_resetting_counter();
        let fetcher = HttpFetcher {
            client: reqwest::Client::new(),
            relaxed_client: None,
            ratelimit_proxy_client: None,
            has_static_proxy: false,
            inject_stealth_headers: false,
        };
        let _ = fetcher
            .fetch(
                &origin,
                &HashMap::new(),
                None,
                Deadline::from_request_ms(10_000),
            )
            .await;
        // Initial attempt + one retry = 2 connects. Without the no-proxy retry branch
        // it would be 1.
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "a connection failure with no proxy must be retried once directly"
        );
    }

    /// When the proxy is tried and it ALSO fails to connect, the error must stay a
    /// 502-class HttpError, not a 422 TargetUnreachable: a proxy connect failure can
    /// be our infra, not proof the caller's target is dead.
    #[tokio::test]
    async fn proxy_connect_failure_is_not_blamed_on_the_caller() {
        // origin: refused. proxy: also refused (closed port).
        let o = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let oaddr = o.local_addr().unwrap();
        drop(o);
        let p = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let paddr = p.local_addr().unwrap();
        drop(p);
        let fetcher = HttpFetcher {
            client: reqwest::Client::new(),
            relaxed_client: None,
            has_static_proxy: false,
            ratelimit_proxy_client: Some(
                build_client(
                    "ua",
                    Some(&format!("http://{paddr}")),
                    std::time::Duration::from_secs(5),
                    false,
                )
                .unwrap(),
            ),
            inject_stealth_headers: false,
        };
        let err = fetcher
            .fetch(
                &format!("http://{oaddr}/"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(10_000),
            )
            .await
            .expect_err("both origin and proxy refuse");
        assert!(
            matches!(err, CrwError::HttpError(_)),
            "a proxy-side failure must not be reported as TargetUnreachable (422); got {err:?}"
        );
    }

    /// The counterpart to `proxy_connect_failure_is_not_blamed_on_the_caller`: when the
    /// origin blackholes our direct SYNs AND the proxy then HANGS instead of refusing,
    /// the origin is the root cause and the caller must get a 422, not a 504.
    ///
    /// The two tests together pin the discriminator: a proxy that is DOWN refuses fast
    /// and stays a 502 (our fault), while a hanging proxy only ever confirms a verdict
    /// the DIRECT attempt already reached on its own. Without this, a DNS-resolvable
    /// host with no listening socket pages the 5xx watchdog, tells the caller to raise
    /// a `timeout` that cannot help, and is billed rather than refunded.
    #[tokio::test]
    async fn direct_blackhole_then_hanging_proxy_is_target_unreachable() {
        // proxy: accepts the connection but never answers, the hang shape.
        let p = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let paddr = p.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = p.accept().await {
                held.push(sock); // hold it open, write nothing
            }
        });
        let fetcher = HttpFetcher {
            // Short connect timeout so the direct blackhole resolves well inside the
            // deadline and still leaves MIN_TIER_BUDGET to arm the proxy.
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_millis(300))
                .build()
                .unwrap(),
            relaxed_client: None,
            has_static_proxy: false,
            ratelimit_proxy_client: Some(
                build_client(
                    "ua",
                    Some(&format!("http://{paddr}")),
                    std::time::Duration::from_secs(5),
                    false,
                )
                .unwrap(),
            ),
            inject_stealth_headers: false,
        };
        // origin: 192.0.2.1 is RFC 5737 TEST-NET-1, never routed, so the SYN is
        // blackholed and the direct attempt is a connect-phase timeout. Same address
        // `connection_failure_catches_connect_timeout` relies on.
        let err = fetcher
            .fetch(
                "http://192.0.2.1/",
                &HashMap::new(),
                None,
                Deadline::from_request_ms(2_000),
            )
            .await
            .expect_err("origin blackholes and the proxy never answers");
        assert!(
            matches!(err, CrwError::TargetUnreachable(_)),
            "a blackholing origin behind a reachable-but-hanging proxy must surface as \
             TargetUnreachable (422), not a 504; got {err:?}"
        );
    }

    /// The third shape, and the one prod actually produces: the origin blackholes our
    /// direct SYNs and the proxy then fails FAST (refused / DNS / TLS) rather than
    /// hanging. That lands on `Ok(Err(_))`, not the `Err(_)` timeout arm, so before this
    /// it exited as `HttpError` and the JS ladder never short-circuited — measured on
    /// prod at ~38 s per request across ~1,050 requests/day, and 895 proxy arms in a
    /// single 6-hour window with the existing 422 path firing zero times.
    ///
    /// Sits between the two tests above: the discriminator is still the DIRECT
    /// connect-timeout, never the proxy's failure mode.
    #[tokio::test]
    async fn direct_blackhole_then_refusing_proxy_is_target_unreachable() {
        // proxy: closed port, so the connect fails immediately (the fast-fail shape).
        let p = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let paddr = p.local_addr().unwrap();
        drop(p);
        let fetcher = HttpFetcher {
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_millis(300))
                .build()
                .unwrap(),
            relaxed_client: None,
            has_static_proxy: false,
            ratelimit_proxy_client: Some(
                build_client(
                    "ua",
                    Some(&format!("http://{paddr}")),
                    std::time::Duration::from_secs(5),
                    false,
                )
                .unwrap(),
            ),
            inject_stealth_headers: false,
        };
        let err = fetcher
            .fetch(
                "http://192.0.2.1/",
                &HashMap::new(),
                None,
                Deadline::from_request_ms(2_000),
            )
            .await
            .expect_err("origin blackholes and the proxy refuses");
        assert!(
            matches!(err, CrwError::TargetUnreachable(_)),
            "a blackholing origin must surface as TargetUnreachable (422) even when the \
             proxy fails fast rather than hanging; got {err:?}"
        );
    }

    /// The revenue guard: an origin that REFUSES our direct connection is alive at the
    /// TCP layer, so a later proxy hang must NOT be read as a dead target. Only a
    /// direct blackhole earns the refunded 422; anything else stays a billed 504, which
    /// also keeps a congested proxy pool visible to the 5xx watchdog instead of
    /// laundering it into caller-blaming refunds.
    #[tokio::test]
    async fn direct_refusal_then_hanging_proxy_stays_a_timeout() {
        let o = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let oaddr = o.local_addr().unwrap();
        drop(o); // closed port: refuses with an RST rather than blackholing
        let p = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let paddr = p.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = p.accept().await {
                held.push(sock);
            }
        });
        let fetcher = HttpFetcher {
            client: reqwest::Client::new(),
            relaxed_client: None,
            has_static_proxy: false,
            ratelimit_proxy_client: Some(
                build_client(
                    "ua",
                    Some(&format!("http://{paddr}")),
                    std::time::Duration::from_secs(5),
                    false,
                )
                .unwrap(),
            ),
            inject_stealth_headers: false,
        };
        let err = fetcher
            .fetch(
                &format!("http://{oaddr}/"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(2_000),
            )
            .await
            .expect_err("origin refuses and the proxy never answers");
        assert!(
            matches!(err, CrwError::Timeout(_)),
            "a live-but-refusing origin must stay a billed 504, not become a refunded \
             422; got {err:?}"
        );
    }

    /// A DNS failure must NOT arm the proxy: it resolves the same name, so the extra
    /// round trip buys nothing. Written to be network-tolerant: if the environment's
    /// resolver hijacks NXDOMAIN and returns a response, there is no error to classify
    /// and the assertion is vacuous rather than flaky.
    #[tokio::test]
    async fn connection_failure_ignores_dns_failure() {
        let res = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap()
            .get("http://nonexistent.invalid./")
            .send()
            .await;
        if let Err(err) = res {
            assert!(
                !is_connection_failure(&err),
                "DNS failure must not arm the proxy (got {err:?})"
            );
        }
    }

    #[test]
    fn charset_from_content_type_parses_label() {
        assert_eq!(
            charset_from_content_type("text/html; charset=ISO-8859-1").as_deref(),
            Some("ISO-8859-1")
        );
        assert_eq!(
            charset_from_content_type("text/html;charset=\"utf-8\"").as_deref(),
            Some("utf-8")
        );
        assert_eq!(charset_from_content_type("text/html").as_deref(), None);
    }

    #[test]
    fn decode_latin1_via_header_no_replacement_char() {
        // "café ûber" in ISO-8859-1: é=0xE9, û=0xFB.
        let bytes = b"caf\xE9 \xFBber";
        let out = decode_html_bytes(bytes, Some("iso-8859-1"));
        assert_eq!(out, "café ûber");
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn decode_windows1254_via_meta_sniff() {
        // Turkish "için" in Windows-1254: i=0x69, ç=0xE7, i=0x69, n=0x6E.
        let bytes = b"<meta charset=windows-1254><p>i\xE7in</p>";
        let out = decode_html_bytes(bytes, None);
        assert!(out.contains("için"), "got: {out}");
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn decode_bogus_header_falls_through_to_meta_then_utf8() {
        // Bogus header label must NOT short-circuit to UTF-8 — a valid <meta>
        // charset should still win. Turkish "için" in Windows-1254.
        let bytes = b"<meta charset=windows-1254><p>i\xE7in</p>";
        let out = decode_html_bytes(bytes, Some("x-bogus-nonsense"));
        assert!(out.contains("için"), "got: {out}");
        // Bogus header + no meta → UTF-8 lossy fallback (no panic).
        let plain = decode_html_bytes(b"plain ascii", Some("x-bogus"));
        assert_eq!(plain, "plain ascii");
    }

    #[test]
    fn decode_utf8_unchanged() {
        let bytes = "café İstanbul 東京".as_bytes();
        assert_eq!(
            decode_html_bytes(bytes, Some("utf-8")),
            "café İstanbul 東京"
        );
        // No charset info → still UTF-8 by default.
        assert_eq!(decode_html_bytes(bytes, None), "café İstanbul 東京");
    }

    #[test]
    fn with_proxy_is_fail_closed_on_bad_url() {
        // A malformed proxy is a hard error — never a silent direct client.
        assert!(
            HttpFetcher::with_proxy("ua", "", false, std::time::Duration::from_secs(5)).is_err()
        );
        assert!(
            HttpFetcher::with_proxy("ua", "not a url", false, std::time::Duration::from_secs(5))
                .is_err()
        );
    }

    #[test]
    fn with_proxy_accepts_valid_url() {
        assert!(
            HttpFetcher::with_proxy(
                "ua",
                "http://user:pass@host:8080",
                false,
                std::time::Duration::from_secs(5),
            )
            .is_ok()
        );
    }

    // ── A. Predicate/pure function coverage ────────────────────────────

    #[test]
    fn is_retriable_status_boundaries() {
        assert!(
            !is_retriable_status(501),
            "501 Not Implemented is permanent"
        );
        assert!(is_retriable_status(502));
        assert!(is_retriable_status(503));
        assert!(is_retriable_status(504));
        assert!(!is_retriable_status(505), "505 HTTP Version is permanent");
        assert!(!is_retriable_status(500));
        assert!(!is_retriable_status(200));
        assert!(
            !is_retriable_status(429),
            "429 has its own proxy arm, not this retry"
        );
    }

    #[test]
    fn is_ratelimit_status_only_429() {
        assert!(is_ratelimit_status(429));
        assert!(!is_ratelimit_status(420));
        assert!(!is_ratelimit_status(430));
        assert!(!is_ratelimit_status(200));
        assert!(!is_ratelimit_status(503));
    }

    #[test]
    fn should_arm_proxy_more_edge_cases() {
        assert!(!should_arm_proxy(500, None));
        assert!(!should_arm_proxy(404, None));
        assert!(!should_arm_proxy(301, None));
        assert!(
            should_arm_proxy(429, Some(ChallengeHeader::AwsWaf)),
            "429 arms on its own even alongside an unrelated challenge header"
        );
    }

    /// A read-phase timeout (origin connected, then stalled) must be retried on
    /// the SAME egress: a different egress cannot make a slow origin faster.
    #[tokio::test]
    async fn is_retriable_error_true_only_for_read_timeout() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::sleep(std::time::Duration::from_secs(30));
                drop(stream);
            }
        });
        let err = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(300))
            .build()
            .unwrap()
            .get(format!("http://{addr}/"))
            .send()
            .await
            .unwrap_err();
        assert!(err.is_timeout() && !err.is_connect());
        assert!(is_retriable_error(&err), "a read timeout must be retriable");
    }

    /// A connect-phase timeout is routed to the proxy arm (`is_connection_failure`),
    /// never to the same-egress retry this predicate guards.
    #[tokio::test]
    async fn is_retriable_error_false_for_connect_timeout() {
        let err = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_millis(200))
            .build()
            .unwrap()
            .get("http://192.0.2.1/")
            .send()
            .await
            .unwrap_err();
        assert!(err.is_connect() && err.is_timeout());
        assert!(!is_retriable_error(&err));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn is_retriable_error_false_for_connection_reset() {
        let url = spawn_resetting_origin();
        let err = reqwest::Client::new().get(&url).send().await.unwrap_err();
        assert!(
            !is_retriable_error(&err),
            "a reset routes to the proxy arm via is_connection_failure, not here"
        );
    }

    /// Guards `is_cert_error` against false positives: a plain connect failure
    /// (refused or blackholed) must never be misclassified as a TLS cert
    /// failure, or a broken proxy pool would spuriously trip the relaxed-TLS
    /// fallback on every host.
    #[tokio::test]
    async fn is_cert_error_false_for_plain_connection_errors() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let err = reqwest::Client::new()
            .get(format!("http://{addr}/"))
            .send()
            .await
            .unwrap_err();
        assert!(
            !is_cert_error(&err),
            "a refused connection is not a cert error"
        );

        let err2 = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_millis(200))
            .build()
            .unwrap()
            .get("http://192.0.2.1/")
            .send()
            .await
            .unwrap_err();
        assert!(!is_cert_error(&err2), "a blackhole is not a cert error");
    }

    // ── B. challenge_header additional edge cases ───────────────────────

    #[test]
    fn challenge_header_name_is_case_insensitive() {
        use ChallengeHeader::CloudflareMitigated;
        assert_eq!(
            challenge_header(&header_map(&[("Cf-Mitigated", "challenge")])),
            Some(CloudflareMitigated),
            "reqwest's HeaderMap normalizes header names case-insensitively"
        );
    }

    #[test]
    fn challenge_header_cloudflare_checked_before_waf_when_both_present() {
        use ChallengeHeader::CloudflareMitigated;
        assert_eq!(
            challenge_header(&header_map(&[
                ("cf-mitigated", "challenge"),
                ("x-amzn-waf-action", "captcha"),
            ])),
            Some(CloudflareMitigated),
            "cf-mitigated is checked first when both headers are present"
        );
    }

    #[test]
    fn challenge_header_waf_wins_when_cf_header_present_but_invalid() {
        use ChallengeHeader::AwsWaf;
        assert_eq!(
            challenge_header(&header_map(&[
                ("cf-mitigated", "monitor"),
                ("x-amzn-waf-action", "captcha"),
            ])),
            Some(AwsWaf),
            "an invalid cf-mitigated value must fall through to the WAF check"
        );
    }

    #[test]
    fn challenge_header_empty_value_is_none() {
        assert_eq!(challenge_header(&header_map(&[("cf-mitigated", "")])), None);
    }

    #[test]
    fn challenge_header_whitespace_only_value_is_none() {
        assert_eq!(
            challenge_header(&header_map(&[("x-amzn-waf-action", "   ")])),
            None
        );
    }

    #[test]
    fn challenge_header_value_with_surrounding_whitespace_still_arms() {
        use ChallengeHeader::CloudflareMitigated;
        assert_eq!(
            challenge_header(&header_map(&[("cf-mitigated", " challenge ")])),
            Some(CloudflareMitigated),
            "the detector predicates trim() the value themselves"
        );
    }

    #[test]
    fn challenge_header_invalid_utf8_value_is_none() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::HeaderName::from_static("cf-mitigated"),
            reqwest::header::HeaderValue::from_bytes(&[0x63, 0x68, 0xFF, 0x6c]).unwrap(),
        );
        assert_eq!(
            challenge_header(&h),
            None,
            "a non-UTF-8 header value must not panic and must not match"
        );
    }

    // ── C. ChallengeHeader enum ──────────────────────────────────────────

    #[test]
    fn challenge_header_marker_values() {
        assert_eq!(
            ChallengeHeader::CloudflareMitigated.marker(),
            "cloudflare_mitigated"
        );
        assert_eq!(ChallengeHeader::AwsWaf.marker(), "waf_challenge");
    }

    #[test]
    fn challenge_header_variant_equality_and_copy() {
        let a = ChallengeHeader::CloudflareMitigated;
        let b = a; // Copy, not a move
        assert_eq!(a, b);
        assert_ne!(
            ChallengeHeader::CloudflareMitigated,
            ChallengeHeader::AwsWaf
        );
    }

    // ── D. charset_from_content_type ────────────────────────────────────

    #[test]
    fn charset_from_content_type_single_quotes() {
        assert_eq!(
            charset_from_content_type("text/html;charset='utf-8'").as_deref(),
            Some("utf-8")
        );
    }

    #[test]
    fn charset_from_content_type_whitespace_around_equals() {
        assert_eq!(
            charset_from_content_type("text/html; charset = 'ISO-8859-1' ").as_deref(),
            Some("ISO-8859-1")
        );
    }

    #[test]
    fn charset_from_content_type_uppercase_keyword() {
        assert_eq!(
            charset_from_content_type("text/html; CHARSET=utf-8").as_deref(),
            Some("utf-8"),
            "the keyword lookup is lowercased before matching"
        );
    }

    #[test]
    fn charset_from_content_type_preserves_label_case() {
        assert_eq!(
            charset_from_content_type("text/html; charset=UTF-8").as_deref(),
            Some("UTF-8"),
            "the label itself is sliced from the ORIGINAL string, not the lowercased copy"
        );
    }

    #[test]
    fn charset_from_content_type_empty_value_returns_none() {
        assert_eq!(
            charset_from_content_type("text/html; charset=").as_deref(),
            None
        );
        assert_eq!(
            charset_from_content_type("text/html; charset=;").as_deref(),
            None
        );
    }

    #[test]
    fn charset_from_content_type_missing_equals_returns_none() {
        assert_eq!(
            charset_from_content_type("text/html; charset").as_deref(),
            None
        );
    }

    /// BUG: the lookup is `str::find("charset")`, an unanchored substring
    /// search — it matches "charset" as a SUFFIX of an unrelated param name
    /// (e.g. a custom `x-ischarset` param), and then parses whatever follows
    /// THAT occurrence as if it were the real charset value. Real-world
    /// Content-Type headers are unlikely to carry such a param, so this is
    /// low severity, but it is a real latent misparse, not intended
    /// behaviour. Documented here rather than fixed (tests-only change).
    #[test]
    fn charset_from_content_type_substring_match_is_a_known_quirk() {
        assert_eq!(
            charset_from_content_type("text/html; x-ischarset=foo; charset=windows-1251")
                .as_deref(),
            Some("foo"),
            "BUG: matches \"charset\" inside \"x-ischarset\" and returns the wrong value"
        );
    }

    #[test]
    fn charset_from_content_type_multiple_params_charset_not_first() {
        assert_eq!(
            charset_from_content_type("text/html; boundary=xyz; charset=koi8-r").as_deref(),
            Some("koi8-r")
        );
    }

    #[test]
    fn charset_from_content_type_numeric_label() {
        assert_eq!(
            charset_from_content_type("text/html; charset=1252").as_deref(),
            Some("1252")
        );
    }

    #[test]
    fn charset_from_content_type_colon_allowed_in_label() {
        // The allowed-char set explicitly includes ':' (some IANA labels use it).
        assert_eq!(
            charset_from_content_type("text/html; charset=x-user:defined").as_deref(),
            Some("x-user:defined")
        );
    }

    #[test]
    fn charset_from_content_type_trailing_whitespace_before_semicolon() {
        assert_eq!(
            charset_from_content_type("text/html; charset=utf-8 ; boundary=x").as_deref(),
            Some("utf-8")
        );
    }

    #[test]
    fn charset_from_content_type_missing_closing_quote_does_not_panic() {
        // Malformed: an opening quote with no matching close. Must not panic;
        // the scan just runs to the end of the allowed-char set.
        assert_eq!(
            charset_from_content_type("text/html; charset=\"utf-8").as_deref(),
            Some("utf-8")
        );
    }

    // ── E. sniff_meta_charset ────────────────────────────────────────────

    #[test]
    fn sniff_meta_charset_finds_basic_declaration() {
        assert_eq!(
            sniff_meta_charset(b"<html><head><meta charset=\"utf-8\"></head></html>").as_deref(),
            Some("utf-8")
        );
    }

    #[test]
    fn sniff_meta_charset_http_equiv_variant() {
        let bytes =
            br#"<meta http-equiv="Content-Type" content="text/html; charset=windows-1251">"#;
        assert_eq!(sniff_meta_charset(bytes).as_deref(), Some("windows-1251"));
    }

    #[test]
    fn sniff_meta_charset_case_insensitive_keyword() {
        assert_eq!(
            sniff_meta_charset(b"<META CHARSET=UTF-8>").as_deref(),
            Some("utf-8"),
            "the whole head is lowercased before scanning, so the label comes back lowercase too"
        );
    }

    #[test]
    fn sniff_meta_charset_none_when_absent() {
        assert_eq!(
            sniff_meta_charset(b"<html><body>hello</body></html>").as_deref(),
            None
        );
        assert_eq!(sniff_meta_charset(b"").as_deref(), None);
    }

    #[test]
    fn sniff_meta_charset_ignores_declaration_past_2kb_window() {
        let mut bytes = vec![b' '; 2100];
        bytes.extend_from_slice(b"<meta charset=windows-1254>");
        assert_eq!(
            sniff_meta_charset(&bytes),
            None,
            "only the first ~2KB is scanned"
        );
    }

    #[test]
    fn sniff_meta_charset_handles_short_buffer_without_panic() {
        assert_eq!(sniff_meta_charset(b"hi").as_deref(), None);
        assert_eq!(sniff_meta_charset(b"").as_deref(), None);
    }

    #[test]
    fn sniff_meta_charset_handles_invalid_utf8_prefix_without_panic() {
        let mut bytes = vec![0xFF, 0xFE, 0x00, 0x01];
        bytes.extend_from_slice(b"<meta charset=utf-8>");
        assert_eq!(sniff_meta_charset(&bytes).as_deref(), Some("utf-8"));
    }

    #[test]
    fn sniff_meta_charset_quoted_double_quotes() {
        assert_eq!(
            sniff_meta_charset(br#"<meta charset="big5">"#).as_deref(),
            Some("big5")
        );
    }

    // ── F. decode_html_bytes ─────────────────────────────────────────────

    #[test]
    fn decode_html_bytes_invalid_utf8_no_hints_uses_lossy_replacement() {
        let bytes = b"plain \xFF\xFE broken";
        let out = decode_html_bytes(bytes, None);
        assert!(out.contains('\u{FFFD}'), "got: {out}");
        assert!(out.starts_with("plain "));
    }

    #[test]
    fn decode_html_bytes_empty_input() {
        assert_eq!(decode_html_bytes(b"", None), "");
        assert_eq!(decode_html_bytes(b"", Some("utf-8")), "");
    }

    #[test]
    fn decode_html_bytes_unknown_header_and_unknown_meta_falls_back_to_lossy() {
        let bytes = b"<meta charset=totally-bogus-label><p>\xFF</p>";
        let out = decode_html_bytes(bytes, Some("also-bogus"));
        assert!(out.contains('\u{FFFD}'), "got: {out}");
        assert!(out.contains("<p>"));
    }

    #[test]
    fn decode_html_bytes_header_wins_over_conflicting_meta() {
        // Header says Latin-1 (é = 0xE9), meta claims UTF-8. Header must win.
        let bytes = b"<meta charset=utf-8><p>caf\xE9</p>";
        let out = decode_html_bytes(bytes, Some("iso-8859-1"));
        assert!(out.contains("café"), "got: {out}");
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn decode_html_bytes_long_unicode_roundtrip() {
        let text = "café ".repeat(5000) + "the end";
        let out = decode_html_bytes(text.as_bytes(), Some("utf-8"));
        assert_eq!(out, text);
    }

    #[test]
    fn decode_html_bytes_emoji_roundtrip() {
        let text = "hello \u{1F600}\u{1F4A9} world";
        assert_eq!(decode_html_bytes(text.as_bytes(), None), text);
        assert_eq!(decode_html_bytes(text.as_bytes(), Some("utf-8")), text);
    }

    #[test]
    fn decode_html_bytes_windows1252_smart_quotes() {
        // Windows-1252 curly double-quotes: 0x93 = “, 0x94 = ”.
        let bytes = b"say \x93hi\x94";
        let out = decode_html_bytes(bytes, Some("windows-1252"));
        assert_eq!(out, "say \u{201C}hi\u{201D}");
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn decode_html_bytes_meta_declared_with_single_quotes() {
        let bytes = b"<meta charset='windows-1254'><p>i\xE7in</p>";
        let out = decode_html_bytes(bytes, None);
        assert!(out.contains("için"), "got: {out}");
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn decode_html_bytes_embedded_null_bytes_do_not_panic() {
        let bytes = b"before\x00after";
        let out = decode_html_bytes(bytes, Some("utf-8"));
        assert!(out.contains("before"));
        assert!(out.contains("after"));
    }

    // ── G. env/config predicates ─────────────────────────────────────────

    #[test]
    fn tls_relaxed_fallback_enabled_truth_table() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for (val, want) in [
            ("true", true),
            ("TRUE", true),
            ("1", true),
            ("yes", true),
            ("YES", true),
            (" 1 ", true),
            ("false", false),
            ("0", false),
            ("no", false),
            ("garbage", false),
            ("", false),
        ] {
            unsafe { std::env::set_var("CRW_HTTP_TLS_RELAXED_FALLBACK", val) };
            assert_eq!(tls_relaxed_fallback_enabled(), want, "val={val:?}");
        }
        unsafe { std::env::remove_var("CRW_HTTP_TLS_RELAXED_FALLBACK") };
        assert!(
            !tls_relaxed_fallback_enabled(),
            "unset must default to false"
        );
    }

    #[test]
    fn relaxed_client_built_only_when_env_flag_enabled() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("CRW_HTTP_TLS_RELAXED_FALLBACK") };
        let f = HttpFetcher::new("ua", None, false);
        assert!(f.relaxed_client.is_none());

        unsafe { std::env::set_var("CRW_HTTP_TLS_RELAXED_FALLBACK", "1") };
        let f2 = HttpFetcher::new("ua", None, false);
        unsafe { std::env::remove_var("CRW_HTTP_TLS_RELAXED_FALLBACK") };
        assert!(f2.relaxed_client.is_some());
    }

    #[test]
    fn relaxed_client_wired_through_with_proxy_constructor() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("CRW_HTTP_TLS_RELAXED_FALLBACK", "1") };
        let f = HttpFetcher::with_proxy(
            "ua",
            "http://gw.example.com:823",
            false,
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        unsafe { std::env::remove_var("CRW_HTTP_TLS_RELAXED_FALLBACK") };
        assert!(f.relaxed_client.is_some());
    }

    #[test]
    fn ratelimit_proxy_url_trims_and_empties_to_none() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("CRW_HTTP_RATELIMIT_PROXY_URL") };
        assert_eq!(ratelimit_proxy_url(), None);

        unsafe { std::env::set_var("CRW_HTTP_RATELIMIT_PROXY_URL", "   ") };
        assert_eq!(
            ratelimit_proxy_url(),
            None,
            "whitespace-only must count as unset"
        );

        unsafe { std::env::set_var("CRW_HTTP_RATELIMIT_PROXY_URL", "  http://gw:823  ") };
        assert_eq!(ratelimit_proxy_url().as_deref(), Some("http://gw:823"));
        unsafe { std::env::remove_var("CRW_HTTP_RATELIMIT_PROXY_URL") };
    }

    #[test]
    fn has_ratelimit_proxy_false_when_url_malformed() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("CRW_HTTP_RATELIMIT_PROXY_URL", "not a url") };
        let f = HttpFetcher::new("ua", None, false);
        unsafe { std::env::remove_var("CRW_HTTP_RATELIMIT_PROXY_URL") };
        assert!(
            !f.has_ratelimit_proxy(),
            "a typo'd proxy URL must not silently claim a working recovery egress"
        );
    }

    #[test]
    fn has_ratelimit_proxy_true_when_url_valid() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("CRW_HTTP_RATELIMIT_PROXY_URL", "http://gw.example.com:823") };
        let f = HttpFetcher::new("ua", None, false);
        unsafe { std::env::remove_var("CRW_HTTP_RATELIMIT_PROXY_URL") };
        assert!(f.has_ratelimit_proxy());
    }

    #[test]
    fn has_ratelimit_proxy_wired_through_with_proxy_constructor() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("CRW_HTTP_RATELIMIT_PROXY_URL", "http://gw.example.com:823") };
        let f = HttpFetcher::with_proxy(
            "ua",
            "http://static-proxy.example.com:1",
            false,
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        unsafe { std::env::remove_var("CRW_HTTP_RATELIMIT_PROXY_URL") };
        assert!(f.has_ratelimit_proxy());
    }

    #[test]
    fn env_proxy_configured_truth_table() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_proxy_env();
        assert!(!env_proxy_configured());

        unsafe { std::env::set_var("HTTP_PROXY", "http://x:1") };
        assert!(env_proxy_configured());
        clear_proxy_env();

        unsafe { std::env::set_var("https_proxy", "  ") };
        assert!(
            !env_proxy_configured(),
            "whitespace-only must count as unset"
        );
        clear_proxy_env();

        unsafe { std::env::set_var("all_proxy", "socks5://x:1") };
        assert!(env_proxy_configured());
        clear_proxy_env();
    }

    #[test]
    fn has_static_proxy_true_when_proxy_arg_given_regardless_of_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_proxy_env();
        let f = HttpFetcher::new("ua", Some("http://user:pass@host:1"), false);
        assert!(f.has_static_proxy);
    }

    #[test]
    fn has_static_proxy_true_when_env_proxy_set_without_explicit_arg() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_proxy_env();
        unsafe { std::env::set_var("HTTP_PROXY", "http://x:1") };
        let f = HttpFetcher::new("ua", None, false);
        clear_proxy_env();
        assert!(f.has_static_proxy);
    }

    #[test]
    fn has_static_proxy_false_with_no_proxy_and_no_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_proxy_env();
        let f = HttpFetcher::new("ua", None, false);
        assert!(!f.has_static_proxy);
    }

    #[test]
    fn with_proxy_always_sets_has_static_proxy_true() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_proxy_env();
        let f = HttpFetcher::with_proxy(
            "ua",
            "http://gw.example.com:823",
            false,
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert!(f.has_static_proxy);
    }

    // ── H. build_client / constructors ──────────────────────────────────

    #[test]
    fn build_client_ok_with_no_proxy() {
        assert!(build_client("ua", None, std::time::Duration::from_secs(5), false).is_ok());
    }

    #[test]
    fn build_client_ok_with_relaxed_tls() {
        assert!(build_client("ua", None, std::time::Duration::from_secs(5), true).is_ok());
    }

    #[test]
    fn build_client_ok_with_valid_proxy_and_credentials() {
        assert!(
            build_client(
                "ua",
                Some("http://user:pass@gw.example.com:823"),
                std::time::Duration::from_secs(5),
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn build_client_err_on_malformed_proxy_url() {
        let err = build_client(
            "ua",
            Some("not a url"),
            std::time::Duration::from_secs(5),
            false,
        )
        .unwrap_err();
        assert!(
            matches!(err, CrwError::ConfigError(ref msg) if msg.contains("invalid proxy URL")),
            "got {err:?}"
        );
    }

    #[test]
    fn build_client_err_on_empty_proxy_url() {
        let err =
            build_client("ua", Some(""), std::time::Duration::from_secs(5), false).unwrap_err();
        assert!(matches!(err, CrwError::ConfigError(_)));
    }

    /// `with_timeout` is the INFALLIBLE constructor (see its docs): a bad
    /// user-agent header value must not panic, and must fall back to a
    /// default client rather than propagating the build error.
    #[test]
    fn with_timeout_falls_back_to_default_client_on_invalid_user_agent() {
        let f =
            HttpFetcher::with_timeout("bad\nua", None, false, std::time::Duration::from_secs(5));
        assert!(!f.has_static_proxy);
    }

    /// `with_proxy` is the STRICT, fail-closed constructor: the same invalid
    /// user-agent must surface as a hard error instead.
    #[test]
    fn with_proxy_is_fail_closed_on_invalid_user_agent_too() {
        // `unwrap_err()` needs `T: Debug` (HttpFetcher isn't), so match instead.
        match HttpFetcher::with_proxy(
            "bad\nua",
            "http://gw.example.com:823",
            false,
            std::time::Duration::from_secs(5),
        ) {
            Err(err) => assert!(matches!(err, CrwError::ConfigError(_))),
            Ok(_) => panic!("an invalid user-agent must be a hard error via with_proxy"),
        }
    }

    // ── I. trait impl metadata ───────────────────────────────────────────

    #[tokio::test]
    async fn trait_impl_reports_http_metadata() {
        let f = HttpFetcher::new("ua", None, false);
        assert_eq!(f.name(), "http");
        assert!(!f.supports_js());
        assert!(f.is_available().await);
    }

    // ── J. UA / headers (pure) ───────────────────────────────────────────

    /// Historical bug: a stale Chrome UA in the stealth pool triggered
    /// "browser outdated" rejections on real sites (fixed v0.18.0 -> v0.18.3).
    /// The sec-ch-ua client hint is kept in sync with `BUILTIN_UA_POOL` by
    /// hand (see the doc comment on the const); guard it stays a modern major
    /// version so a future edit does not silently reintroduce that class.
    #[test]
    fn stealth_sec_ch_ua_reports_a_modern_chrome_version() {
        let major: u32 = STEALTH_SEC_CH_UA
            .split("Chrome\";v=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("sec-ch-ua must carry a Chrome version")
            .parse()
            .expect("Chrome version must be numeric");
        assert!(
            major >= 100,
            "sec-ch-ua Chrome version {major} looks stale/ancient"
        );
    }

    #[test]
    fn stealth_accept_header_includes_html_mime_types() {
        assert!(STEALTH_ACCEPT.contains("text/html"));
        assert!(
            STEALTH_ACCEPT.starts_with("text/html"),
            "text/html must be the most-preferred type"
        );
    }

    // ── K. UA / headers (network) ────────────────────────────────────────

    async fn echo_headers_handler(
        headers: axum::http::HeaderMap,
    ) -> impl axum::response::IntoResponse {
        let mut out = String::new();
        for (name, value) in headers.iter() {
            out.push_str(name.as_str());
            out.push_str(": ");
            out.push_str(value.to_str().unwrap_or("<non-utf8>"));
            out.push('\n');
        }
        (
            axum::http::StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            out,
        )
    }

    #[tokio::test]
    async fn stealth_headers_are_injected_when_enabled() {
        let base = spawn_router(
            axum::Router::new().route("/echo", axum::routing::get(echo_headers_handler)),
        )
        .await;
        let fetcher = HttpFetcher::new("crw-test-ua/1.0", None, true);
        let res = fetcher
            .fetch(
                &format!("{base}/echo"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5_000),
            )
            .await
            .unwrap();
        let body = res.html.to_lowercase();
        assert!(body.contains("user-agent: crw-test-ua/1.0"), "got: {body}");
        assert!(res.html.contains(STEALTH_ACCEPT));
        assert!(res.html.contains(STEALTH_SEC_CH_UA));
        assert!(body.contains("sec-fetch-dest: document"));
        assert!(body.contains("upgrade-insecure-requests: 1"));
    }

    #[tokio::test]
    async fn stealth_headers_absent_when_disabled() {
        let base = spawn_router(
            axum::Router::new().route("/echo", axum::routing::get(echo_headers_handler)),
        )
        .await;
        let fetcher = HttpFetcher::new("crw-test-ua/1.0", None, false);
        let res = fetcher
            .fetch(
                &format!("{base}/echo"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5_000),
            )
            .await
            .unwrap();
        let body = res.html.to_lowercase();
        assert!(!body.contains("sec-fetch-dest"));
        assert!(!res.html.contains(STEALTH_SEC_CH_UA));
    }

    #[tokio::test]
    async fn caller_headers_are_forwarded_and_can_add_new_headers() {
        let base = spawn_router(
            axum::Router::new().route("/echo", axum::routing::get(echo_headers_handler)),
        )
        .await;
        let fetcher = HttpFetcher::new("crw-test", None, false);
        let mut headers = HashMap::new();
        headers.insert("X-Custom-Test".to_string(), "hello-world".to_string());
        let res = fetcher
            .fetch(
                &format!("{base}/echo"),
                &headers,
                None,
                Deadline::from_request_ms(5_000),
            )
            .await
            .unwrap();
        assert!(
            res.html
                .to_lowercase()
                .contains("x-custom-test: hello-world"),
            "got: {}",
            res.html
        );
    }

    // ── L. redirects (network) ───────────────────────────────────────────

    async fn redirect_target_handler() -> impl axum::response::IntoResponse {
        (
            axum::http::StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<html><body>redirect target reached</body></html>",
        )
    }

    async fn redirect_once_handler() -> impl axum::response::IntoResponse {
        axum::response::Redirect::to("/target")
    }

    async fn redirect_loop_handler() -> impl axum::response::IntoResponse {
        axum::response::Redirect::to("/loop")
    }

    async fn clean_200_handler() -> impl axum::response::IntoResponse {
        (
            axum::http::StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<html><body>clean page</body></html>",
        )
    }

    fn redirect_router() -> axum::Router {
        axum::Router::new()
            .route("/redirect-once", axum::routing::get(redirect_once_handler))
            .route("/target", axum::routing::get(redirect_target_handler))
            .route("/loop", axum::routing::get(redirect_loop_handler))
            .route("/clean", axum::routing::get(clean_200_handler))
    }

    /// By default (no `CRW_ALLOW_LOOPBACK_FOR_TESTS` escape hatch), a redirect
    /// to a loopback host is blocked by `safe_redirect_policy` even when the
    /// origin itself is also loopback — the SSRF check runs on every redirect
    /// hop, not just the caller-supplied URL.
    #[tokio::test]
    // Holds ENV_LOCK across awaits on purpose: the guard serialises these
    // tests against each other while they mutate a process-wide env var.
    #[allow(clippy::await_holding_lock)]
    async fn redirect_is_blocked_by_default_ssrf_policy() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("CRW_ALLOW_LOOPBACK_FOR_TESTS") };
        let base = spawn_router(redirect_router()).await;
        let fetcher = HttpFetcher::new("crw-test", None, false);
        let err = fetcher
            .fetch(
                &format!("{base}/redirect-once"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5_000),
            )
            .await
            .expect_err("a redirect to a loopback host must be blocked by default");
        assert!(matches!(err, CrwError::HttpError(_)), "got {err:?}");
    }

    #[tokio::test]
    // Holds ENV_LOCK across awaits on purpose: the guard serialises these
    // tests against each other while they mutate a process-wide env var.
    #[allow(clippy::await_holding_lock)]
    async fn redirect_follows_when_ssrf_escape_hatch_enabled() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("CRW_ALLOW_LOOPBACK_FOR_TESTS", "1") };
        let base = spawn_router(redirect_router()).await;
        let fetcher = HttpFetcher::new("crw-test", None, false);
        let res = fetcher
            .fetch(
                &format!("{base}/redirect-once"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5_000),
            )
            .await;
        unsafe { std::env::remove_var("CRW_ALLOW_LOOPBACK_FOR_TESTS") };
        let res = res.expect("redirect must be followed once the escape hatch is set");
        assert_eq!(res.status_code, 200);
        assert!(
            res.html.contains("redirect target reached"),
            "got: {}",
            res.html
        );
        assert!(
            res.final_url
                .as_deref()
                .is_some_and(|u| u.ends_with("/target")),
            "got final_url={:?}",
            res.final_url
        );
    }

    #[tokio::test]
    // Holds ENV_LOCK across awaits on purpose: the guard serialises these
    // tests against each other while they mutate a process-wide env var.
    #[allow(clippy::await_holding_lock)]
    async fn redirect_loop_exceeds_max_hops() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("CRW_ALLOW_LOOPBACK_FOR_TESTS", "1") };
        let base = spawn_router(redirect_router()).await;
        let fetcher = HttpFetcher::new("crw-test", None, false);
        let res = fetcher
            .fetch(
                &format!("{base}/loop"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5_000),
            )
            .await;
        unsafe { std::env::remove_var("CRW_ALLOW_LOOPBACK_FOR_TESTS") };
        assert!(
            res.is_err(),
            "an infinite redirect loop must not hang or succeed"
        );
    }

    #[tokio::test]
    async fn final_url_is_none_when_no_redirect_occurred() {
        let base = spawn_router(redirect_router()).await;
        let fetcher = HttpFetcher::new("crw-test", None, false);
        let res = fetcher
            .fetch(
                &format!("{base}/clean"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5_000),
            )
            .await
            .unwrap();
        assert_eq!(res.final_url, None);
    }

    // ── M. content-length / body size ───────────────────────────────────

    /// The cap must be enforced while streaming, not measured afterwards.
    ///
    /// A chunked response carries no `Content-Length`, so the header pre-check
    /// at the call site is skipped entirely and the body used to land in one
    /// buffering call. Note the assertion cannot be on the error: the post-hoc
    /// check in `build_http_fetch_result` already produced `Response too large`
    /// either way, so only the memory was ever wrong. Driving
    /// `read_body_capped` with a small cap makes the boundary itself testable.
    #[tokio::test]
    async fn read_body_capped_rejects_a_chunked_body_over_the_cap() {
        // Two 1 KiB chunks, no Content-Length.
        let body = format!(
            "400\r\n{}\r\n400\r\n{}\r\n0\r\n\r\n",
            "a".repeat(1024),
            "b".repeat(1024)
        );
        let url = serve_raw(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            body.into_bytes(),
        );
        let resp = reqwest::Client::new().get(&url).send().await.unwrap();
        assert_eq!(
            resp.content_length(),
            None,
            "a chunked response must carry no Content-Length, or this test is not testing the gap"
        );
        let err = read_body_capped(resp, 1500)
            .await
            .expect_err("2048 bytes past a 1500-byte cap must be rejected");
        assert!(
            matches!(err, CrwError::HttpError(ref m) if m.contains("too large")),
            "got {err:?}"
        );
    }

    /// Exactly the cap is accepted; one byte over is not.
    ///
    /// Pins the boundary against `build_http_fetch_result`'s own
    /// `len > MAX_RESPONSE_BYTES`, so the two cannot drift into disagreeing
    /// about whether a body of exactly the cap is legal.
    #[tokio::test]
    async fn read_body_capped_boundary_is_inclusive() {
        for (len, cap, want_ok) in [(100usize, 100usize, true), (101, 100, false)] {
            let url = serve_raw(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                format!("{len:x}\r\n{}\r\n0\r\n\r\n", "a".repeat(len)).into_bytes(),
            );
            let resp = reqwest::Client::new().get(&url).send().await.unwrap();
            let got = read_body_capped(resp, cap).await;
            assert_eq!(got.is_ok(), want_ok, "len={len} cap={cap} got {got:?}");
            if want_ok {
                assert_eq!(got.unwrap().len(), len);
            }
        }
    }

    /// The cap is measured AFTER decompression.
    ///
    /// This is the case the header check can never catch and the one the whole
    /// change exists for: tower-http's decompression body reports no size hint,
    /// so `content_length()` is `None`. There is no compressed-size bound to
    /// scale up, there is no number at all. 1 MiB of zeros gzips to about a
    /// kilobyte, so a cap applied to wire bytes would wave this through.
    #[tokio::test]
    async fn read_body_capped_measures_decompressed_bytes() {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;

        let plain = vec![b'a'; 1024 * 1024];
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&plain).unwrap();
        let gz = enc.finish().unwrap();
        assert!(
            gz.len() < 64 * 1024,
            "fixture must be much smaller on the wire than decompressed: {} bytes",
            gz.len()
        );

        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            gz.len()
        );
        let url = serve_raw(head, gz);

        // The default client has gzip decoding on, like the renderer's.
        let resp = reqwest::Client::new().get(&url).send().await.unwrap();
        assert_eq!(
            resp.content_length(),
            None,
            "a decompressing body must report no size hint, or the premise is wrong"
        );
        let err = read_body_capped(resp, 128 * 1024)
            .await
            .expect_err("1 MiB decompressed past a 128 KiB cap must be rejected");
        assert!(
            matches!(err, CrwError::HttpError(ref m) if m.contains("too large")),
            "got {err:?}"
        );
    }

    /// A chunked body under the cap is reassembled whole, across chunks.
    #[tokio::test]
    async fn read_body_capped_reassembles_a_chunked_body() {
        let a = "<html><body>chunked ";
        let b = "ok</body></html>";
        let url = serve_raw(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            format!("{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n", a.len(), b.len()).into_bytes(),
        );
        let resp = reqwest::Client::new().get(&url).send().await.unwrap();
        let body = read_body_capped(resp, 1024).await.unwrap();
        assert_eq!(String::from_utf8(body).unwrap(), format!("{a}{b}"));
    }

    /// An empty body is not an error and does not trip the cap, even at zero.
    #[tokio::test]
    async fn read_body_capped_accepts_an_empty_body() {
        let url = serve_raw(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            Vec::new(),
        );
        let resp = reqwest::Client::new().get(&url).send().await.unwrap();
        assert!(read_body_capped(resp, 0).await.unwrap().is_empty());
    }

    /// A raw response whose declared Content-Length exceeds MAX_RESPONSE_BYTES
    /// must be rejected from the HEADER alone, before any body bytes are
    /// read — the server sends no body at all, so a bug that instead tried
    /// to download it would hang rather than error fast.
    #[tokio::test]
    async fn content_length_over_limit_is_rejected_before_download() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};

            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let huge = MAX_RESPONSE_BYTES + 1;
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {huge}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            );
        });
        let url = format!("http://{addr}/");
        let fetcher = HttpFetcher::new("crw-test", None, false);
        let started = std::time::Instant::now();
        let err = fetcher
            .fetch(
                &url,
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5_000),
            )
            .await
            .expect_err("an oversized declared Content-Length must be rejected");
        assert!(
            matches!(err, CrwError::HttpError(ref msg) if msg.contains("too large")),
            "got {err:?}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "must reject from the header alone, not wait to download; took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn content_length_matching_small_body_is_accepted() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                const BODY: &str = "<html><body>tiny</body></html>";
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
                        BODY.len()
                    )
                    .as_bytes(),
                );
            }
        });
        let url = format!("http://{addr}/");
        let fetcher = HttpFetcher::new("crw-test", None, false);
        let res = fetcher
            .fetch(
                &url,
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5_000),
            )
            .await
            .expect("a normal small response must not be rejected");
        assert_eq!(res.status_code, 200);
        assert!(res.html.contains("tiny"));
    }

    // ── N. content-type dispatch (network) ──────────────────────────────

    async fn pdf_handler() -> impl axum::response::IntoResponse {
        (
            axum::http::StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/pdf")],
            b"%PDF-1.4 fake pdf payload".to_vec(),
        )
    }

    #[tokio::test]
    // Holds ENV_LOCK across awaits on purpose: the fetcher build below reads
    // CRW_HTTP_RATELIMIT_PROXY_URL, which sibling tests flip under the lock.
    #[allow(clippy::await_holding_lock)]
    async fn pdf_content_type_populates_raw_bytes_not_html() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("CRW_HTTP_RATELIMIT_PROXY_URL") };
        let base =
            spawn_router(axum::Router::new().route("/doc.pdf", axum::routing::get(pdf_handler)))
                .await;
        let fetcher = HttpFetcher::new("crw-test", None, false);
        let res = fetcher
            .fetch(
                &format!("{base}/doc.pdf"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5_000),
            )
            .await
            .unwrap();
        assert_eq!(res.html, "");
        assert_eq!(
            res.raw_bytes.as_deref(),
            Some(&b"%PDF-1.4 fake pdf payload"[..])
        );
        assert_eq!(res.rendered_with.as_deref(), Some("pdf"));
        assert_eq!(res.content_type.as_deref(), Some("application/pdf"));
    }

    #[tokio::test]
    // Holds ENV_LOCK across awaits on purpose: see the pdf test above.
    #[allow(clippy::await_holding_lock)]
    async fn non_pdf_content_type_sets_rendered_with_http() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("CRW_HTTP_RATELIMIT_PROXY_URL") };
        let base = spawn_router(redirect_router()).await;
        let fetcher = HttpFetcher::new("crw-test", None, false);
        let res = fetcher
            .fetch(
                &format!("{base}/clean"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5_000),
            )
            .await
            .unwrap();
        assert_eq!(res.rendered_with.as_deref(), Some("http"));
        assert!(res.raw_bytes.is_none());
    }

    /// `content_type` is lowercased before the PDF check, so an uppercase
    /// (or mixed-case) `Content-Type` header must still dispatch to the PDF
    /// path rather than being decoded as HTML.
    #[tokio::test]
    // Holds ENV_LOCK across awaits on purpose: see the pdf test above.
    #[allow(clippy::await_holding_lock)]
    async fn is_pdf_check_is_case_insensitive() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("CRW_HTTP_RATELIMIT_PROXY_URL") };
        async fn uppercase_pdf_handler() -> impl axum::response::IntoResponse {
            (
                axum::http::StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "APPLICATION/PDF")],
                b"%PDF-1.4 upper".to_vec(),
            )
        }
        let base = spawn_router(
            axum::Router::new().route("/doc.pdf", axum::routing::get(uppercase_pdf_handler)),
        )
        .await;
        let fetcher = HttpFetcher::new("crw-test", None, false);
        let res = fetcher
            .fetch(
                &format!("{base}/doc.pdf"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5_000),
            )
            .await
            .unwrap();
        assert_eq!(res.html, "");
        assert_eq!(res.raw_bytes.as_deref(), Some(&b"%PDF-1.4 upper"[..]));
        assert_eq!(res.rendered_with.as_deref(), Some("pdf"));
    }

    // ── O. challenge warning end-to-end (network) ───────────────────────

    async fn cf_challenge_as_200_handler() -> impl axum::response::IntoResponse {
        (
            axum::http::StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (
                    axum::http::HeaderName::from_static("cf-mitigated"),
                    "challenge",
                ),
            ],
            "<html><body>are you human?</body></html>",
        )
    }

    #[tokio::test]
    async fn challenge_header_on_final_response_populates_warning_fields() {
        let base = spawn_router(
            axum::Router::new().route("/wall", axum::routing::get(cf_challenge_as_200_handler)),
        )
        .await;
        let fetcher = HttpFetcher::new("crw-test", None, false);
        let res = fetcher
            .fetch(
                &format!("{base}/wall"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5_000),
            )
            .await
            .unwrap();
        assert_eq!(res.warning.as_deref(), Some("cloudflare_mitigated"));
        assert_eq!(res.warnings.len(), 1);
        assert!(res.warnings[0].contains("Cloudflare"));
    }

    #[tokio::test]
    async fn clean_response_has_no_warning() {
        let base = spawn_router(redirect_router()).await;
        let fetcher = HttpFetcher::new("crw-test", None, false);
        let res = fetcher
            .fetch(
                &format!("{base}/clean"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5_000),
            )
            .await
            .unwrap();
        assert_eq!(res.warning, None);
        assert!(res.warnings.is_empty());
    }

    /// The AWS-WAF shape end-to-end: a 202 with an empty body still carries a
    /// distinct warning marker and customer-visible text naming AWS, never
    /// the Cloudflare one.
    #[tokio::test]
    async fn aws_waf_challenge_on_final_response_populates_distinct_warning() {
        async fn aws_waf_202_handler() -> impl axum::response::IntoResponse {
            (
                axum::http::StatusCode::ACCEPTED,
                [(
                    axum::http::HeaderName::from_static("x-amzn-waf-action"),
                    "challenge",
                )],
                "",
            )
        }
        let base = spawn_router(
            axum::Router::new().route("/wall", axum::routing::get(aws_waf_202_handler)),
        )
        .await;
        let fetcher = HttpFetcher::new("crw-test", None, false);
        let res = fetcher
            .fetch(
                &format!("{base}/wall"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(5_000),
            )
            .await
            .unwrap();
        assert_eq!(res.status_code, 202);
        assert_eq!(res.warning.as_deref(), Some("waf_challenge"));
        assert_eq!(res.warnings.len(), 1);
        assert!(res.warnings[0].contains("AWS WAF"));
        assert!(!res.warnings[0].contains("Cloudflare"));
    }

    // ── P. deadline ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn deadline_already_expired_returns_error_without_network_attempt() {
        // Port nothing listens on: if the code somehow attempted the network
        // instead of failing fast on the expired deadline, this would still
        // error, but for the wrong reason and slower — the fast-path check is
        // what this test pins.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let fetcher = HttpFetcher::new("crw-test", None, false);
        let started = std::time::Instant::now();
        let err = fetcher
            .fetch(
                &format!("http://{addr}/"),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(0),
            )
            .await
            .expect_err("an already-expired deadline must error immediately");
        assert!(matches!(err, CrwError::HttpError(ref msg) if msg.contains("deadline expired")));
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
    }

    /// `with_timeout`'s custom `request_timeout` parameter must actually be
    /// wired into the built client — a short custom timeout must fire even
    /// when the caller's `Deadline` budget is generous, proving the two are
    /// independent bounds.
    #[tokio::test]
    async fn with_timeout_custom_request_timeout_is_enforced_by_the_client() {
        async fn slow_handler() -> impl axum::response::IntoResponse {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            (axum::http::StatusCode::OK, "<html>too late</html>")
        }
        let base =
            spawn_router(axum::Router::new().route("/slow", axum::routing::get(slow_handler)))
                .await;
        let fetcher = HttpFetcher::with_timeout(
            "crw-test",
            None,
            false,
            std::time::Duration::from_millis(250),
        );
        let started = std::time::Instant::now();
        let err = fetcher
            .fetch(
                &format!("{base}/slow"),
                &HashMap::new(),
                None,
                // A generous 10s Deadline: only the client's own 250ms
                // request_timeout should be able to cut this short.
                Deadline::from_request_ms(10_000),
            )
            .await
            .expect_err("the client-level request_timeout must fire before the 3s response");
        assert!(
            matches!(err, CrwError::HttpError(_) | CrwError::Timeout(_)),
            "got {err:?}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "took {:?}, the 250ms client timeout did not fire",
            started.elapsed()
        );
    }

    // ── Q. mid-loop 429 / challenge-header rescue ───────────────────────

    fn spawn_429_origin() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                const BODY: &str = "rate limited";
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
                        BODY.len()
                    )
                    .as_bytes(),
                );
            }
        });
        format!("http://{addr}/")
    }

    fn spawn_cf_challenge_origin() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                const BODY: &str = "are you human?";
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\ncf-mitigated: challenge\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
                        BODY.len()
                    )
                    .as_bytes(),
                );
            }
        });
        format!("http://{addr}/")
    }

    /// Minimal forward-proxy stub (portable, no unix-only socket options):
    /// reads whatever the client sends and always answers 200 with a marker
    /// body. Distinct from `spawn_stub_proxy` (which is `#[cfg(unix)]`-only)
    /// so these mid-loop tests run on every platform.
    fn spawn_ok_proxy(body: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        format!("http://{addr}")
    }

    /// End-to-end: an origin that answers 429 (its egress-IP rate limit) is
    /// retried once through the fallback proxy and the PROXY's response wins.
    /// This is the mid-loop arm (`should_arm_proxy`) exercised over a real
    /// HTTP round trip, distinct from the `proxy_first` (latched) tests above.
    ///
    /// `has_static_proxy: true` here is a test-only trick to keep the
    /// process-wide egress latch (`crate::egress::global()`) untouched: that
    /// write hook is gated on `!has_static_proxy`, and it is a 10-minute-TTL
    /// singleton keyed by host string ("127.0.0.1") that every other test in
    /// this file also targets — a real write here would leak into them. The
    /// field is read nowhere else in the retry state machine, so this does
    /// not affect the behaviour under test.
    #[tokio::test]
    async fn ratelimit_429_arms_proxy_and_rescues_response() {
        let origin = spawn_429_origin();
        let proxy = spawn_ok_proxy("served via proxy after 429");
        let fetcher = HttpFetcher {
            client: reqwest::Client::new(),
            relaxed_client: None,
            has_static_proxy: true,
            ratelimit_proxy_client: Some(
                build_client("ua", Some(&proxy), std::time::Duration::from_secs(5), false).unwrap(),
            ),
            inject_stealth_headers: false,
        };
        let res = fetcher
            .fetch(
                &origin,
                &HashMap::new(),
                None,
                Deadline::from_request_ms(10_000),
            )
            .await
            .expect("429 must be rescued through the armed proxy");
        assert_eq!(res.status_code, 200);
        assert!(
            res.html.contains("served via proxy after 429"),
            "got: {}",
            res.html
        );
    }

    #[tokio::test]
    async fn challenge_header_arms_proxy_and_rescues_response() {
        let origin = spawn_cf_challenge_origin();
        let proxy = spawn_ok_proxy("clean page via proxy");
        let fetcher = HttpFetcher {
            client: reqwest::Client::new(),
            relaxed_client: None,
            has_static_proxy: true,
            ratelimit_proxy_client: Some(
                build_client("ua", Some(&proxy), std::time::Duration::from_secs(5), false).unwrap(),
            ),
            inject_stealth_headers: false,
        };
        let res = fetcher
            .fetch(
                &origin,
                &HashMap::new(),
                None,
                Deadline::from_request_ms(10_000),
            )
            .await
            .expect("cf-mitigated challenge must be rescued through the armed proxy");
        assert_eq!(res.status_code, 200);
        assert!(
            res.html.contains("clean page via proxy"),
            "got: {}",
            res.html
        );
        // The FINAL result is the proxy's clean response, which carries no
        // cf-mitigated header, so no challenge warning should be stamped.
        assert!(res.warning.is_none(), "got warning: {:?}", res.warning);
    }

    /// The 429 mid-loop arm deliberately has NO reserve (unlike `proxy_first`
    /// and the challenge-header arm — see the `attempt_budget` comment: "the
    /// 429 arm predates this change and its budget behaviour is deliberately
    /// left byte-identical"). A hanging proxy after a 429 can therefore eat
    /// the WHOLE deadline and starve the direct rescue entirely. This pins
    /// that asymmetry: a short deadline plus a hanging proxy on this specific
    /// arm produces a Timeout, not a rescued direct response.
    #[tokio::test]
    async fn ratelimit_429_arm_has_no_reserve_hanging_proxy_can_starve_direct() {
        let origin = spawn_429_origin();
        let p = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let paddr = p.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = p.accept().await {
                held.push(sock); // hold it open, write nothing
            }
        });
        let fetcher = HttpFetcher {
            client: reqwest::Client::new(),
            relaxed_client: None,
            has_static_proxy: true,
            ratelimit_proxy_client: Some(
                build_client(
                    "ua",
                    Some(&format!("http://{paddr}")),
                    std::time::Duration::from_secs(5),
                    false,
                )
                .unwrap(),
            ),
            inject_stealth_headers: false,
        };
        let err = fetcher
            .fetch(
                &origin,
                &HashMap::new(),
                None,
                Deadline::from_request_ms(300),
            )
            .await
            .expect_err("a hanging proxy on the un-reserved 429 arm must exhaust the deadline");
        assert!(matches!(err, CrwError::Timeout(_)), "got {err:?}");
    }
}
