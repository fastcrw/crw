//! Outbound URL for a connector call: a 1:1 port of crw-saas
//! `src/lib/integrations/upstream-url.ts`.
//!
//! The manifest supplies the base and the path template; the agent supplies the
//! values. Every value is encoded before it reaches the path, only allowlisted
//! query params are forwarded, and the finished URL's origin must equal the
//! base's. `tests/fixtures/connectors-url-vectors.json` holds the vectors both
//! builders must agree on; crw-saas `scripts/gen-url-vectors.ts` generates it,
//! so a rule change lands there and the regenerated copy comes here.
//!
//! WHATWG details the port has to reproduce: `encodeURIComponent` leaves
//! `!'()*` alone, value length counts UTF-16 units, `String.prototype.trim`
//! strips U+FEFF but not U+0085, and `searchParams.set` replaces the first
//! match in place and re-serialises the whole query as form-urlencoded.

use std::collections::BTreeMap;
use url::Url;

/// Caller values are identifiers, not documents.
const MAX_VALUE_LENGTH: usize = 200;

pub(crate) const MISCONFIGURED: &str = "This integration is misconfigured";

/// `{name}` placeholders in template order, de-duplicated. Mirrors the TS
/// regex `/\{([A-Za-z][A-Za-z0-9_.-]*)\}/g`.
pub fn template_params(path_template: &str) -> Vec<String> {
    let bytes = path_template.as_bytes();
    let mut names: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' && bytes.get(i + 1).is_some_and(u8::is_ascii_alphabetic) {
            let start = i + 1;
            let mut end = start + 1;
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'_' | b'.' | b'-'))
            {
                end += 1;
            }
            if bytes.get(end) == Some(&b'}') {
                let name = &path_template[start..end];
                if !names.iter().any(|n| n == name) {
                    names.push(name.to_string());
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
    names
}

/// ECMAScript `WhiteSpace` + `LineTerminator`, the set `trim()` strips.
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}' | '\u{a}' | '\u{b}' | '\u{c}' | '\u{d}' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

fn too_long(s: &str) -> bool {
    s.encode_utf16().count() > MAX_VALUE_LENGTH
}

/// `encodeURIComponent`: everything but `A-Za-z0-9-_.!~*'()` as UTF-8 `%XX`.
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn join_path(base_path: &str, path: &str) -> String {
    let left = base_path.trim_end_matches('/');
    if path.starts_with('/') {
        format!("{left}{path}")
    } else {
        format!("{left}/{path}")
    }
}

/// Build the upstream URL, or say why not. Error strings match the TS builder.
pub fn build_url(
    base_url: &str,
    path_template: &str,
    forward_params: &[String],
    params: &BTreeMap<String, String>,
) -> Result<Url, String> {
    build_url_with(base_url, path_template, forward_params, params, false)
}

/// [`build_url`], with plain http allowed when `test_http` is set (only the
/// test guard sets it, for a loopback test server).
pub(crate) fn build_url_with(
    base_url: &str,
    path_template: &str,
    forward_params: &[String],
    params: &BTreeMap<String, String>,
    test_http: bool,
) -> Result<Url, String> {
    let base = Url::parse(base_url).map_err(|_| MISCONFIGURED.to_string())?;
    if base.scheme() != "https" && !(test_http && base.scheme() == "http") {
        return Err(MISCONFIGURED.into());
    }

    let (mut path, mut fixed_query) = match path_template.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (path_template.to_string(), String::new()),
    };
    for name in template_params(path_template) {
        let raw = js_trim(params.get(&name).map(String::as_str).unwrap_or(""));
        if raw.is_empty() {
            return Err(format!("Missing required parameter \"{name}\""));
        }
        if too_long(raw) {
            return Err(format!("Parameter \"{name}\" is too long"));
        }
        // "." and ".." survive encoding and are the only values that move the
        // request up the path.
        if raw == "." || raw == ".." || raw.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}') {
            return Err(format!("Parameter \"{name}\" is not valid"));
        }
        let placeholder = format!("{{{name}}}");
        let encoded = encode_uri_component(raw);
        path = path.replace(&placeholder, &encoded);
        fixed_query = fixed_query.replace(&placeholder, &encoded);
    }

    let mut url = base.clone();
    url.set_path(&join_path(base.path(), &path));
    // The `search` setter: one leading "?" dropped, empty means no query.
    let fixed_query = fixed_query.strip_prefix('?').unwrap_or(&fixed_query);
    url.set_query((!fixed_query.is_empty()).then_some(fixed_query));

    // A name the template pinned is not forwardable: the pins are what hold
    // the response to JSON (`format=json`), so the caller cannot replace them.
    let pinned: Vec<String> = url.query_pairs().map(|(k, _)| k.into_owned()).collect();

    for name in forward_params
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty() && !pinned.iter().any(|k| k == p))
    {
        let Some(raw) = params.get(name) else {
            continue;
        };
        let value = js_trim(raw);
        if value.is_empty() {
            continue;
        }
        if too_long(value) {
            return Err(format!("Parameter \"{name}\" is too long"));
        }
        search_params_set(&mut url, name, value);
    }

    // Last line of defence: nothing above may have moved the host.
    if url.origin() != base.origin() {
        return Err(MISCONFIGURED.into());
    }
    Ok(url)
}

/// `url.searchParams.set(name, value)`: replace the first pair named `name`,
/// drop the rest, append if absent, then re-serialise every pair.
pub(crate) fn search_params_set(url: &mut Url, name: &str, value: &str) {
    let mut pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    match pairs.iter().position(|(k, _)| k == name) {
        Some(first) => {
            pairs[first].1 = value.to_string();
            let mut i = 0;
            pairs.retain(|(k, _)| {
                let keep = k != name || i == first;
                i += 1;
                keep
            });
        }
        None => pairs.push((name.to_string(), value.to_string())),
    }
    url.query_pairs_mut().clear().extend_pairs(pairs);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Fixture {
        fixture_version: u32,
        vectors: Vec<Vector>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Vector {
        name: String,
        base_url: String,
        path_template: String,
        base_url_override: Option<String>,
        forward_params: Vec<String>,
        params: BTreeMap<String, String>,
        expect: Expect,
    }

    #[derive(Deserialize)]
    struct Expect {
        ok: bool,
        url: Option<String>,
    }

    #[test]
    fn connectors_url_parity_vectors() {
        let fixture: Fixture = serde_json::from_str(include_str!(
            "../../tests/fixtures/connectors-url-vectors.json"
        ))
        .expect("fixture parses");
        assert_eq!(fixture.fixture_version, 1);
        assert!(!fixture.vectors.is_empty());
        for v in &fixture.vectors {
            let base = v.base_url_override.as_deref().unwrap_or(&v.base_url);
            let got = build_url(base, &v.path_template, &v.forward_params, &v.params);
            match (&got, v.expect.ok) {
                (Ok(url), true) => assert_eq!(
                    Some(url.as_str()),
                    v.expect.url.as_deref(),
                    "vector {:?}",
                    v.name
                ),
                (Err(_), false) => {}
                _ => panic!(
                    "vector {:?}: expected ok={}, got {got:?}",
                    v.name, v.expect.ok
                ),
            }
        }
    }

    #[test]
    fn connectors_template_params_skip_non_identifiers() {
        assert_eq!(
            template_params("/{{id}/{1x}/{a_b}/{a_b}"),
            vec!["id", "a_b"]
        );
    }

    #[test]
    fn connectors_js_trim_matches_ecmascript() {
        assert_eq!(js_trim("\u{feff} x \u{2028}"), "x");
        assert_eq!(js_trim("\u{85}x"), "\u{85}x");
    }

    #[test]
    fn connectors_length_counts_utf16_units() {
        // 100 emoji = 200 UTF-16 units (ok); 101 = 202 (too long).
        assert!(!too_long(&"\u{1f3b8}".repeat(100)));
        assert!(too_long(&"\u{1f3b8}".repeat(101)));
    }
}
