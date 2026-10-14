//! Proxy list + rotation primitives shared across the HTTP, crawl, and CDP
//! paths.
//!
//! A [`ProxyRotator`] holds a set of validated [`ProxyEntry`] and selects one
//! per request according to a [`ProxyRotation`] strategy. The rotator is built
//! once (from config) or per request (BYOP) and is cheap to share behind an
//! `Arc`.
//!
//! # Safety
//!
//! Proxy URLs are validated up front via [`ProxyEntry::parse`]. A malformed
//! entry is a hard error — we never silently fall back to a direct (no-proxy)
//! connection, which would leak the host's real IP. Callers map the returned
//! error string to the appropriate [`crate::CrwError`] variant
//! (`ConfigError` at startup, `InvalidRequest` for per-request BYOP).

use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

/// Strategy for selecting a proxy from a [`ProxyRotator`]'s pool.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyRotation {
    /// Cycle through the pool in order, one step per request (process-wide).
    RoundRobin,
    /// Pick a uniformly random entry per request.
    Random,
    /// Pin each target host to a single proxy for the rotator's lifetime.
    /// Default: keeps cookie/TLS sessions coherent per host (anti-bot systems
    /// flag mid-session IP changes), while still spreading load across hosts.
    #[default]
    StickyPerHost,
}

/// A single validated proxy endpoint.
///
/// `raw` carries the full URL (including any `user:pass`) for `reqwest`, which
/// honours embedded credentials. `chrome_proxy_server` is the scheme-qualified
/// `host:port` **without** credentials, suitable for Chrome's
/// `Target.createBrowserContext { proxyServer }` (Chrome takes creds via the
/// `Fetch.authRequired` auth pump, not in the URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyEntry {
    raw: String,
    scheme: String,
    chrome_proxy_server: String,
    auth: Option<(String, String)>,
}

const ALLOWED_SCHEMES: [&str; 4] = ["http", "https", "socks5", "socks5h"];

/// `raw` with any `user:pass@` userinfo replaced by `***@`.
///
/// Proxy URLs carry credentials and end up in `ConfigError` messages that are
/// returned to API callers verbatim, so no error string may ever quote a proxy
/// URL directly. Purely textual on purpose: it must also redact a value that
/// failed to parse as a URL, which is exactly when these errors fire.
pub fn redact_proxy_url(raw: &str) -> String {
    let trimmed = raw.trim();
    // Keep the scheme only when it is one (RFC 3986: a letter followed by
    // letters, digits, `+`, `-`, `.`). Anything else before `://` may itself be
    // the credentials of a mangled value, so it is masked with the rest.
    let is_scheme = |s: &str| {
        let mut chars = s.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    };
    let (prefix, rest) = match trimmed.split_once("://") {
        Some((scheme, rest)) if is_scheme(scheme) => (format!("{scheme}://"), rest),
        _ => (String::new(), trimmed),
    };
    // Last `@` wins: a host never contains one, an unencoded password can.
    match rest.rsplit_once('@') {
        Some((_, host)) => format!("{prefix}***@{host}"),
        None => format!("{prefix}{rest}"),
    }
}

/// `text` with the userinfo of every `scheme://` URL inside it masked, via
/// [`redact_proxy_url`].
///
/// For free-form text that may quote a proxy URL somewhere in the middle, such
/// as an error cause chain about to be logged. A URL runs from its scheme to the
/// next whitespace, deliberately ignoring `)`, `,` and quotes: those are legal in
/// an unencoded password, and stopping at one would leave the rest of the
/// password after the cut in the clear.
pub(crate) fn redact_url_userinfo(text: &str) -> String {
    let is_scheme_byte = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.');
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(sep) = rest.find("://") {
        // Scheme bytes are ASCII, so stopping on any other byte (including the
        // tail of a multi-byte char) always lands on a char boundary.
        let start = rest.as_bytes()[..sep]
            .iter()
            .rposition(|&b| !is_scheme_byte(b))
            .map_or(0, |i| i + 1);
        let end = rest[sep..]
            .find(char::is_whitespace)
            .map_or(rest.len(), |i| sep + i);
        out.push_str(&rest[..start]);
        out.push_str(&redact_proxy_url(&rest[start..end]));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

impl ProxyEntry {
    /// Parse and validate a proxy URL. Returns an error string (no silent
    /// fallback) when the scheme is unsupported or the host is missing.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("empty proxy URL".to_string());
        }
        let redacted = redact_proxy_url(trimmed);
        let url =
            url::Url::parse(trimmed).map_err(|e| format!("invalid proxy URL '{redacted}': {e}"))?;

        let scheme = url.scheme().to_ascii_lowercase();
        if !ALLOWED_SCHEMES.contains(&scheme.as_str()) {
            return Err(format!(
                "unsupported proxy scheme '{scheme}' in '{redacted}' (allowed: http, https, socks5, socks5h)"
            ));
        }

        let host = url
            .host_str()
            .ok_or_else(|| format!("proxy URL '{redacted}' has no host"))?;

        // Chrome's `proxyServer` only understands `socks5` (which already does
        // remote DNS) — it does not recognize the `socks5h` scheme. Normalize so
        // the CDP path passes a scheme Chrome accepts. (`reqwest`/`raw` keeps the
        // original scheme for the HTTP path.)
        let chrome_scheme = if scheme == "socks5h" {
            "socks5"
        } else {
            &scheme
        };
        let chrome_proxy_server = match url.port() {
            Some(port) => format!("{chrome_scheme}://{host}:{port}"),
            None => format!("{chrome_scheme}://{host}"),
        };

        let auth = match (url.username(), url.password()) {
            ("", _) => None,
            (user, Some(pass)) => Some((percent_decode(user), percent_decode(pass))),
            (user, None) => Some((percent_decode(user), String::new())),
        };

        Ok(Self {
            raw: trimmed.to_string(),
            scheme,
            chrome_proxy_server,
            auth,
        })
    }

    /// Full proxy URL (with credentials) for `reqwest::Proxy::all`.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// URL scheme (lowercased): `http`, `https`, `socks5`, or `socks5h`.
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Scheme-qualified `host:port` (no credentials) for Chrome `proxyServer`.
    pub fn chrome_proxy_server(&self) -> &str {
        &self.chrome_proxy_server
    }

    /// Optional `(username, password)` for the CDP auth pump.
    pub fn auth(&self) -> Option<&(String, String)> {
        self.auth.as_ref()
    }

    /// Whether this proxy can authenticate on the Chrome/CDP path. Chrome's
    /// network stack never emits `Fetch.authRequired` for SOCKS proxies, so a
    /// `socks5`/`socks5h` proxy that carries credentials cannot authenticate via
    /// the CDP auth pump (it would hang/fail). HTTP/HTTPS proxies and
    /// credential-less SOCKS proxies are fine. The HTTP (reqwest) path is
    /// unaffected — it authenticates SOCKS natively via [`Self::raw`].
    pub fn supports_cdp_auth(&self) -> bool {
        !(self.scheme.starts_with("socks") && self.auth.is_some())
    }
}

/// Minimal percent-decoding for proxy userinfo (handles `%XX`). Credentials
/// frequently contain URL-encoded characters; `url` exposes them encoded.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// A pool of validated proxies plus a selection strategy.
///
/// Construct via [`ProxyRotator::build`]. Returns `Ok(None)` when there are no
/// proxies (caller then connects directly, preserving today's behaviour).
#[derive(Debug)]
pub struct ProxyRotator {
    entries: Vec<ProxyEntry>,
    strategy: ProxyRotation,
    rr_cursor: AtomicUsize,
}

impl ProxyRotator {
    /// Build a rotator with precedence: a non-empty `list` wins; otherwise the
    /// single `single` proxy becomes a pool of one; otherwise `Ok(None)`.
    ///
    /// Every entry is validated — any malformed URL is a hard error (no silent
    /// no-proxy fallback). The error is a human-readable string the caller maps
    /// to a [`crate::CrwError`].
    pub fn build(
        list: &[String],
        single: Option<&str>,
        strategy: ProxyRotation,
    ) -> Result<Option<Self>, String> {
        let raws: Vec<&str> = if !list.is_empty() {
            list.iter().map(String::as_str).collect()
        } else if let Some(s) = single.map(str::trim).filter(|s| !s.is_empty()) {
            vec![s]
        } else {
            return Ok(None);
        };

        let mut entries = Vec::with_capacity(raws.len());
        for raw in raws {
            entries.push(ProxyEntry::parse(raw)?);
        }
        if entries.is_empty() {
            return Ok(None);
        }

        Ok(Some(Self {
            entries,
            strategy,
            rr_cursor: AtomicUsize::new(0),
        }))
    }

    /// Number of proxies in the pool.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Always false — `build` returns `None` for empty pools.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Select a proxy for a request. `host_key` is used only by
    /// [`ProxyRotation::StickyPerHost`]; pass the normalized target host.
    pub fn pick(&self, host_key: Option<&str>) -> &ProxyEntry {
        &self.entries[self.pick_index(host_key)]
    }

    /// Index into the validated pool for this request, applying the strategy.
    ///
    /// `StickyPerHost` is **stateless**: the index is a deterministic hash of the
    /// host modulo the pool size. This keeps a host pinned to one proxy for the
    /// rotator's lifetime with no per-host map (no unbounded growth, no lock, and
    /// — crucially — no cursor side-effect, so repeated picks for the same host
    /// are idempotent and HTTP + CDP always agree).
    pub fn pick_index(&self, host_key: Option<&str>) -> usize {
        let len = self.entries.len();
        if len == 0 {
            return 0; // unreachable: `build` never yields an empty rotator.
        }
        match self.strategy {
            ProxyRotation::RoundRobin => self.next_rr() % len,
            ProxyRotation::Random => rand::random_range(0..len),
            ProxyRotation::StickyPerHost => match host_key {
                Some(host) => (fnv1a(host) % len as u64) as usize,
                None => self.next_rr() % len,
            },
        }
    }

    fn next_rr(&self) -> usize {
        self.rr_cursor.fetch_add(1, Ordering::Relaxed)
    }
}

/// FNV-1a 64-bit hash — small, stable, dependency-free. Used for stateless
/// sticky-per-host proxy assignment.
fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_url_userinfo_masks_every_url_in_free_text() {
        // The shape a proxy failure takes once its cause chain is flattened.
        assert_eq!(
            redact_url_userinfo(
                "error sending request: proxy http://user:s3cret@gw.example.com:823 refused"
            ),
            "error sending request: proxy http://***@gw.example.com:823 refused"
        );
        // Wrapped in parens, two URLs, a `)` inside the password: nothing of
        // the password survives, the surrounding text does.
        let out = redact_url_userinfo("a (socks5h://u:p)w@h1:1080) then https://v:x@h2/path, ok");
        assert!(!out.contains("p)w"), "{out}");
        assert!(!out.contains("v:x"), "{out}");
        assert_eq!(
            out,
            "a (socks5h://***@h1:1080) then https://***@h2/path, ok"
        );
        // No URL, or a URL without credentials: unchanged, multi-byte text too.
        assert_eq!(
            redact_url_userinfo("tunnel error: unsuccessful"),
            "tunnel error: unsuccessful"
        );
        assert_eq!(
            redact_url_userinfo("çağrı http://gw.example.com:823 bitti"),
            "çağrı http://gw.example.com:823 bitti"
        );
        assert_eq!(redact_url_userinfo("://u:p@h"), "***@h");
        assert_eq!(redact_url_userinfo(""), "");
    }

    #[test]
    fn redact_proxy_url_masks_userinfo() {
        assert_eq!(
            redact_proxy_url("http://user:s3cret@gw.example.com:823"),
            "http://***@gw.example.com:823"
        );
        // No credentials: unchanged.
        assert_eq!(
            redact_proxy_url("socks5h://gw.example.com:1080"),
            "socks5h://gw.example.com:1080"
        );
        // A typo'd scheme still parses out, which is the common `--proxy htp://`
        // case an operator needs to see.
        assert_eq!(redact_proxy_url("htp://user:pw@host"), "htp://***@host");
        // A value too mangled to split on `://` loses the prefix rather than
        // risking the credentials: masking wins over fidelity here.
        assert_eq!(redact_proxy_url("htp:/user:pw@host"), "***@host");
        // A "scheme" that is not one is credentials in disguise: masked too.
        assert_eq!(
            redact_proxy_url("user:s3cret://tail@host:8080"),
            "***@host:8080"
        );
        assert_eq!(redact_proxy_url(""), "");
        // An `@` in the path of a credential-free value is masked too: masking
        // wins over fidelity, and this documents that it is on purpose.
        assert_eq!(
            redact_proxy_url("http://gw.example.com:823/a@b"),
            "http://***@b"
        );
        // A literal `@` in the password does not fool the split.
        assert_eq!(
            redact_proxy_url("http://user:p@ss@host:8080"),
            "http://***@host:8080"
        );
    }

    #[test]
    fn parse_error_never_quotes_credentials() {
        let err = ProxyEntry::parse("ftp://user:s3cret@gw.example.com:823").unwrap_err();
        assert!(!err.contains("s3cret"), "credentials leaked: {err}");
        assert!(err.contains("***@gw.example.com:823"), "got {err}");
    }

    #[test]
    fn parse_http_with_auth() {
        let e = ProxyEntry::parse("http://user:pass@host.example:8080").unwrap();
        assert_eq!(e.scheme(), "http");
        assert_eq!(e.chrome_proxy_server(), "http://host.example:8080");
        assert_eq!(e.auth(), Some(&("user".to_string(), "pass".to_string())));
        assert_eq!(e.raw(), "http://user:pass@host.example:8080");
    }

    #[test]
    fn parse_socks5_no_auth() {
        let e = ProxyEntry::parse("socks5://1.2.3.4:1080").unwrap();
        assert_eq!(e.scheme(), "socks5");
        assert_eq!(e.chrome_proxy_server(), "socks5://1.2.3.4:1080");
        assert!(e.auth().is_none());
    }

    #[test]
    fn parse_percent_encoded_auth() {
        let e = ProxyEntry::parse("http://u%40b:p%3Aw@h:8080").unwrap();
        assert_eq!(e.auth(), Some(&("u@b".to_string(), "p:w".to_string())));
    }

    #[test]
    fn parse_rejects_bad_scheme() {
        assert!(ProxyEntry::parse("ftp://h:21").is_err());
        assert!(ProxyEntry::parse("not a url").is_err());
        assert!(ProxyEntry::parse("").is_err());
    }

    #[test]
    fn build_empty_is_none() {
        assert!(
            ProxyRotator::build(&[], None, ProxyRotation::RoundRobin)
                .unwrap()
                .is_none()
        );
        assert!(
            ProxyRotator::build(&[], Some("  "), ProxyRotation::RoundRobin)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn build_single_is_pool_of_one() {
        let r = ProxyRotator::build(&[], Some("http://h:8080"), ProxyRotation::RoundRobin)
            .unwrap()
            .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r.pick(None).chrome_proxy_server(), "http://h:8080");
    }

    #[test]
    fn build_list_wins_over_single() {
        let list = vec!["http://a:1".to_string(), "http://b:2".to_string()];
        let r = ProxyRotator::build(&list, Some("http://single:9"), ProxyRotation::RoundRobin)
            .unwrap()
            .unwrap();
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn build_bad_entry_is_hard_error() {
        let list = vec!["http://ok:1".to_string(), "ftp://bad:2".to_string()];
        assert!(ProxyRotator::build(&list, None, ProxyRotation::RoundRobin).is_err());
    }

    #[test]
    fn round_robin_cycles_in_order() {
        let list = vec![
            "http://a:1".to_string(),
            "http://b:2".to_string(),
            "http://c:3".to_string(),
        ];
        let r = ProxyRotator::build(&list, None, ProxyRotation::RoundRobin)
            .unwrap()
            .unwrap();
        let seq: Vec<&str> = (0..4).map(|_| r.pick(None).raw()).collect();
        assert_eq!(
            seq,
            vec!["http://a:1", "http://b:2", "http://c:3", "http://a:1"]
        );
    }

    #[test]
    fn random_stays_in_bounds() {
        let list = vec!["http://a:1".to_string(), "http://b:2".to_string()];
        let r = ProxyRotator::build(&list, None, ProxyRotation::Random)
            .unwrap()
            .unwrap();
        for _ in 0..100 {
            let raw = r.pick(None).raw();
            assert!(raw == "http://a:1" || raw == "http://b:2");
        }
    }

    #[test]
    fn sticky_pins_host_to_one_proxy() {
        let list = vec![
            "http://a:1".to_string(),
            "http://b:2".to_string(),
            "http://c:3".to_string(),
        ];
        let r = ProxyRotator::build(&list, None, ProxyRotation::StickyPerHost)
            .unwrap()
            .unwrap();
        let first = r.pick(Some("example.com")).raw().to_string();
        for _ in 0..50 {
            assert_eq!(r.pick(Some("example.com")).raw(), first);
        }
        // A different host may land on a different proxy, but is itself stable.
        let other = r.pick(Some("other.com")).raw().to_string();
        for _ in 0..50 {
            assert_eq!(r.pick(Some("other.com")).raw(), other);
        }
    }

    #[test]
    fn default_strategy_is_sticky() {
        assert_eq!(ProxyRotation::default(), ProxyRotation::StickyPerHost);
    }

    #[test]
    fn socks5h_maps_to_socks5_for_chrome() {
        let e = ProxyEntry::parse("socks5h://host:1080").unwrap();
        assert_eq!(e.scheme(), "socks5h"); // reqwest/raw keeps original
        assert_eq!(e.chrome_proxy_server(), "socks5://host:1080"); // chrome normalized
    }

    #[test]
    fn socks_with_auth_unsupported_on_cdp() {
        let e = ProxyEntry::parse("socks5://user:pass@host:1080").unwrap();
        assert!(!e.supports_cdp_auth());
        let e2 = ProxyEntry::parse("socks5h://user:pass@host:1080").unwrap();
        assert!(!e2.supports_cdp_auth());
        // No-auth SOCKS and HTTP(+auth) are fine for CDP.
        assert!(
            ProxyEntry::parse("socks5://host:1080")
                .unwrap()
                .supports_cdp_auth()
        );
        assert!(
            ProxyEntry::parse("http://user:pass@host:8080")
                .unwrap()
                .supports_cdp_auth()
        );
    }

    #[test]
    fn sticky_is_stateless_and_deterministic() {
        // Two independent rotators with the same pool map a host identically
        // (proves stickiness is a pure hash, not per-instance state).
        let list = vec![
            "http://a:1".to_string(),
            "http://b:2".to_string(),
            "http://c:3".to_string(),
        ];
        let r1 = ProxyRotator::build(&list, None, ProxyRotation::StickyPerHost)
            .unwrap()
            .unwrap();
        let r2 = ProxyRotator::build(&list, None, ProxyRotation::StickyPerHost)
            .unwrap()
            .unwrap();
        assert_eq!(
            r1.pick(Some("example.com")).raw(),
            r2.pick(Some("example.com")).raw()
        );
        // Repeated picks never advance the round-robin cursor (idempotent).
        let first = r1.pick(Some("example.com")).raw().to_string();
        for _ in 0..10 {
            assert_eq!(r1.pick(Some("example.com")).raw(), first);
        }
    }

    #[test]
    fn round_robin_advances_exactly_once_per_pick() {
        let list = vec!["http://a:1".to_string(), "http://b:2".to_string()];
        let r = ProxyRotator::build(&list, None, ProxyRotation::RoundRobin)
            .unwrap()
            .unwrap();
        // Each pick advances by exactly one step (a→b→a→b).
        assert_eq!(r.pick_index(None), 0);
        assert_eq!(r.pick_index(None), 1);
        assert_eq!(r.pick_index(None), 0);
        assert_eq!(r.pick_index(None), 1);
    }
}
