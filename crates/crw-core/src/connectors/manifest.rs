//! The connector manifest served at `GET {api_url}/v1/data/manifest`.
//!
//! No `deny_unknown_fields`: the server may add fields without a schema bump.
//! Only fields this client reads are declared; the rest are ignored.

use super::client::Guard;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::Mutex;
use url::Url;

/// The major schema this client understands. Anything else is refused.
pub const SCHEMA_VERSION: u64 = 1;

/// Hard bound on how long a fetched manifest may drive execution. Past it the
/// copy is never used, so a revoked connector stops running within an hour
/// even when the API is down.
pub const MAX_AGE: Duration = Duration::from_secs(3600);

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest per-provider spacing a manifest may ask for.
const MAX_MIN_INTERVAL_MS: u64 = 60_000;

#[derive(Debug, Clone)]
pub struct Manifest {
    pub schema_version: u64,
    pub connectors: Vec<Connector>,
}

impl Manifest {
    pub fn connector(&self, provider: &str) -> Option<&Connector> {
        self.connectors.iter().find(|c| c.provider == provider)
    }
}

// Serialize only so the fixture test can check every field round-trips.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(rename_all = "camelCase")]
pub struct Connector {
    pub provider: String,
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub user_agent: Option<String>,
    pub auth: AuthSpec,
    #[serde(default)]
    pub key_required: bool,
    #[serde(default)]
    pub key_help_url: Option<String>,
    #[serde(default)]
    pub min_interval_ms: Option<u64>,
    #[serde(default)]
    pub licence: Option<String>,
    #[serde(default)]
    pub licence_url: Option<String>,
    #[serde(default)]
    pub attribution_text: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
pub struct AuthSpec {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub param: Option<String>,
}

impl AuthSpec {
    /// The placement string a stored key is approved for: `query:api_key`,
    /// `header:X-Api-Key`, `basic`, `none`.
    pub fn placement(&self) -> String {
        match self
            .param
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            Some(p) => format!("{}:{p}", self.kind),
            None => self.kind.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub capability: String,
    pub path_template: String,
    #[serde(default)]
    pub base_url_override: Option<String>,
    #[serde(default)]
    pub forward_params: Vec<String>,
}

impl Connector {
    pub fn capability(&self, slug: &str) -> Option<&Capability> {
        self.capabilities.iter().find(|c| c.capability == slug)
    }

    /// Every origin this connector can send a request to, deduplicated, in
    /// manifest order. This is what `crw connectors set-key` asks the user to
    /// approve.
    pub fn origins(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let bases = std::iter::once(self.base_url.as_str()).chain(
            self.capabilities
                .iter()
                .filter_map(|c| c.base_url_override.as_deref()),
        );
        for base in bases {
            if let Ok(url) = Url::parse(base) {
                let origin = url.origin().ascii_serialization();
                if !out.contains(&origin) {
                    out.push(origin);
                }
            }
        }
        out
    }

    /// Why this entry must not run, if it must not. https with a DNS host on
    /// every base, a known auth type, a contact User-Agent.
    fn reject_reason(&self, guard: Guard) -> Option<String> {
        let bases = std::iter::once(self.base_url.as_str()).chain(
            self.capabilities
                .iter()
                .filter_map(|c| c.base_url_override.as_deref()),
        );
        for base in bases {
            let Ok(url) = Url::parse(base) else {
                return Some(format!("base URL {base:?} does not parse"));
            };
            if guard.is_test_loopback(&url) {
                continue;
            }
            if url.scheme() != "https" {
                return Some(format!("base URL {base:?} is not https"));
            }
            if !matches!(url.host(), Some(url::Host::Domain(_))) {
                return Some(format!("base URL {base:?} has no DNS host"));
            }
            if let Err(e) = crate::url_safety::validate_safe_url(&url) {
                return Some(format!("base URL {base:?} refused: {e}"));
            }
        }
        let needs_param = matches!(self.auth.kind.as_str(), "query" | "header");
        let has_param = self
            .auth
            .param
            .as_deref()
            .is_some_and(|p| !p.trim().is_empty());
        if !matches!(
            self.auth.kind.as_str(),
            "none" | "query" | "header" | "basic"
        ) || (needs_param && !has_param)
        {
            return Some(format!("auth {:?} is not usable", self.auth.placement()));
        }
        if self
            .user_agent
            .as_deref()
            .is_none_or(|ua| ua.trim().is_empty())
        {
            return Some("no User-Agent".into());
        }
        // Bounds the pacing sleep, and keeps `Instant + interval` far from overflow.
        if self
            .min_interval_ms
            .is_some_and(|ms| ms > MAX_MIN_INTERVAL_MS)
        {
            return Some(format!("minIntervalMs over {MAX_MIN_INTERVAL_MS}"));
        }
        None
    }
}

/// Parse a manifest body. An unknown schema major is `[crw_too_old]`. Each
/// connector is read on its own: one that does not parse or fails the safety
/// rules is dropped (logged), and the rest stay usable.
pub(crate) fn parse(body: &str, guard: Guard) -> Result<Manifest, String> {
    let raw: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| unavailable("the data source list is not valid JSON"))?;
    let version = raw.get("schemaVersion").and_then(serde_json::Value::as_u64);
    if version != Some(SCHEMA_VERSION) {
        return Err(format!(
            "[crw_too_old] This crw is too old for the current data source list. Upgrade it ({}) and restart the MCP server.",
            super::upgrade_hint()
        ));
    }
    let entries = match raw.get("connectors") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(items)) => items.clone(),
        Some(_) => {
            return Err(unavailable("the data source list has an unexpected shape"));
        }
    };
    let connectors = entries
        .into_iter()
        .filter_map(|entry| {
            let provider = entry["provider"].as_str().unwrap_or("?").to_string();
            let connector = serde_json::from_value::<Connector>(entry)
                .map_err(|e| format!("does not parse: {e}"))
                .and_then(|c| match c.reject_reason(guard) {
                    Some(reason) => Err(reason),
                    None => Ok(c),
                });
            connector
                .inspect_err(
                    |reason| tracing::warn!(%provider, "connector dropped from manifest: {reason}"),
                )
                .ok()
        })
        .collect();
    Ok(Manifest {
        schema_version: SCHEMA_VERSION,
        connectors,
    })
}

/// A `Retry-After` given in seconds (the HTTP-date form is ignored).
pub(crate) fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

pub(crate) fn unavailable(detail: &str) -> String {
    format!(
        "[data_sources_unavailable] Data sources are unavailable right now ({detail}). Try again shortly."
    )
}

/// Per-process manifest cache with a hard expiry. Concurrent callers wait on
/// one fetch instead of each starting their own.
#[derive(Default)]
pub(crate) struct ManifestCache {
    slot: Mutex<Slot>,
}

#[derive(Default)]
struct Slot {
    /// The copy, when it was fetched by the monotonic clock, and by the wall
    /// clock: `Instant` stops while the machine sleeps, so a laptop that
    /// slept through the hour would otherwise keep a stale copy.
    copy: Option<(Arc<Manifest>, Instant, SystemTime)>,
    /// When the last fetch failed, so a down API is not asked again on
    /// every call that wants a newer copy.
    failed_at: Option<Instant>,
}

/// Whether a copy fetched at `at` / `wall` is younger than `limit` by both
/// clocks. A wall clock that went backwards counts as expired.
fn fresh(at: Instant, wall: SystemTime, now_wall: SystemTime, limit: Duration) -> bool {
    at.elapsed() < limit && now_wall.duration_since(wall).is_ok_and(|age| age < limit)
}

impl ManifestCache {
    /// The cached manifest if it was fetched within `max_age` (capped at
    /// [`MAX_AGE`]), else a fresh fetch. A failed fetch keeps the cached copy
    /// for later calls but never returns it here. With `reuse_after_failure`,
    /// for `max_age` after a failed fetch an unexpired copy answers without a
    /// new one; a caller that must not act on an older copy than it asked for
    /// passes `false` and gets the fetch or its error. An expired copy is
    /// never used.
    pub(crate) async fn get(
        &self,
        http: &reqwest::Client,
        api_url: &str,
        api_key: Option<&str>,
        max_age: Duration,
        reuse_after_failure: bool,
        guard: Guard,
    ) -> Result<Arc<Manifest>, String> {
        let mut slot = self.slot.lock().await;
        let now_wall = SystemTime::now();
        if let Some((manifest, at, wall)) = slot.copy.as_ref() {
            let recent_failure =
                reuse_after_failure && slot.failed_at.is_some_and(|t| t.elapsed() < max_age);
            if fresh(*at, *wall, now_wall, max_age.min(MAX_AGE))
                || (recent_failure && fresh(*at, *wall, now_wall, MAX_AGE))
            {
                return Ok(manifest.clone());
            }
        }
        match fetch(http, api_url, api_key, guard).await {
            Ok(manifest) => {
                let manifest = Arc::new(manifest);
                slot.copy = Some((manifest.clone(), Instant::now(), SystemTime::now()));
                slot.failed_at = None;
                Ok(manifest)
            }
            Err(e) => {
                slot.failed_at = Some(Instant::now());
                Err(e)
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn age_by(&self, by: Duration) {
        let mut slot = self.slot.lock().await;
        if let Some((_, at, wall)) = slot.copy.as_mut() {
            *at -= by;
            *wall -= by;
        }
        if let Some(t) = slot.failed_at.as_mut() {
            *t -= by;
        }
    }

    /// As if the machine slept for `by`: only the wall clock moves.
    #[cfg(test)]
    pub(crate) async fn sleep_by(&self, by: Duration) {
        if let Some((_, _, wall)) = self.slot.lock().await.copy.as_mut() {
            *wall -= by;
        }
    }
}

async fn fetch(
    http: &reqwest::Client,
    api_url: &str,
    api_key: Option<&str>,
    guard: Guard,
) -> Result<Manifest, String> {
    let mut req = http
        .get(format!(
            "{}/v1/data/manifest",
            api_url.trim_end_matches('/')
        ))
        .timeout(FETCH_TIMEOUT);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.map_err(|e| {
        tracing::debug!(
            "manifest fetch failed: {}",
            crate::error::reqwest_message(e)
        );
        unavailable("the fastCRW API could not be reached")
    })?;
    let status = resp.status().as_u16();
    match status {
        200..=299 => {}
        401 | 403 if api_key.is_none() => {
            return Err(format!(
                "[unauthorized] The fastCRW API needs an API key and none is configured (HTTP {status}). Set CRW_API_KEY."
            ));
        }
        401 | 403 => {
            return Err(format!(
                "[unauthorized] The fastCRW API did not accept the API key (HTTP {status}). Check CRW_API_KEY."
            ));
        }
        429 => {
            let wait = retry_after_secs(resp.headers())
                .map_or_else(|| "shortly".to_string(), |s| format!("in {s} seconds"));
            return Err(format!(
                "[rate_limited] Too many requests to the fastCRW API. Try again {wait}."
            ));
        }
        _ => {
            return Err(unavailable(&format!(
                "the fastCRW API answered HTTP {status}"
            )));
        }
    }
    let body = resp
        .text()
        .await
        .map_err(|_| unavailable("the data source list could not be read"))?;
    parse(&body, guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn connector_json(base: &str) -> serde_json::Value {
        json!({
            "provider": "openalex",
            "name": "OpenAlex",
            "description": null,
            "baseUrl": base,
            "userAgent": "crw (+https://fastcrw.com)",
            "auth": {"type": "query", "param": "api_key"},
            "keyRequired": false,
            "keyHelpUrl": null,
            "minIntervalMs": null,
            "licence": "CC0 1.0",
            "licenceUrl": null,
            "attributionText": null,
            "futureField": {"ignored": true},
            "capabilities": [{
                "capability": "works",
                "label": null,
                "description": null,
                "pathTemplate": "/works/{id}",
                "baseUrlOverride": null,
                "forwardParams": [],
                "parameters": [{"name": "id", "type": "string", "required": true,
                                 "description": null, "example": null, "enumValues": null}]
            }]
        })
    }

    #[test]
    fn connectors_manifest_cloud_rendered_fixture_is_accepted_whole() {
        // Generated in crw-saas by scripts/gen-connectors-manifest.ts (the real buildManifest).
        let body = include_str!("../../tests/fixtures/connectors-manifest.json");
        let raw: serde_json::Value = serde_json::from_str(body).unwrap();
        let m = parse(body, Guard::default()).unwrap();
        let want: Vec<(&str, usize)> = raw["connectors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["provider"].as_str().unwrap(),
                    c["capabilities"].as_array().unwrap().len(),
                )
            })
            .collect();
        let got: Vec<(&str, usize)> = m
            .connectors
            .iter()
            .map(|c| (c.provider.as_str(), c.capabilities.len()))
            .collect();
        assert_eq!(got, want, "a connector the cloud renders was dropped");
        // Every field the cloud sends is one this client reads, under the same
        // name: a renamed field would parse to its default and differ here.
        for (c, entry) in m
            .connectors
            .iter()
            .zip(raw["connectors"].as_array().unwrap())
        {
            assert_eq!(&serde_json::to_value(c).unwrap(), entry, "{}", c.provider);
        }
        for c in &m.connectors {
            assert_eq!(c.reject_reason(Guard::default()), None, "{}", c.provider);
        }
        let kinds: Vec<&str> = m.connectors.iter().map(|c| c.auth.kind.as_str()).collect();
        for kind in ["query", "header", "basic", "none"] {
            assert!(kinds.contains(&kind), "fixture lost its {kind} row");
        }
    }

    #[test]
    fn connectors_manifest_copy_expires_by_either_clock() {
        let hour = MAX_AGE;
        let now = SystemTime::now();
        let at = Instant::now();
        assert!(fresh(at, now, now, hour));
        // Slept through the hour: the monotonic clock barely moved.
        assert!(!fresh(at, now - hour, now, hour));
        assert!(!fresh(at, now - hour - Duration::from_secs(1), now, hour));
        // The wall clock was set back.
        assert!(!fresh(at, now + Duration::from_secs(5), now, hour));
        // Awake past the limit.
        assert!(!fresh(at - hour, now, now, hour));
    }

    #[test]
    fn connectors_manifest_parses_contract_and_ignores_unknown_fields() {
        let body =
            json!({"schemaVersion": 1, "connectors": [connector_json("https://api.openalex.org")]});
        let m = parse(&body.to_string(), Guard::default()).unwrap();
        assert_eq!(m.connectors.len(), 1);
        let c = &m.connectors[0];
        assert_eq!(c.auth.placement(), "query:api_key");
        assert_eq!(c.origins(), vec!["https://api.openalex.org"]);
        assert!(c.capability("works").is_some());
    }

    #[test]
    fn connectors_manifest_unknown_schema_version_is_too_old() {
        for v in [json!(2), json!(0), json!("1"), json!(null)] {
            let body = json!({"schemaVersion": v, "connectors": []});
            let err = parse(&body.to_string(), Guard::default()).unwrap_err();
            assert!(err.starts_with("[crw_too_old]"), "{v}: {err}");
        }
    }

    #[test]
    fn connectors_manifest_drops_bad_entries_and_keeps_the_rest() {
        let mut bad_auth = connector_json("https://ok.example.org");
        bad_auth["auth"] = json!({"type": "header", "param": null});
        let mut bad_override = connector_json("https://ok.example.org");
        bad_override["capabilities"][0]["baseUrlOverride"] = json!("https://10.0.0.1");
        let mut slow = connector_json("https://slow.example.org");
        slow["minIntervalMs"] = json!(60_001);
        let mut no_ua = connector_json("https://no-ua.example.org");
        no_ua["userAgent"] = json!(null);
        let body = json!({"schemaVersion": 1, "connectors": [
            connector_json("https://93.184.216.34"),
            connector_json("https://[2606:4700::1]"),
            connector_json("http://api.example.org"),
            connector_json("https://localhost"),
            bad_auth,
            bad_override,
            json!({"provider": "broken", "baseUrl": 5}),
            slow,
            no_ua,
            connector_json("https://api.example.org"),
        ]});
        let m = parse(&body.to_string(), Guard::default()).unwrap();
        assert_eq!(m.connectors.len(), 1);
        assert_eq!(m.connectors[0].base_url, "https://api.example.org");
    }
}
