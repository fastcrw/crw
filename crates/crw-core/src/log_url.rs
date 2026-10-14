//! Rendering of target URLs for log lines.
//!
//! A customer's URL can carry secrets in its userinfo, path, query or fragment
//! (signed links, magic links, share ids, emails), and accounts on a zero
//! retention plan are promised their URLs are not stored. Engine logs are
//! shipped to an external store, so a log line never prints a target URL
//! verbatim. Every site that logs one goes through [`LogUrl`]; free text that may
//! quote one (an error chain) goes through [`LogErr`].
//!
//! Output is the origin plus a short stable hash of the full URL:
//! `https://example.com (u:3f9a1c02e4b7)`. The hash keeps the lines of one URL
//! correlatable, and joinable offline against a URL held elsewhere, without
//! revealing the path or query. The hash is HMAC-SHA256 (first 48 bits) under a
//! random key generated once per process and never stored or logged, so a
//! low-entropy value in a URL (`?code=482913`) cannot be confirmed offline by
//! anyone holding only the logs. The price: tags correlate within one engine
//! process lifetime and change on restart, which is what incident debugging
//! needs. The policy lives in this file only.

use std::borrow::Cow;
use std::fmt::{self, Display, Write};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use url::Url;

/// Printed when a string is not a URL this module is willing to echo.
pub const REDACTED: &str = "<redacted-url>";

/// Schemes echoed with a host.
const HOST_SCHEMES: [&str; 4] = ["http", "https", "ws", "wss"];
/// Schemes echoed as `scheme:<opaque>` (no host to show, body never printed).
const OPAQUE_SCHEMES: [&str; 6] = ["data", "mailto", "file", "blob", "about", "javascript"];
/// Trailing punctuation kept next to a replaced URL in free text, at most this many.
const KEEP_TRAILING: usize = 4;

/// A target URL rendered for a log line: `scheme://host[:port] (u:<hash>)`.
///
/// Userinfo, path, query and fragment are never written. Only known schemes
/// are echoed; anything else, and anything that does not parse, prints
/// [`REDACTED`], so `admin:s3cret@host` (a "scheme" of `admin`) cannot leak.
/// The hash covers the parsed (serialised) form of the URL, so the same URL gets
/// the same tag in a `url` field and when quoted inside an error text.
pub struct LogUrl<'a, T: AsRef<str> + ?Sized>(pub &'a T);

impl<T: AsRef<str> + ?Sized> Display for LogUrl<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw = self.0.as_ref();
        let Ok(url) = Url::parse(raw.trim()) else {
            return f.write_str(REDACTED);
        };
        let scheme = url.scheme();
        if HOST_SCHEMES.contains(&scheme) {
            match host_and_port(&url) {
                Some(origin) => write!(f, "{scheme}://{origin} (u:{})", short_hash(url.as_str())),
                None => f.write_str(REDACTED),
            }
        } else if OPAQUE_SCHEMES.contains(&scheme) {
            write!(f, "{scheme}:<opaque> (u:{})", short_hash(url.as_str()))
        } else {
            f.write_str(REDACTED)
        }
    }
}

/// Free text (an error chain) with every URL in it replaced by its [`LogUrl`]
/// form. The scan mirrors `proxy::redact_url_userinfo`: a URL runs from its
/// scheme to the next whitespace, deliberately ignoring quotes, brackets and
/// `)`, which are legal in an unencoded password. A short run of trailing
/// punctuation (`)`, `,`, `.` ...) is kept after the replacement so the
/// sentence still reads. Text without `://` is unchanged.
pub struct LogErr<'a, E: Display + ?Sized>(pub &'a E);

impl<E: Display + ?Sized> Display for LogErr<'_, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&scrub_urls(&self.0.to_string()))
    }
}

/// The text behind [`LogErr`].
pub fn scrub_urls(text: &str) -> String {
    let is_scheme_byte = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.');
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(sep) = rest.find("://") {
        // Scheme bytes are ASCII, so stopping on any other byte always lands on
        // a char boundary.
        let start = rest.as_bytes()[..sep]
            .iter()
            .rposition(|&b| !is_scheme_byte(b))
            .map_or(0, |i| i + 1);
        let end = rest[sep..]
            .find(char::is_whitespace)
            .map_or(rest.len(), |i| sep + i);
        let token = &rest[start..end];
        let (core, tail) = split_trailing_punctuation(token);
        out.push_str(&rest[..start]);
        let _ = write!(out, "{}", LogUrl(core));
        out.push_str(tail);
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// `scheme://host[:port]` with no hash and any scheme that has a host, or
/// [`REDACTED`]. The form for config-derived origins (the search backend), where
/// there is no customer URL to correlate.
pub fn origin_only(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(url) => match host_and_port(&url) {
            Some(origin) => format!("{}://{origin}", url.scheme()),
            None => REDACTED.to_string(),
        },
        Err(_) => REDACTED.to_string(),
    }
}

/// `host` or `host:port` (non-default port only). IPv6 hosts keep their brackets.
fn host_and_port(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// First 48 bits of HMAC-SHA256 of `text` under the per-process key, as 12 hex digits.
fn short_hash(text: &str) -> String {
    tag_with_key(process_key(), text)
}

fn tag_with_key(key: &[u8], text: &str) -> String {
    let mac = hmac_sha256(key, text.as_bytes());
    let mut out = String::with_capacity(12);
    for byte in &mac[..6] {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// 32 random bytes, drawn once per process. No env var, no config, never logged.
fn process_key() -> &'static [u8; 32] {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    KEY.get_or_init(rand::random::<[u8; 32]>)
}

/// HMAC-SHA256 (RFC 2104). Written out because it is twelve lines and keeps a
/// dependency off the engine; checked against the RFC 4231 vectors in the tests.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    if key.len() > BLOCK {
        padded[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        padded[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(padded.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(padded.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// A URL for an error text: unchanged unless it holds whitespace, in which case
/// it is the parsed serialisation (spaces become `%20`), or, when it does not
/// parse, the string with each whitespace character percent-encoded. Error
/// texts are later scrubbed by [`LogErr`], which ends a URL at whitespace, so a
/// raw space would leave the tail of the URL (and any token in it) in the
/// clear. What is fetched is never changed; only the text built from it.
pub fn url_for_error(raw: &str) -> Cow<'_, str> {
    if !raw.contains(char::is_whitespace) {
        return Cow::Borrowed(raw);
    }
    match Url::parse(raw.trim()) {
        Ok(url) => Cow::Owned(url.into()),
        Err(_) => Cow::Owned(
            raw.chars()
                .map(|c| {
                    if c.is_whitespace() {
                        c.to_string().bytes().map(|b| format!("%{b:02X}")).collect()
                    } else {
                        c.to_string()
                    }
                })
                .collect::<String>(),
        ),
    }
}

fn split_trailing_punctuation(token: &str) -> (&str, &str) {
    let mut cut = token.len();
    for (kept, (i, c)) in token.char_indices().rev().enumerate() {
        if kept >= KEEP_TRAILING
            || !matches!(
                c,
                ')' | ']' | '}' | '>' | ',' | ';' | '.' | ':' | '"' | '\''
            )
        {
            break;
        }
        cut = i;
    }
    token.split_at(cut)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(raw: &str) -> String {
        LogUrl(raw).to_string()
    }

    /// Needles are named by position only: printing one would put the value
    /// under test into the test output.
    fn assert_none_of(text: &str, needles: &[&str]) {
        for (i, needle) in needles.iter().enumerate() {
            assert!(!text.contains(needle), "needle #{i} leaked into: {text}");
        }
    }

    #[test]
    fn origin_plus_hash_drops_userinfo_path_query_and_fragment() {
        let out =
            render("https://user:pw-9x@Example.COM/reset/tok-4471?token=t0k3n&e=a@b.c#frag-77");
        let (origin, tag) = out.split_once(" (u:").expect("hash suffix");
        assert_eq!(origin, "https://example.com");
        assert_eq!(
            tag.len(),
            12 + 1,
            "12 hex digits and the closing paren: {out}"
        );
        assert!(tag.ends_with(')') && tag[..12].bytes().all(|b| b.is_ascii_hexdigit()));
        assert_none_of(
            &out,
            &[
                "user", "pw-9x", "reset", "tok-4471", "t0k3n", "a@b.c", "frag-77",
            ],
        );
    }

    #[test]
    fn hash_is_stable_within_a_process_and_distinguishes_full_urls() {
        let a = render("https://h.example/p?x=1");
        assert_eq!(a, render("https://h.example/p?x=1"));
        assert_ne!(a, render("https://h.example/p?x=2"));
        assert_ne!(a, render("https://h.example/q?x=1"));
        // The tag covers the parsed form: a field and an error text agree.
        assert_eq!(render("https://h.example"), render("https://h.example/"));
        assert_eq!(
            render("https://h.example/a b?x=1"),
            render("https://h.example/a%20b?x=1")
        );
    }

    #[test]
    fn tag_depends_on_the_key_and_is_not_a_plain_sha256() {
        let text = "https://h.example/p?code=482913";
        let one = tag_with_key(&[1u8; 32], text);
        assert_eq!(one, tag_with_key(&[1u8; 32], text));
        assert_ne!(one, tag_with_key(&[2u8; 32], text));
        let plain = hex_prefix(&Sha256::digest(text.as_bytes())[..6]);
        assert_ne!(one, plain, "an unkeyed digest would be confirmable offline");
        assert_eq!(one.len(), 12);
    }

    fn hex_prefix(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn hmac_hex(key: &[u8], msg: &[u8]) -> String {
        hex_prefix(&hmac_sha256(key, msg))
    }

    /// RFC 4231 test cases 1, 2 and 6 (the last one has a key longer than a block).
    #[test]
    fn hmac_matches_rfc_4231_vectors() {
        assert_eq!(
            hmac_hex(&[0x0b; 20], b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hmac_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hmac_hex(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn url_for_error_never_returns_whitespace_and_leaves_clean_urls_alone() {
        let clean = "https://h.example";
        assert!(matches!(url_for_error(clean), Cow::Borrowed(_)));
        assert_eq!(url_for_error(clean), clean);
        let spaced = url_for_error("https://h.example/foo bar&token=s3cret\tx");
        assert!(!spaced.contains(char::is_whitespace), "{spaced}");
        assert!(spaced.contains("foo%20bar&token="), "{spaced}");
        // Unparsable with a space: still no whitespace.
        let broken = url_for_error("not a url ?token=s3cret");
        assert!(!broken.contains(char::is_whitespace), "{broken}");
        assert_eq!(broken, "not%20a%20url%20?token=s3cret");
        // The point: the scrub sees the whole URL, so no tail survives.
        let text = format!(
            "{}: refused",
            url_for_error("https://h.example/foo bar&token=s3cret")
        );
        let out = scrub_urls(&text);
        assert!(
            out.starts_with("https://h.example (u:") && out.ends_with("): refused"),
            "{out}"
        );
        assert_none_of(&out, &["bar", "s3cret"]);
    }

    #[test]
    fn port_and_ipv6_hosts() {
        assert!(render("http://h.example:8080/a?b").starts_with("http://h.example:8080 (u:"));
        // The default port is normalised away by the parser.
        assert!(render("https://h.example:443/x").starts_with("https://h.example (u:"));
        assert!(render("http://[::1]:8080/a?b").starts_with("http://[::1]:8080 (u:"));
        assert!(render("http://[2001:db8::7]/a").starts_with("http://[2001:db8::7] (u:"));
    }

    #[test]
    fn idn_hosts_are_punycoded_and_whitespace_is_trimmed() {
        assert!(
            render("  https://b\u{fc}cher.example/x \n")
                .starts_with("https://xn--bcher-kva.example (u:")
        );
    }

    #[test]
    fn websocket_schemes_keep_their_host() {
        assert!(render("wss://h.example/s?k=1").starts_with("wss://h.example (u:"));
        assert!(render("ws://h.example:9222/devtools").starts_with("ws://h.example:9222 (u:"));
    }

    #[test]
    fn opaque_schemes_never_print_their_body() {
        for (raw, prefix) in [
            ("mailto:a@b.example?subject=s3cret", "mailto:<opaque> (u:"),
            ("data:text/plain;base64,QUFBQQ==", "data:<opaque> (u:"),
            ("javascript:alert(1)", "javascript:<opaque> (u:"),
            ("blob:https://h.example/0b1c", "blob:<opaque> (u:"),
            ("about:blank", "about:<opaque> (u:"),
            ("file:///etc/secret-file", "file:<opaque> (u:"),
        ] {
            let out = render(raw);
            assert!(out.starts_with(prefix), "{raw} rendered as {out}");
            assert_none_of(&out, &["s3cret", "QUFBQQ", "alert", "0b1c", "secret-file"]);
        }
    }

    #[test]
    fn unknown_schemes_and_unparsable_input_print_the_sentinel() {
        for raw in [
            "",
            "   ",
            "not a url?token=t0k3n",
            "example.com/p?token=t0k3n",
            "/relative/path?token=t0k3n",
            "https://",
            "ftp://u:pw@h.example/x?y=t0k3n",
            // Parses as scheme `admin` with an opaque path: must not echo `admin`.
            "admin:s3cret@host.example",
            "gopher://h.example/",
        ] {
            let out = render(raw);
            assert_eq!(out, REDACTED, "input #{}", raw.len());
        }
    }

    #[test]
    fn accepts_string_str_and_url_arguments() {
        let s = String::from("https://h.example/p");
        let u = Url::parse("https://h.example/p").unwrap();
        assert_eq!(
            LogUrl(&s).to_string(),
            LogUrl("https://h.example/p").to_string()
        );
        assert_eq!(LogUrl(&u).to_string(), LogUrl(s.as_str()).to_string());
    }

    #[test]
    fn origin_only_keeps_its_previous_contract() {
        assert_eq!(
            origin_only("https://user:pass@host:9000/searxng/tok123?q=x#frag"),
            "https://host:9000"
        );
        assert_eq!(origin_only("http://searxng:8080/"), "http://searxng:8080");
        assert_eq!(origin_only("ftp://h.example/x"), "ftp://h.example");
        assert_eq!(origin_only("not a url"), REDACTED);
        assert_eq!(origin_only("mailto:a@b.example"), REDACTED);
    }

    #[test]
    fn scrub_reduces_a_reqwest_style_message() {
        let msg = "error sending request for url (https://a.example/p?token=t0k3n)";
        let out = scrub_urls(msg);
        assert!(
            out.starts_with("error sending request for url (https://a.example (u:")
                && out.ends_with("))"),
            "{out}"
        );
        assert_none_of(&out, &["/p", "t0k3n"]);
        // The hash matches the one a LogUrl field prints for the same URL.
        let url = "https://a.example/p?token=t0k3n";
        assert!(out.contains(&LogUrl(url).to_string()), "{out}");
    }

    #[test]
    fn scrub_handles_several_urls_and_trailing_punctuation() {
        let out = scrub_urls(
            "Could not reach https://one.example/a?k=s1: refused; via http://u:pw@two.example:81/b, then done.",
        );
        assert!(
            out.starts_with("Could not reach https://one.example (u:"),
            "{out}"
        );
        assert!(out.contains("): refused; via "), "{out}");
        assert!(out.contains("via http://two.example:81 (u:"), "{out}");
        assert!(out.ends_with(", then done."), "{out}");
        assert_none_of(&out, &["/a", "s1", "pw", "/b", "u:pw"]);
    }

    #[test]
    fn scrub_drops_passwords_holding_closers_quotes_and_angle_brackets() {
        for host_part in [
            "u:p)w@h1:1080",
            "u:p\"w@h1:1080",
            "u:p<w>x@h1:1080",
            "u:p'w]}@h1:1080",
        ] {
            let text = format!("proxy http://{host_part}/path?k=v refused");
            let out = scrub_urls(&text);
            assert!(out.starts_with("proxy http://h1:1080 (u:"), "{out}");
            assert!(out.ends_with(") refused"), "{out}");
            assert_none_of(&out, &["u:p", "w@", "w>", "path", "k=v"]);
        }
    }

    #[test]
    fn scrub_leaves_plain_text_and_multibyte_text_alone() {
        assert_eq!(
            scrub_urls("tunnel error: unsuccessful"),
            "tunnel error: unsuccessful"
        );
        assert_eq!(scrub_urls(""), "");
        let out = scrub_urls("\u{e7}a\u{11f}r\u{131} https://h.example/x bitti");
        assert!(
            out.starts_with("\u{e7}a\u{11f}r\u{131} https://h.example (u:"),
            "{out}"
        );
        assert!(out.ends_with(" bitti"), "{out}");
    }

    #[test]
    fn scrub_sentinels_unknown_schemes_and_broken_tokens() {
        assert_eq!(
            scrub_urls("fetch ftp://u:pw@h.example/x failed"),
            format!("fetch {REDACTED} failed")
        );
        assert_eq!(scrub_urls("bad ://u:pw@h"), format!("bad {REDACTED}"));
        assert_eq!(
            scrub_urls("got https://) end"),
            format!("got {REDACTED}) end")
        );
    }

    #[test]
    fn log_err_wraps_any_display() {
        let e = crate::CrwError::HttpError("https://h.example/p?token=t0k3n: reset".into());
        let out = LogErr(&e).to_string();
        assert!(
            out.starts_with("HTTP request failed: https://h.example (u:"),
            "{out}"
        );
        assert!(out.ends_with("): reset"), "{out}");
        assert_none_of(&out, &["/p", "t0k3n"]);
    }

    /// A real reqwest error carries `for url (<full url>)`. Loopback port 1 is
    /// privileged, so nothing listens: the connect is refused at once and
    /// nothing leaves the machine.
    #[tokio::test]
    async fn log_err_scrubs_a_real_reqwest_error() {
        let err = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get("http://127.0.0.1:1/customer-path?token=t0k3n")
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("customer-path"),
            "precondition: reqwest still appends the URL, got {err}"
        );
        let out = LogErr(&err).to_string();
        assert!(
            out.starts_with("error sending request for url (http://127.0.0.1:1 (u:"),
            "{out}"
        );
        assert_none_of(&out, &["customer-path", "t0k3n"]);
    }
}
