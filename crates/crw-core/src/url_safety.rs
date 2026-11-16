use std::net::{IpAddr, ToSocketAddrs};

/// Build a reqwest redirect policy that validates each redirect target
/// against the SSRF safety checks. This prevents attackers from using
/// `https://evil.com` → 302 → `http://169.254.169.254/metadata` bypasses.
pub fn safe_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else if let Err(e) = check_redirect_target(attempt.url()) {
            attempt.error(e)
        } else {
            attempt.follow()
        }
    })
}

/// A destination the SSRF policy refuses: a blocked literal address, a blocked
/// host name, or a name that resolved to a blocked address.
///
/// Typed so it survives the transport's error wrapping: the renderer ladder
/// must stop on it (a later tier would only retry the same destination, and a
/// browser tier has weaker guards than the one that refused), while a DNS
/// failure or a malformed URL stays an ordinary failure the ladder may recover.
#[derive(Debug)]
pub struct DestinationRefused(pub String);

impl std::fmt::Display for DestinationRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DestinationRefused {}

/// The [`DestinationRefused`] in `e` or anywhere in its source chain.
pub fn refused_destination<'a>(
    e: &'a (dyn std::error::Error + 'static),
) -> Option<&'a DestinationRefused> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        if let Some(refused) = err.downcast_ref::<DestinationRefused>() {
            return Some(refused);
        }
        cur = err.source();
    }
    None
}

/// The verdict on one redirect hop, shared by the reqwest and wreq policies.
///
/// A policy refusal is a [`DestinationRefused`]; a URL-shape problem or a DNS
/// failure is a plain `"redirect blocked: ..."` message, whose text
/// `http_only::dead_redirect_reason` matches.
pub fn check_redirect_target(
    url: &url::Url,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // A non-HTTP target (`mailto:`) has no host to refuse; it is a dead link,
    // left to the shape check below.
    if matches!(url.scheme(), "http" | "https") {
        validate_safe_host(url).map_err(DestinationRefused)?;
    }
    let blocked = |e: String| -> Box<dyn std::error::Error + Send + Sync> {
        format!("redirect blocked: {e}").into()
    };
    validate_safe_url(url).map_err(blocked)?;
    if std::env::var("CRW_ALLOW_LOOPBACK_FOR_TESTS").as_deref() == Ok("1") {
        return Ok(());
    }
    let host = url
        .host_str()
        .ok_or_else(|| blocked("URL has no host".into()))?;
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if stripped.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| blocked("URL has no resolvable port".into()))?;
    // Blocking DNS: reqwest's redirect callback cannot await.
    let ips: Vec<IpAddr> = (stripped, port)
        .to_socket_addrs()
        .map_err(|_| blocked("DNS resolution failed".into()))?
        .map(|addr| addr.ip())
        .collect();
    if ips.is_empty() {
        return Err(blocked("DNS resolution returned no addresses".into()));
    }
    validate_resolved_ips(ips).map_err(|e| Box::new(DestinationRefused(e)) as _)
}

/// Validate that a URL is safe to fetch (not targeting internal/private networks).
///
/// Blocks:
/// - Non-http(s) schemes
/// - Loopback addresses (127.x, ::1, localhost)
/// - Private IP ranges (10.x, 172.16-31.x, 192.168.x)
/// - Link-local addresses (169.254.x — e.g. AWS metadata endpoint)
/// - 0.0.0.0
///
/// **Test-only escape hatch**: when the env var
/// `CRW_ALLOW_LOOPBACK_FOR_TESTS=1` is set, the loopback/private-range
/// checks are skipped so wiremock-backed integration tests can target
/// `127.0.0.1:<random>`. The opt-in is read at every call (cheap) so it
/// can be flipped per-test. Never set this in production.
pub fn validate_safe_url(url: &url::Url) -> Result<(), String> {
    // URL length limit
    const MAX_URL_LENGTH: usize = 2048;
    if url.as_str().len() > MAX_URL_LENGTH {
        return Err(format!(
            "URL exceeds maximum length of {MAX_URL_LENGTH} characters"
        ));
    }

    // Null byte check
    if url.as_str().contains("%00") || url.as_str().contains('\0') {
        return Err("URL contains null bytes".into());
    }

    // Scheme check
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Only http/https URLs are allowed".into());
    }

    validate_safe_host(url)
}

/// The destination half of [`validate_safe_url`]: deny-listed host names and
/// blocked literal address ranges, without the URL-shape rules (length, null
/// bytes, scheme).
///
/// Callers that validate a URL the *caller* supplied want the full
/// [`validate_safe_url`]. Callers that validate a URL the *browser* produced —
/// a redirect target, a subresource — want this one: a signed CDN URL past the
/// length cap is not an SSRF, and failing it would drop a legitimate request.
pub fn validate_safe_host(url: &url::Url) -> Result<(), String> {
    let test_allow_loopback = std::env::var("CRW_ALLOW_LOOPBACK_FOR_TESTS").as_deref() == Ok("1");
    let host = url
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;

    if !test_allow_loopback && is_blocked_host_name(host) {
        return Err("host is not allowed".into());
    }

    // Parse as IP if possible and check ranges
    if !test_allow_loopback
        && let Ok(ip) = host.parse::<IpAddr>()
        && is_blocked_ip(&ip)
    {
        return Err(format!("Access to {ip} is not allowed"));
    }

    // Also check bracket-stripped IPv6 (e.g. [::1])
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if !test_allow_loopback
        && let Ok(ip) = stripped.parse::<IpAddr>()
        && is_blocked_ip(&ip)
    {
        return Err(format!("Access to {ip} is not allowed"));
    }

    Ok(())
}

/// Validate the URL and resolve DNS for hostname targets, rejecting any private,
/// loopback, link-local, or otherwise non-public address. This is intentionally
/// fail-closed: if DNS cannot be resolved, callers cannot prove the destination
/// is public and must not fetch it.
pub async fn validate_safe_url_resolved(url: &url::Url) -> Result<(), String> {
    validate_safe_url(url)?;
    resolve_and_validate(url).await
}

/// [`validate_safe_host`] plus DNS resolution. The destination check for URLs
/// the browser produced rather than the caller; see [`validate_safe_host`] for
/// why the URL-shape rules are excluded.
pub async fn validate_safe_host_resolved(url: &url::Url) -> Result<(), String> {
    validate_safe_host(url)?;
    resolve_and_validate(url).await
}

/// Why a destination was refused.
///
/// The two cases look identical to the caller and are opposite in meaning: one
/// is a destination we refuse to reach, the other is us failing to find out.
/// Conflating them reports a blocked internal address as our own outage, and a
/// resolver brown-out as the caller's unreachable target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRejection {
    /// The destination resolved to, or literally is, an address we refuse.
    Policy(String),
    /// We could not establish where the destination points.
    Unresolved(String),
}

/// [`validate_safe_host_resolved`], with the reason kept apart.
pub async fn classify_safe_host_resolved(url: &url::Url) -> Result<(), HostRejection> {
    validate_safe_host(url).map_err(HostRejection::Policy)?;

    let test_allow_loopback = std::env::var("CRW_ALLOW_LOOPBACK_FOR_TESTS").as_deref() == Ok("1");
    if test_allow_loopback {
        return Ok(());
    }
    let host = url
        .host_str()
        .ok_or_else(|| HostRejection::Policy("URL has no host".to_string()))?;
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if stripped.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| HostRejection::Unresolved("URL has no resolvable port".to_string()))?;
    let addrs = tokio::time::timeout(
        DNS_RESOLVE_TIMEOUT,
        tokio::net::lookup_host((stripped, port)),
    )
    .await
    .map_err(|_| HostRejection::Unresolved("DNS resolution timed out".to_string()))?
    .map_err(|_| HostRejection::Unresolved("DNS resolution failed".to_string()))?;
    // An answer with no addresses is a failure to find out, not a policy call.
    let ips: Vec<IpAddr> = addrs.map(|addr| addr.ip()).collect();
    if ips.is_empty() {
        return Err(HostRejection::Unresolved(
            "DNS resolution returned no addresses".into(),
        ));
    }
    validate_resolved_ips(ips).map_err(HostRejection::Policy)
}

async fn resolve_and_validate(url: &url::Url) -> Result<(), String> {
    let test_allow_loopback = std::env::var("CRW_ALLOW_LOOPBACK_FOR_TESTS").as_deref() == Ok("1");
    if test_allow_loopback {
        return Ok(());
    }

    let host = url
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if stripped.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "URL has no resolvable port".to_string())?;
    // Bound the lookup. `lookup_host` inherits the OS resolver's own retry
    // behaviour, which against a stalled or blackholed nameserver can hang for
    // tens of seconds — long past any route backstop. This is the single choke
    // point every SSRF caller routes through, so bounding it here covers the ~14
    // callers that have no request-scoped timeout of their own; the two that do
    // (crw-crawl seed + BFS) keep winning since the tighter timeout applies.
    // Fails CLOSED: an elapsed lookup maps to the same rejection a resolver error
    // already produces.
    let addrs = tokio::time::timeout(
        DNS_RESOLVE_TIMEOUT,
        tokio::net::lookup_host((stripped, port)),
    )
    .await
    .map_err(|_| "DNS resolution timed out".to_string())?
    .map_err(|_| "DNS resolution failed".to_string())?;
    validate_resolved_ips(addrs.map(|addr| addr.ip()))
}

/// Resolve `host` and refuse the answer if any address is blocked, returning
/// the addresses that passed. The connect-time half of the SSRF guard: a
/// client that dials what this returns dials what was checked, so a name that
/// answers "public" to the pre-flight check and "private" a moment later (DNS
/// rebinding) cannot move the socket.
///
/// `exempt` names hosts resolved without the check: an operator-configured
/// proxy, which may legitimately live on a private address.
pub async fn resolve_checked(host: &str, exempt: &[String]) -> Result<Vec<IpAddr>, HostRejection> {
    let addrs = tokio::time::timeout(DNS_RESOLVE_TIMEOUT, tokio::net::lookup_host((host, 0)))
        .await
        .map_err(|_| HostRejection::Unresolved("DNS resolution timed out".into()))?
        .map_err(|_| HostRejection::Unresolved("DNS resolution failed".into()))?;
    let ips: Vec<IpAddr> = addrs.map(|addr| addr.ip()).collect();
    if ips.is_empty() {
        return Err(HostRejection::Unresolved(
            "DNS resolution returned no addresses".into(),
        ));
    }
    let test_allow_loopback = std::env::var("CRW_ALLOW_LOOPBACK_FOR_TESTS").as_deref() == Ok("1");
    let exempted = exempt.iter().any(|e| e.eq_ignore_ascii_case(host));
    if !test_allow_loopback && !exempted {
        validate_resolved_ips(ips.iter().copied()).map_err(HostRejection::Policy)?;
    }
    Ok(ips)
}

/// reqwest resolver backed by [`resolve_checked`]. Install it on every client
/// that fetches a caller- or page-controlled URL.
///
/// With an HTTP(S) proxy reqwest resolves only the proxy host, never the
/// target, so the proxy's host must be in `exempt` or a proxy on a private
/// address stops working. IP-literal hosts skip DNS entirely and stay covered
/// by [`validate_safe_url`] and [`safe_redirect_policy`].
pub struct SsrfResolver {
    exempt: std::sync::Arc<[String]>,
}

impl SsrfResolver {
    /// Exempts `proxy`'s host (when given) plus every environment proxy host.
    pub fn new(proxy: Option<&str>) -> Self {
        let mut exempt = env_proxy_hosts();
        exempt.extend(proxy.and_then(proxy_host));
        Self {
            exempt: exempt.into(),
        }
    }

    /// The exemption list, for transport adapters that cannot use this type
    /// directly (the wreq tier).
    pub fn exempt(&self) -> std::sync::Arc<[String]> {
        self.exempt.clone()
    }
}

impl reqwest::dns::Resolve for SsrfResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let exempt = self.exempt.clone();
        let host = name.as_str().to_string();
        Box::pin(async move {
            let ips = resolve_checked(&host, &exempt)
                .await
                .map_err(resolver_error)?;
            let addrs: reqwest::dns::Addrs =
                Box::new(ips.into_iter().map(|ip| std::net::SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

/// A [`resolve_checked`] failure as a transport resolver error: a policy
/// refusal stays a typed [`DestinationRefused`] through the transport's
/// wrapping, a DNS failure is an ordinary error.
pub fn resolver_error(e: HostRejection) -> Box<dyn std::error::Error + Send + Sync> {
    match e {
        HostRejection::Policy(m) => Box::new(DestinationRefused(m)),
        HostRejection::Unresolved(m) => m.into(),
    }
}

/// Hosts of the proxies reqwest picks up from the environment on its own
/// (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, either case). Operator-set, so
/// trusted like a configured proxy.
pub fn env_proxy_hosts() -> Vec<String> {
    [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok())
    .filter_map(|v| proxy_host(&v))
    .collect()
}

/// Host of a proxy URL. reqwest accepts a bare `host:port` for env proxies and
/// assumes `http://`, so this does too.
fn proxy_host(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed = url::Url::parse(raw)
        .ok()
        .filter(|u| u.has_host())
        .or_else(|| url::Url::parse(&format!("http://{raw}")).ok())?;
    let host = parsed.host_str()?;
    Some(
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .to_string(),
    )
}

/// Upper bound on a single SSRF DNS resolution. Purpose is to bound a stalled /
/// blackholed resolver, not to race a slow-but-working one: recall is the hard
/// invariant, so this sits high enough (8s) to still resolve a legitimate host
/// behind a CNAME chain or a slow authoritative server that needs a UDP retry,
/// while staying under the search enrichment per-result budget. Two orders of
/// magnitude above a healthy lookup (tens of ms).
const DNS_RESOLVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Synchronous resolved validation for places that cannot await, notably
/// reqwest's redirect-policy callback. Keep use narrow; it performs DNS on the
/// current thread and intentionally fails closed.
pub fn validate_safe_url_blocking_resolved(url: &url::Url) -> Result<(), String> {
    validate_safe_url(url)?;

    let test_allow_loopback = std::env::var("CRW_ALLOW_LOOPBACK_FOR_TESTS").as_deref() == Ok("1");
    if test_allow_loopback {
        return Ok(());
    }

    let host = url
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if stripped.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "URL has no resolvable port".to_string())?;
    (stripped, port)
        .to_socket_addrs()
        .map_err(|_| "DNS resolution failed".to_string())
        .and_then(|addrs| validate_resolved_ips(addrs.map(|addr| addr.ip())))
}

pub fn validate_resolved_ips<I>(ips: I) -> Result<(), String>
where
    I: IntoIterator<Item = IpAddr>,
{
    let mut saw_ip = false;
    for ip in ips {
        saw_ip = true;
        if is_blocked_ip(&ip) {
            return Err(format!("Access to {ip} is not allowed"));
        }
    }
    if !saw_ip {
        return Err("DNS resolution returned no addresses".into());
    }
    Ok(())
}

fn is_blocked_host_name(host: &str) -> bool {
    let host_lower = host.to_lowercase();
    let host_lower = host_lower.trim_end_matches('.');
    host_lower == "localhost"
        || host_lower == "metadata.google.internal"
        || host_lower.ends_with(".localhost")
        || host_lower.ends_with(".localtest.me")
        || host_lower.ends_with(".lvh.me")
        || host_lower.ends_with(".nip.io")
        || host_lower.ends_with(".xip.io")
        || host_lower.ends_with(".sslip.io")
}

fn is_blocked_ipv4(v4: &std::net::Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    v4.is_loopback()                       // 127.0.0.0/8
        || v4.is_private()                 // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
        || v4.is_link_local()              // 169.254.0.0/16 (metadata)
        || v4.is_unspecified()             // 0.0.0.0
        || v4.is_broadcast()               // 255.255.255.255
        || (a == 100 && (64..=127).contains(&b)) // carrier-grade NAT
        || a == 0
        || a >= 224                        // multicast/reserved
        // Reserved blocks inside otherwise-public /16s. Match the exact /24s,
        // not the whole /16: e.g. 192.0.78.0/24 is WordPress.com/Automattic and
        // must stay reachable — over-blocking 192.0.0.0/16 silently dropped
        // legitimate sources.
        || (a == 192 && b == 0 && (c == 0 || c == 2)) // 192.0.0.0/24 + TEST-NET-1 192.0.2.0/24
        || (a == 198 && (b == 18 || b == 19))         // 198.18.0.0/15 benchmarking
        || (a == 198 && b == 51 && c == 100)          // TEST-NET-2 198.51.100.0/24
        || (a == 203 && b == 0 && c == 113) // TEST-NET-3 203.0.113.0/24
}

/// IPv4 address a v6 address actually reaches through a translation prefix.
///
/// NAT64 (`64:ff9b::/96`) and 6to4 (`2002::/16`) both carry a real IPv4
/// destination in their low bits, and on a network that implements them the
/// host connects to *that* address. Returning the embedded address rather than
/// blocking the whole prefix keeps legitimate public IPv4 reachable through a
/// DNS64/NAT64 resolver, which is the common case on IPv6-only networks.
fn embedded_ipv4(v6: &std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    let s = v6.segments();
    // Well-known NAT64 prefix (RFC 6052): the IPv4 address sits in the last
    // 32 bits.
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0 {
        return Some(std::net::Ipv4Addr::from(
            ((s[6] as u32) << 16) | (s[7] as u32),
        ));
    }
    // 6to4 (RFC 3056): 2002:<v4>::/48.
    if s[0] == 0x2002 {
        return Some(std::net::Ipv4Addr::from(
            ((s[1] as u32) << 16) | (s[2] as u32),
        ));
    }
    None
}

fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => {
            v6.is_loopback()                       // ::1
                || v6.is_unspecified()              // ::
                // IPv4-mapped (::ffff:127.0.0.1) AND IPv4-compatible (::7f00:1).
                // `to_ipv4` covers both ::/96 and ::ffff:0:0/96; `to_ipv4_mapped`
                // covered only the latter, so `http://[::7f00:1]/` reached
                // loopback.
                || v6.to_ipv4().is_some_and(|v4| is_blocked_ipv4(&v4))
                // Addresses reached through a v4 translation prefix.
                || embedded_ipv4(v6).is_some_and(|v4| is_blocked_ipv4(&v4))
                // Local-use translation prefix (RFC 8215, 64:ff9b:1::/48). It
                // admits several RFC 6052 prefix lengths, so the embedded v4
                // cannot be decoded unambiguously; block the whole /48 rather
                // than guess. It is local-use by definition, never a public
                // destination.
                || (v6.segments()[0] == 0x0064
                    && v6.segments()[1] == 0xff9b
                    && v6.segments()[2] == 0x0001)
                // IPv6 link-local (fe80::/10)
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // Deprecated site-local (fec0::/10), still routed on some networks.
                || (v6.segments()[0] & 0xffc0) == 0xfec0
                // IPv6 unique-local / ULA (fc00::/7) — private network equivalent
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // IPv6 multicast and special/reserved ranges.
                || (v6.segments()[0] & 0xff00) == 0xff00
                || (v6.segments()[0] & 0xff00) == 0x0200
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> url::Url {
        url::Url::parse(s).unwrap()
    }

    #[test]
    fn allows_normal_urls() {
        assert!(validate_safe_url(&url("https://example.com")).is_ok());
        assert!(validate_safe_url(&url("http://example.com/path")).is_ok());
    }

    #[test]
    fn blocks_non_http_schemes() {
        assert!(validate_safe_url(&url("ftp://example.com")).is_err());
        assert!(validate_safe_url(&url("file:///etc/passwd")).is_err());
    }

    #[test]
    fn blocks_localhost() {
        assert!(validate_safe_url(&url("http://localhost")).is_err());
        assert!(validate_safe_url(&url("http://localhost:8080")).is_err());
        assert!(validate_safe_url(&url("http://127.0.0.1")).is_err());
        assert!(validate_safe_url(&url("http://127.0.0.1:9999")).is_err());
        assert!(validate_safe_url(&url("http://metadata.google.internal")).is_err());
        assert!(validate_safe_url(&url("http://127.0.0.1.nip.io")).is_err());
    }

    #[test]
    fn blocks_private_ips() {
        assert!(validate_safe_url(&url("http://10.0.0.1")).is_err());
        assert!(validate_safe_url(&url("http://172.16.0.1")).is_err());
        assert!(validate_safe_url(&url("http://192.168.1.1")).is_err());
    }

    #[test]
    fn blocks_link_local() {
        assert!(validate_safe_url(&url("http://169.254.169.254/latest/meta-data/")).is_err());
    }

    #[test]
    fn blocks_zero_ip() {
        assert!(validate_safe_url(&url("http://0.0.0.0")).is_err());
        assert!(validate_safe_url(&url("http://100.64.0.1")).is_err());
        assert!(validate_safe_url(&url("http://224.0.0.1")).is_err());
    }

    #[test]
    fn blocks_only_reserved_24s_not_whole_16() {
        // Reserved /24s stay blocked.
        assert!(validate_safe_url(&url("http://192.0.0.1")).is_err()); // 192.0.0.0/24
        assert!(validate_safe_url(&url("http://192.0.2.1")).is_err()); // TEST-NET-1
        assert!(validate_safe_url(&url("http://198.18.0.1")).is_err()); // benchmarking /15
        assert!(validate_safe_url(&url("http://198.19.0.1")).is_err()); // benchmarking /15
        assert!(validate_safe_url(&url("http://198.51.100.1")).is_err()); // TEST-NET-2
        assert!(validate_safe_url(&url("http://203.0.113.1")).is_err()); // TEST-NET-3

        // Public addresses in the SAME /16s must be reachable (were false-positives).
        assert!(validate_safe_url(&url("http://192.0.78.24")).is_ok()); // WordPress.com/Automattic
        assert!(validate_safe_url(&url("http://198.51.99.1")).is_ok()); // outside TEST-NET-2 /24
        assert!(validate_safe_url(&url("http://203.0.112.1")).is_ok()); // outside TEST-NET-3 /24
    }

    #[test]
    fn blocks_ipv6_loopback() {
        assert!(validate_safe_url(&url("http://[::1]")).is_err());
    }

    #[test]
    fn blocks_ipv4_mapped_ipv6() {
        // ::ffff:127.0.0.1 — IPv4-mapped loopback
        assert!(validate_safe_url(&url("http://[::ffff:127.0.0.1]")).is_err());
        // ::ffff:169.254.169.254 — IPv4-mapped AWS metadata
        assert!(validate_safe_url(&url("http://[::ffff:169.254.169.254]")).is_err());
        // ::ffff:10.0.0.1 — IPv4-mapped private
        assert!(validate_safe_url(&url("http://[::ffff:10.0.0.1]")).is_err());
    }

    #[test]
    fn blocks_ipv6_link_local() {
        assert!(validate_safe_url(&url("http://[fe80::1]")).is_err());
    }

    #[test]
    fn blocks_ipv4_compatible_and_translated_forms() {
        // IPv4-compatible ::a.b.c.d — reaches loopback, was allowed.
        assert!(validate_safe_url(&url("http://[::7f00:1]")).is_err());
        // NAT64 well-known prefix carrying loopback / metadata / RFC1918.
        assert!(validate_safe_url(&url("http://[64:ff9b::7f00:1]")).is_err());
        assert!(validate_safe_url(&url("http://[64:ff9b::a9fe:a9fe]")).is_err());
        assert!(validate_safe_url(&url("http://[64:ff9b::a00:1]")).is_err());
        // 6to4 carrying a private address.
        assert!(validate_safe_url(&url("http://[2002:a00:1::1]")).is_err());
        // RFC 8215 local-use translation prefix, blocked wholesale.
        assert!(validate_safe_url(&url("http://[64:ff9b:1::5db8:d822]")).is_err());
        // Deprecated site-local.
        assert!(validate_safe_url(&url("http://[fec0::1]")).is_err());
        // Public IPv4 behind the same translation prefixes must stay reachable.
        assert!(validate_safe_url(&url("http://[64:ff9b::5db8:d822]")).is_ok()); // 93.184.216.34
        assert!(validate_safe_url(&url("http://[2002:5db8:d822::1]")).is_ok());
        // An ordinary public IPv6 address is unaffected.
        assert!(validate_safe_url(&url("http://[2606:4700:4700::1111]")).is_ok());
    }

    #[test]
    fn blocks_ipv6_ula() {
        assert!(validate_safe_url(&url("http://[fc00::1]")).is_err());
        assert!(validate_safe_url(&url("http://[fd00::1]")).is_err());
    }

    #[tokio::test]
    async fn rejection_reason_separates_policy_from_resolver_failure() {
        // A literal blocked address is a policy call.
        assert!(matches!(
            classify_safe_host_resolved(&url("http://169.254.169.254/")).await,
            Err(HostRejection::Policy(_))
        ));
        assert!(matches!(
            classify_safe_host_resolved(&url("http://localhost/")).await,
            Err(HostRejection::Policy(_))
        ));
        // A host that cannot be resolved is not.
        assert!(matches!(
            classify_safe_host_resolved(&url("https://nonexistent.invalid/")).await,
            Err(HostRejection::Unresolved(_))
        ));
    }

    #[test]
    fn host_check_covers_destinations_without_the_url_shape_rules() {
        // Same destination verdicts as the full check.
        assert!(validate_safe_host(&url("http://169.254.169.254/")).is_err());
        assert!(validate_safe_host(&url("http://10.0.0.1/")).is_err());
        assert!(validate_safe_host(&url("http://localhost/")).is_err());
        assert!(validate_safe_host(&url("https://example.com/")).is_ok());
        // But a long URL is a shape problem, not an SSRF: the renderer guard
        // runs this against browser-produced subresource URLs, and signed CDN
        // links routinely exceed the caller-facing cap.
        let long = format!("https://example.com/{}", "a".repeat(3000));
        assert!(validate_safe_url(&url(&long)).is_err());
        assert!(validate_safe_host(&url(&long)).is_ok());
    }

    #[test]
    fn blocks_extremely_long_urls() {
        let long = format!("https://example.com/{}", "a".repeat(3000));
        assert!(validate_safe_url(&url(&long)).is_err());
    }

    #[test]
    fn allows_url_within_length_limit() {
        let ok = format!("https://example.com/{}", "a".repeat(1000));
        assert!(validate_safe_url(&url(&ok)).is_ok());
    }

    #[test]
    fn safe_redirect_policy_exists() {
        // Verify the policy is constructible (runtime redirect validation
        // is tested via integration tests with actual HTTP redirects).
        let _policy = super::safe_redirect_policy();
    }

    #[test]
    fn resolved_ips_fail_closed_for_private_or_empty_answers() {
        assert!(validate_resolved_ips([IpAddr::from([93, 184, 216, 34])]).is_ok());
        assert!(
            validate_resolved_ips([
                IpAddr::from([93, 184, 216, 34]),
                IpAddr::from([10, 0, 0, 1])
            ])
            .is_err()
        );
        assert!(validate_resolved_ips([]).is_err());
    }

    #[test]
    fn blocking_resolved_validation_rejects_denied_literal_ips() {
        assert!(validate_safe_url_blocking_resolved(&url("http://169.254.169.254")).is_err());
        assert!(validate_safe_url_blocking_resolved(&url("http://127.0.0.1")).is_err());
        assert!(validate_safe_url_blocking_resolved(&url("http://[::1]")).is_err());
    }

    #[tokio::test]
    async fn resolution_fails_closed_and_bounded() {
        // `.invalid` is reserved (RFC 6761) and never resolves. The result must be
        // a fail-closed `Err`, and it must return in well under the DNS timeout
        // (proving the wrapper doesn't hang the caller). NXDOMAIN returns fast; the
        // bound here only asserts we never sit past the timeout.
        let start = std::time::Instant::now();
        let res = validate_safe_url_resolved(&url("https://nonexistent.invalid")).await;
        assert!(res.is_err());
        assert!(start.elapsed() < DNS_RESOLVE_TIMEOUT + std::time::Duration::from_secs(1));
    }

    #[tokio::test]
    async fn connect_time_resolver_refuses_blocked_answers_unless_exempt() {
        // `localhost` answers loopback: the pinned resolver must refuse it, so a
        // rebinding name that flips to a private address cannot be dialed.
        assert!(matches!(
            resolve_checked("localhost", &[]).await,
            Err(HostRejection::Policy(_))
        ));
        // An operator proxy on a private address stays reachable.
        let ips = resolve_checked("localhost", &["LOCALHOST".to_string()])
            .await
            .expect("exempt host resolves");
        assert!(ips.iter().all(|ip| ip.is_loopback()));
    }

    #[test]
    fn proxy_host_reads_urls_and_bare_host_port() {
        assert_eq!(
            proxy_host("http://u:p@squid:3128").as_deref(),
            Some("squid")
        );
        assert_eq!(
            proxy_host("socks5h://10.0.0.5:1080").as_deref(),
            Some("10.0.0.5")
        );
        assert_eq!(proxy_host("proxy.corp:8080").as_deref(), Some("proxy.corp"));
        assert_eq!(proxy_host("http://[::1]:3128").as_deref(), Some("::1"));
        assert_eq!(proxy_host("  "), None);
    }

    #[test]
    fn redirect_verdict_types_policy_refusals_only() {
        let refused = check_redirect_target(&url("http://169.254.169.254/latest")).unwrap_err();
        assert!(refused_destination(refused.as_ref()).is_some());
        let refused = check_redirect_target(&url("http://localhost/")).unwrap_err();
        assert!(refused_destination(refused.as_ref()).is_some());
        // Shape and DNS failures stay recoverable, with the text
        // `dead_redirect_reason` matches.
        let scheme = check_redirect_target(&url("mailto:a@example.com")).unwrap_err();
        assert!(refused_destination(scheme.as_ref()).is_none());
        assert!(
            scheme
                .to_string()
                .contains("Only http/https URLs are allowed")
        );
        let dns = check_redirect_target(&url("https://nonexistent.invalid/")).unwrap_err();
        assert!(refused_destination(dns.as_ref()).is_none());
        assert!(dns.to_string().contains("DNS resolution"));
        let long = format!("https://example.com/{}", "a".repeat(3000));
        let long = check_redirect_target(&url(&long)).unwrap_err();
        assert!(refused_destination(long.as_ref()).is_none());
    }
}
