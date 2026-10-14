//! Operator-facing diagnostics helpers (issue #90).
//!
//! `search_backend_url` is operator-set and can carry secrets (`https://user:pass@host`,
//! a `?token=…`, or a token embedded in the path of a reverse-proxy URL). Anything
//! we log or return in an error must be sanitized to the bare origin first.

use crw_core::config::SearchConfig;
use tracing::Level;

/// Reduce a URL to its **origin** — `scheme://host[:port]` — dropping userinfo,
/// path, query, and fragment. This is the only form safe to log or echo in an
/// error, because every other component can carry a secret. Falls back to a
/// fixed redaction string if the URL doesn't parse.
pub fn sanitize_url_origin(raw: &str) -> String {
    crw_core::log_url::origin_only(raw)
}

/// One-line summary of the search subsystem's configured state, for the startup
/// log. Distinguishes the three states that otherwise collapse to a single
/// "search disabled" at request time:
///   - `enabled = false`            → intentionally off
///   - enabled, `search_backend_url` unset → misconfigured (every call will 503)
///   - enabled, `search_backend_url` set   → active (host shown, origin-sanitized)
pub fn search_startup_status(cfg: &SearchConfig) -> (Level, String) {
    if !cfg.enabled {
        (
            Level::INFO,
            "search: disabled ([search].enabled = false)".to_string(),
        )
    } else if let Some(url) = cfg.resolve_backend_url() {
        (
            Level::INFO,
            format!("search: enabled (searxng={})", sanitize_url_origin(url)),
        )
    } else {
        (
            Level::WARN,
            "search: enabled but no [search].searxng_url — /v1/search will return 503".to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_userinfo_path_query_fragment() {
        assert_eq!(
            sanitize_url_origin("https://user:pass@host:9000/searxng/tok123?q=x#frag"),
            "https://host:9000"
        );
        assert_eq!(
            sanitize_url_origin("http://searxng:8080"),
            "http://searxng:8080"
        );
        assert_eq!(
            sanitize_url_origin("http://searxng:8080/"),
            "http://searxng:8080"
        );
        // Default port is preserved as written only if explicit; no port → no port.
        assert_eq!(
            sanitize_url_origin("https://example.com"),
            "https://example.com"
        );
    }

    #[test]
    fn sanitize_redacts_unparseable() {
        assert_eq!(sanitize_url_origin("not a url"), "<redacted-url>");
    }

    fn cfg(enabled: bool, url: Option<&str>) -> SearchConfig {
        let toml = match url {
            Some(u) => format!("enabled = {enabled}\nsearxng_url = \"{u}\"\n"),
            None => format!("enabled = {enabled}\n"),
        };
        toml::from_str(&toml).expect("valid SearchConfig")
    }

    #[test]
    fn startup_status_enabled_with_url() {
        let (level, msg) = search_startup_status(&cfg(true, Some("http://searxng:8080")));
        assert_eq!(level, Level::INFO);
        assert!(
            msg.contains("enabled (searxng=http://searxng:8080)"),
            "{msg}"
        );
    }

    #[test]
    fn startup_status_enabled_no_url_warns() {
        let (level, msg) = search_startup_status(&cfg(true, None));
        assert_eq!(level, Level::WARN);
        assert!(msg.contains("no [search].searxng_url"), "{msg}");
    }

    #[test]
    fn startup_status_disabled() {
        let (level, msg) = search_startup_status(&cfg(false, Some("http://searxng:8080")));
        assert_eq!(level, Level::INFO);
        assert!(msg.contains("disabled"), "{msg}");
    }

    #[test]
    fn startup_status_never_leaks_credentials() {
        let (_, msg) = search_startup_status(&cfg(true, Some("https://u:secret@host:8080/tok")));
        assert!(!msg.contains("secret"), "{msg}");
        assert!(!msg.contains("tok"), "{msg}");
        assert!(msg.contains("https://host:8080"), "{msg}");
    }
}
