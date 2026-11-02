//! Run one connector capability on this machine. Success rules match crw-saas
//! `src/lib/integrations/execute.ts`: status < 400, a JSON content type, a
//! body within the cap that parses to something other than `null`. An
//! upstream's own error body is never passed through.

use super::client::{ConnectorHttp, FetchError, read_capped};
use super::keys::{self, ConnectorEntry};
use super::manifest::Connector;
use super::url::{MISCONFIGURED, build_url_with, encode_uri_component, search_params_set};
use base64::Engine;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(20);
/// A 429's `Retry-After` is honoured once, and only when this short.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(10);

/// What happened to a call that reached the network, for usage telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Success,
    UpstreamError,
    RateLimited,
    Timeout,
    InvalidResponse,
    Blocked,
}

impl Outcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::UpstreamError => "upstream_error",
            Outcome::RateLimited => "rate_limited",
            Outcome::Timeout => "timeout",
            Outcome::InvalidResponse => "invalid_response",
            Outcome::Blocked => "blocked",
        }
    }
}

/// Per-provider pacing: one call at a time, `minIntervalMs` between sends.
/// Per process; several agents on one machine can still exceed a per-IP limit.
#[derive(Default)]
pub(crate) struct Pacer {
    slots: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Instant>>>>>,
}

impl Pacer {
    async fn lock(&self, provider: &str) -> tokio::sync::OwnedMutexGuard<Option<Instant>> {
        let slot = self
            .slots
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(provider.to_string())
            .or_default()
            .clone();
        slot.lock_owned().await
    }
}

async fn wait_turn(last: &mut Option<Instant>, min_interval: Duration) {
    if let Some(at) = *last {
        tokio::time::sleep_until((at + min_interval).into()).await;
    }
    *last = Some(Instant::now());
}

/// A header key written as a flat JSON object of non-empty strings, sent as one
/// header per pair: the FCA Register wants `X-Auth-Email` beside `X-Auth-Key`.
/// Same rule as the cloud's `parseHeaderSet`; anything else is one header value.
pub(crate) fn header_set(key: &str) -> Option<Vec<(String, String)>> {
    let text = key.trim();
    if !text.starts_with('{') {
        return None;
    }
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(text) else {
        return None;
    };
    let mut out = Vec::with_capacity(map.len());
    for (k, v) in map {
        let (k, Value::String(v)) = (k.trim().to_string(), v) else {
            return None;
        };
        if k.is_empty() || v.is_empty() {
            return None;
        }
        out.push((k, v));
    }
    (!out.is_empty()).then_some(out)
}

fn key_invalid(connector: &Connector) -> String {
    format!(
        "[connector_key_invalid] The stored key for {} contains characters a header cannot carry. {}",
        connector.name,
        keys::set_key_hint(&connector.provider)
    )
}

/// Replaces a key in every form it can come back in: raw, `encodeURIComponent`,
/// form-urlencoded and HTTP Basic `base64(key:)`. Matching ignores ASCII case,
/// so `%2f` and `%2F` both count; over-redacting a look-alike costs nothing.
///
/// JSON bodies are redacted after the parse, string by string and key by key
/// ([`Redactor::apply_json`]): JSON escapes (`\/`, `\u0061`) can spell a key
/// that no text pass over the raw body would see.
pub(crate) struct Redactor {
    forms: Vec<String>,
}

impl Redactor {
    pub(crate) fn new(key: Option<&str>) -> Self {
        let mut forms: Vec<String> = Vec::new();
        let key = key.filter(|k| !k.is_empty());
        // A header set is sent value by value, so each value is a secret too.
        let set: Vec<String> = key
            .and_then(header_set)
            .map(|s| s.into_iter().map(|(_, v)| v).collect())
            .unwrap_or_default();
        for key in key.into_iter().chain(set.iter().map(String::as_str)) {
            for form in [
                key.to_string(),
                encode_uri_component(key),
                url::form_urlencoded::byte_serialize(key.as_bytes()).collect(),
                base64::engine::general_purpose::STANDARD.encode(format!("{key}:")),
            ] {
                let form = form.to_ascii_lowercase();
                if !form.is_empty() && !forms.contains(&form) {
                    forms.push(form);
                }
            }
            // Longest first, so a form that contains another is replaced whole.
            forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
        }
        Self { forms }
    }

    pub(crate) fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for form in &self.forms {
            // ASCII lowercasing keeps byte offsets, so matches found in the
            // folded copy are valid ranges of the original.
            let mut result = String::with_capacity(out.len());
            let folded = out.to_ascii_lowercase();
            let mut last = 0;
            for (at, _) in folded.match_indices(form.as_str()) {
                result.push_str(&out[last..at]);
                result.push_str("REDACTED");
                last = at + form.len();
            }
            result.push_str(&out[last..]);
            out = result;
        }
        out
    }

    /// Redact every decoded string and object key in a parsed JSON value.
    pub(crate) fn apply_json(&self, value: Value) -> Value {
        if self.forms.is_empty() {
            return value;
        }
        match value {
            Value::String(s) => Value::String(self.apply(&s)),
            Value::Array(items) => {
                Value::Array(items.into_iter().map(|v| self.apply_json(v)).collect())
            }
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(k, v)| (self.apply(&k), self.apply_json(v)))
                    .collect(),
            ),
            // An all-digit key can come back as a JSON number.
            Value::Number(n) => {
                let text = n.to_string();
                let redacted = self.apply(&text);
                if redacted == text {
                    Value::Number(n)
                } else {
                    Value::String(redacted)
                }
            }
            other => other,
        }
    }
}

/// JSON `options` as query values, the same way `paramsFrom` in execute.ts
/// does: strings, numbers and booleans as text, `null` skipped, anything else
/// refused.
pub(crate) fn params_from(options: &Value) -> Result<BTreeMap<String, String>, String> {
    let Some(obj) = options.as_object() else {
        return Err("[invalid_options] options must be an object".into());
    };
    let mut out = BTreeMap::new();
    for (key, value) in obj {
        let text = match value {
            Value::Null => continue,
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            // JS `String(n)`: integers without a fraction, floats as shortest.
            Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
                (Some(i), _, _) => i.to_string(),
                (_, Some(u), _) => u.to_string(),
                (_, _, Some(f)) => f.to_string(),
                _ => n.to_string(),
            },
            _ => {
                return Err(format!(
                    "[invalid_options] Option \"{key}\" must be a string, number or boolean"
                ));
            }
        };
        out.insert(key.clone(), text);
    }
    Ok(out)
}

/// A local call's result plus, when a request went out, its telemetry facts.
pub(crate) struct LocalRun {
    pub(crate) result: Result<Value, String>,
    pub(crate) usage: Option<(Outcome, Duration)>,
}

impl LocalRun {
    fn refused(error: String) -> Self {
        Self {
            result: Err(error),
            usage: None,
        }
    }
}

pub(crate) async fn run(
    http: &ConnectorHttp,
    pacer: &Pacer,
    connector: &Connector,
    capability: &str,
    options: &Value,
    entry: Option<ConnectorEntry>,
) -> LocalRun {
    let name = &connector.name;
    let Some(cap) = connector.capability(capability) else {
        return LocalRun::refused(format!(
            "[unknown_data_source] {name} has no capability \"{capability}\". Call crw_find_data to see what it offers."
        ));
    };
    let params = match params_from(options) {
        Ok(p) => p,
        Err(e) => return LocalRun::refused(e),
    };
    let base = cap
        .base_url_override
        .as_deref()
        .unwrap_or(&connector.base_url);
    let test_http = url::Url::parse(base).is_ok_and(|b| http.guard().is_test_loopback(&b));
    let mut url = match build_url_with(
        base,
        &cap.path_template,
        &cap.forward_params,
        &params,
        test_http,
    ) {
        Ok(u) => u,
        Err(e) if e == MISCONFIGURED => {
            return LocalRun::refused(format!(
                "[connector_misconfigured] {name} is misconfigured right now. Try again later."
            ));
        }
        Err(e) => return LocalRun::refused(format!("[invalid_options] {e}")),
    };

    let key = match keys::bind(connector, &url, entry) {
        Ok(k) => k,
        Err(e) => return LocalRun::refused(e),
    };

    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    match HeaderValue::from_str(connector.user_agent.as_deref().unwrap_or("").trim()) {
        Ok(ua) => headers.insert(USER_AGENT, ua),
        Err(_) => {
            return LocalRun::refused(format!(
                "[connector_misconfigured] {name} is misconfigured right now. Try again later."
            ));
        }
    };
    if let Some(key) = key.as_deref() {
        let param = connector.auth.param.as_deref().unwrap_or("").trim();
        let header = match connector.auth.kind.as_str() {
            "query" => {
                search_params_set(&mut url, param, key);
                None
            }
            "header" => match header_set(key) {
                Some(set) => {
                    for (n, v) in set {
                        let (Ok(n), Ok(mut v)) = (
                            HeaderName::from_bytes(n.as_bytes()),
                            HeaderValue::from_str(&v),
                        ) else {
                            return LocalRun::refused(key_invalid(connector));
                        };
                        v.set_sensitive(true);
                        headers.insert(n, v);
                    }
                    None
                }
                None => {
                    let Ok(n) = HeaderName::from_bytes(param.as_bytes()) else {
                        return LocalRun::refused(format!(
                            "[connector_misconfigured] {name} is misconfigured right now. Try again later."
                        ));
                    };
                    Some((n, HeaderValue::from_str(key)))
                }
            },
            "basic" => {
                let token = base64::engine::general_purpose::STANDARD.encode(format!("{key}:"));
                Some((
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Basic {token}")),
                ))
            }
            _ => None,
        };
        if let Some((name_h, value)) = header {
            let Ok(mut value) = value else {
                return LocalRun::refused(key_invalid(connector));
            };
            value.set_sensitive(true);
            headers.insert(name_h, value);
        }
    }
    let redactor = Redactor::new(key.as_deref());

    let min_interval = Duration::from_millis(connector.min_interval_ms.unwrap_or(0));
    let mut last = pacer.lock(&connector.provider).await;
    let started = Instant::now();
    let mut retried = false;
    let (result, outcome) = loop {
        wait_turn(&mut last, min_interval).await;
        let attempt = tokio::time::timeout(
            ATTEMPT_TIMEOUT,
            attempt(http, url.clone(), &headers, &redactor),
        )
        .await
        .unwrap_or(Err(Failure::Fetch(FetchError::Timeout)));
        match attempt {
            Err(Failure::RateLimited(Some(wait))) if !retried && wait <= MAX_RETRY_AFTER => {
                retried = true;
                tokio::time::sleep(wait).await;
            }
            Ok((data, final_url)) => break (Ok((data, final_url)), Outcome::Success),
            Err(f) => {
                let set_key = key
                    .is_some()
                    .then(|| keys::set_key_hint(&connector.provider));
                break (
                    Err(f.message(name, &redactor, set_key.as_deref())),
                    f.outcome(),
                );
            }
        }
    };
    let latency = started.elapsed();
    drop(last);

    let result = result.map(|(data, final_url)| {
        json!({
            "provider": connector.provider,
            "capability": cap.capability,
            "creditsUsed": 0,
            "provenance": {
                "source": name,
                "upstreamUrl": redactor.apply(final_url.as_str()),
                "licence": connector.licence,
                "licenceUrl": connector.licence_url,
                "attributionText": connector.attribution_text,
                "retrievedAt": crate::types::rfc3339_utc(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                ),
            },
            "data": data,
        })
    });
    LocalRun {
        result,
        usage: Some((outcome, latency)),
    }
}

enum Failure {
    Fetch(FetchError),
    RateLimited(Option<Duration>),
    Status(u16),
    Unusable(&'static str),
}

impl Failure {
    fn outcome(&self) -> Outcome {
        match self {
            Failure::Fetch(FetchError::Blocked(_)) => Outcome::Blocked,
            Failure::Fetch(FetchError::Timeout) => Outcome::Timeout,
            Failure::Fetch(FetchError::Network(_)) | Failure::Status(_) => Outcome::UpstreamError,
            Failure::Fetch(FetchError::TooLarge) | Failure::Unusable(_) => Outcome::InvalidResponse,
            Failure::RateLimited(_) => Outcome::RateLimited,
        }
    }

    /// `set_key` is the remedy line when the user's own key was sent.
    fn message(&self, name: &str, redactor: &Redactor, set_key: Option<&str>) -> String {
        match self {
            Failure::Fetch(FetchError::Blocked(why)) => format!(
                "[blocked] The request to {name} was refused before it left this machine ({}).",
                redactor.apply(why)
            ),
            Failure::Fetch(FetchError::Timeout) => {
                format!("[upstream_timeout] {name} did not respond in time. Try again.")
            }
            Failure::Fetch(FetchError::Network(e)) => {
                tracing::debug!("connector network error: {}", redactor.apply(e));
                format!("[upstream_error] {name} could not be reached. Try again.")
            }
            Failure::Fetch(FetchError::TooLarge) => {
                format!("[invalid_response] {name} returned a response over the 8 MiB limit.")
            }
            Failure::RateLimited(_) => format!(
                "[upstream_rate_limited] {name} is rate limiting requests from this machine. Try again later."
            ),
            Failure::Status(status) if *status >= 500 => {
                format!(
                    "[upstream_error] {name} is currently unavailable (HTTP {status}). Try again."
                )
            }
            Failure::Status(status @ (401 | 403)) if set_key.is_some() => format!(
                "[upstream_error] {name} refused the stored key (HTTP {status}). Check it or store it again. {}",
                set_key.unwrap_or_default()
            ),
            Failure::Status(status) => {
                format!("[upstream_error] {name} returned HTTP {status} for this request.")
            }
            Failure::Unusable(why) => {
                tracing::debug!("connector {name}: {why}");
                format!("[invalid_response] {name} returned an unusable response. Try again.")
            }
        }
    }
}

/// `\bjson\b`, case-insensitive, as in execute.ts.
fn is_json_content_type(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    let word = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
    ct.match_indices("json").any(|(i, _)| {
        let bytes = ct.as_bytes();
        !word(i.checked_sub(1).and_then(|j| bytes.get(j))) && !word(bytes.get(i + 4))
    })
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

async fn attempt(
    http: &ConnectorHttp,
    url: url::Url,
    headers: &HeaderMap,
    redactor: &Redactor,
) -> Result<(Value, url::Url), Failure> {
    let (resp, final_url) = http.get(url, headers).await.map_err(Failure::Fetch)?;
    let status = resp.status().as_u16();
    if status == 429 {
        return Err(Failure::RateLimited(retry_after(resp.headers())));
    }
    if status >= 400 {
        return Err(Failure::Status(status));
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !is_json_content_type(content_type) {
        return Err(Failure::Unusable("not a JSON content type"));
    }
    let raw = read_capped(resp).await.map_err(Failure::Fetch)?;
    // A body that is not JSON is never surfaced, so only parsed values need the
    // pass; it runs before anything else holds the value.
    match serde_json::from_slice::<Value>(&raw) {
        Ok(Value::Null) => Err(Failure::Unusable("empty body")),
        Ok(v) => Ok((redactor.apply_json(v), final_url)),
        Err(_) => Err(Failure::Unusable("body is not JSON")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connectors_json_content_type_matches_word_boundary() {
        for ok in [
            "application/json",
            "application/vnd.api+json; charset=utf-8",
            "Application/JSON",
            "text/json",
        ] {
            assert!(is_json_content_type(ok), "{ok}");
        }
        for bad in ["text/html", "application/jsonp", "application/xjson", ""] {
            assert!(!is_json_content_type(bad), "{bad}");
        }
    }

    #[test]
    fn connectors_redactor_covers_every_form_in_any_case() {
        let key = "a b+c/d\"é";
        let r = Redactor::new(Some(key));
        let uri = encode_uri_component(key);
        let forms = [
            key.to_string(),
            uri.clone(),
            uri.to_ascii_lowercase(),
            url::form_urlencoded::byte_serialize(key.as_bytes()).collect::<String>(),
            base64::engine::general_purpose::STANDARD.encode(format!("{key}:")),
        ];
        for form in forms {
            let out = r.apply(&format!("x {form} y"));
            assert_eq!(out, "x REDACTED y", "form {form:?}");
        }
        assert_eq!(Redactor::new(None).apply("abc"), "abc");
        assert_eq!(Redactor::new(Some("")).apply("abc"), "abc");
    }

    /// JSON escapes decode to the key only after the parse; the value pass
    /// must catch them in strings and in object keys.
    #[test]
    fn connectors_redactor_sees_through_json_escapes() {
        let r = Redactor::new(Some("abc/def"));
        for body in [
            r#"{"echo":"abc\/def"}"#,
            r#"{"echo":"\u0061bc/def"}"#,
            r#"{"echo":"x abc%2fdef y"}"#,
            r#"{"abc\/def":1}"#,
            r#"[["\u0061bc\u002fdef"]]"#,
        ] {
            let v: Value = serde_json::from_str(body).unwrap();
            let out = r.apply_json(v).to_string();
            assert!(
                !out.contains("abc/def") && !out.contains("abc%2fdef"),
                "{body} -> {out}"
            );
            assert!(out.contains("REDACTED"), "{body} -> {out}");
        }
        let digits = Redactor::new(Some("12345678"));
        let v: Value = serde_json::from_str(r#"{"apikey":12345678,"n":42,"ok":true}"#).unwrap();
        assert_eq!(
            digits.apply_json(v),
            json!({"apikey": "REDACTED", "n": 42, "ok": true})
        );
        let plus = Redactor::new(Some("a b"));
        let v: Value = serde_json::from_str(r#"{"q":"a+b","u":"a%20b"}"#).unwrap();
        assert_eq!(
            plus.apply_json(v),
            json!({"q": "REDACTED", "u": "REDACTED"})
        );
    }

    #[test]
    fn connectors_params_from_matches_execute_ts() {
        let p = params_from(&json!({"a": "x", "b": 2, "c": 1.5, "d": true, "e": null})).unwrap();
        assert_eq!(p.get("a").unwrap(), "x");
        assert_eq!(p.get("b").unwrap(), "2");
        assert_eq!(p.get("c").unwrap(), "1.5");
        assert_eq!(p.get("d").unwrap(), "true");
        assert!(!p.contains_key("e"));
        assert!(
            params_from(&json!({"a": [1]}))
                .unwrap_err()
                .starts_with("[invalid_options]")
        );
        assert!(params_from(&json!({"a": {}})).is_err());
        assert!(params_from(&json!([])).is_err());
    }
}
