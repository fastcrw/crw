//! The user's own keys for connectors, and where each one may go.
//!
//! Read from `[connectors.<slug>]` in the per-user config file (the one
//! `crw setup` writes), outside [`crate::config::AppConfig`], so a malformed
//! entry can never break loading of `client.api_key`. A `[connectors]` table
//! in a `$CRW_CONFIG` file is not read; Docker and CI use the env form:
//! `CRW_CONNECTORS__<SLUG>__API_KEY`, `__ORIGINS` (comma-separated) and
//! `__AUTH`, with `-` in the slug written as `_`.
//!
//! A key is attached only when the request's origin is one the user approved
//! and the manifest's auth placement is the one they approved, so a manifest
//! changed after `crw connectors set-key` can make a call fail but cannot
//! send the key anywhere new.

use super::manifest::Connector;
use serde::{Deserialize, Serialize};
use std::path::Path;
use url::Url;

/// One `[connectors.<slug>]` entry. `crw setup`'s writer reuses this type, so
/// reader and writer cannot drift.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConnectorEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Origins the user approved for this key, e.g. `https://api.openalex.org`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origins: Vec<String>,
    /// Approved placement: `query:<param>`, `header:<name>` or `basic`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
}

impl std::fmt::Debug for ConnectorEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectorEntry")
            .field("api_key", &self.api_key.as_ref().map(|_| "REDACTED"))
            .field("origins", &self.origins)
            .field("auth", &self.auth)
            .finish()
    }
}

impl ConnectorEntry {
    fn key(&self) -> Option<&str> {
        self.api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
    }
}

fn env_prefix(slug: &str) -> String {
    format!(
        "CRW_CONNECTORS__{}",
        slug.to_ascii_uppercase().replace('-', "_")
    )
}

/// The entry for `slug` from the real env and user config file.
pub fn load(slug: &str) -> Option<ConnectorEntry> {
    load_from(slug, crate::config::user_config_path().as_deref(), |name| {
        std::env::var(name).ok()
    })
}

/// Whether a usable key is stored for `slug`.
pub fn key_configured(slug: &str) -> bool {
    load(slug).is_some_and(|e| e.key().is_some())
}

/// Env wins as a whole entry when its `__API_KEY` is set; otherwise the file.
pub(crate) fn load_from(
    slug: &str,
    path: Option<&Path>,
    env: impl Fn(&str) -> Option<String>,
) -> Option<ConnectorEntry> {
    let prefix = env_prefix(slug);
    let var = |suffix: &str| {
        env(&format!("{prefix}__{suffix}"))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    if let Some(api_key) = var("API_KEY") {
        return Some(ConnectorEntry {
            api_key: Some(api_key),
            origins: var("ORIGINS")
                .map(|o| {
                    o.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default(),
            auth: var("AUTH"),
        });
    }

    let raw = std::fs::read_to_string(path?).ok()?;
    let table: toml::Table = toml::from_str(&raw)
        .inspect_err(|_| tracing::warn!("config file is not valid TOML; no connector keys read"))
        .ok()?;
    let entry = table.get("connectors")?.get(slug)?.clone();
    entry
        .try_into()
        .inspect_err(|_| tracing::warn!("[connectors.{slug}] is malformed and was ignored"))
        .ok()
}

pub(crate) fn set_key_hint(slug: &str) -> String {
    format!(
        "Ask the user to run this in their own terminal: {} set-key {slug}",
        super::connectors_command()
    )
}

/// The key to attach to a request for `url`, if any, or why the call must not
/// go out.
pub(crate) fn bind(
    connector: &Connector,
    url: &Url,
    entry: Option<ConnectorEntry>,
) -> Result<Option<String>, String> {
    if connector.auth.kind == "none" {
        return Ok(None);
    }
    let slug = &connector.provider;
    let entry = entry.unwrap_or_default();
    let Some(key) = entry.key() else {
        if !connector.key_required {
            return Ok(None);
        }
        let help = connector
            .key_help_url
            .as_deref()
            .map(|u| format!(" ({u})"))
            .unwrap_or_default();
        return Err(format!(
            "[connector_key_missing] {} needs an API key{help}. {}",
            connector.name,
            set_key_hint(slug)
        ));
    };
    let Some(approved_auth) = entry.auth.as_deref().map(str::trim) else {
        return Err(not_approved(connector, "has no approved auth placement"));
    };
    if entry.origins.is_empty() {
        return Err(not_approved(connector, "has no approved origins"));
    }
    let origin = url.origin();
    let origin_ok = entry
        .origins
        .iter()
        .any(|o| Url::parse(o).is_ok_and(|o| o.origin() == origin));
    let placement = connector.auth.placement();
    if !origin_ok || approved_auth != placement {
        return Err(not_approved(
            connector,
            &format!(
                "was approved for other origins or another placement; this call wants {} as {placement}",
                origin.ascii_serialization()
            ),
        ));
    }
    Ok(Some(key.to_string()))
}

fn not_approved(connector: &Connector, why: &str) -> String {
    format!(
        "[connector_origin_not_approved] The stored key for {} {why}. It was not sent. {}",
        connector.name,
        set_key_hint(&connector.provider)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn connector(auth: serde_json::Value, key_required: bool) -> Connector {
        serde_json::from_value(serde_json::json!({
            "provider": "open-alex",
            "name": "OpenAlex",
            "baseUrl": "https://api.openalex.org",
            "userAgent": "crw",
            "auth": auth,
            "keyRequired": key_required,
            "capabilities": []
        }))
        .unwrap()
    }

    fn entry(key: &str, origins: &[&str], auth: Option<&str>) -> Option<ConnectorEntry> {
        Some(ConnectorEntry {
            api_key: Some(key.into()),
            origins: origins.iter().map(|s| s.to_string()).collect(),
            auth: auth.map(String::from),
        })
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn connectors_bind_attaches_only_on_approved_origin_and_placement() {
        let c = connector(
            serde_json::json!({"type": "query", "param": "api_key"}),
            true,
        );
        let u = url("https://api.openalex.org/works/W1");
        let ok = entry("k1", &["https://api.openalex.org"], Some("query:api_key"));
        assert_eq!(bind(&c, &u, ok).unwrap().as_deref(), Some("k1"));

        let other_origin = entry("k1", &["https://evil.example"], Some("query:api_key"));
        let err = bind(&c, &u, other_origin).unwrap_err();
        assert!(err.starts_with("[connector_origin_not_approved]"), "{err}");
        assert!(!err.contains("k1"));

        let other_auth = entry("k1", &["https://api.openalex.org"], Some("header:api_key"));
        assert!(
            bind(&c, &u, other_auth)
                .unwrap_err()
                .starts_with("[connector_origin_not_approved]")
        );
    }

    #[test]
    fn connectors_bind_refuses_key_without_origins_or_auth() {
        let c = connector(
            serde_json::json!({"type": "header", "param": "X-Key"}),
            false,
        );
        let u = url("https://api.openalex.org/x");
        assert!(bind(&c, &u, entry("k", &[], Some("header:X-Key"))).is_err());
        assert!(bind(&c, &u, entry("k", &["https://api.openalex.org"], None)).is_err());
    }

    #[test]
    fn connectors_bind_missing_key() {
        let required = connector(
            serde_json::json!({"type": "query", "param": "api_key"}),
            true,
        );
        let u = url("https://api.openalex.org/x");
        let err = bind(&required, &u, None).unwrap_err();
        assert!(err.starts_with("[connector_key_missing]"), "{err}");
        assert!(err.contains("crw connectors set-key open-alex"), "{err}");

        let optional = connector(
            serde_json::json!({"type": "query", "param": "api_key"}),
            false,
        );
        assert_eq!(bind(&optional, &u, None).unwrap(), None);
        let keyless = connector(serde_json::json!({"type": "none", "param": null}), false);
        assert_eq!(bind(&keyless, &u, entry("k", &[], None)).unwrap(), None);
    }

    #[test]
    fn connectors_env_form_with_hyphen_slug_and_comma_origins() {
        let vars: HashMap<&str, &str> = HashMap::from([
            ("CRW_CONNECTORS__OPEN_ALEX__API_KEY", " envkey "),
            (
                "CRW_CONNECTORS__OPEN_ALEX__ORIGINS",
                "https://api.openalex.org, https://b.example ,",
            ),
            ("CRW_CONNECTORS__OPEN_ALEX__AUTH", "query:api_key"),
        ]);
        let e = load_from("open-alex", None, |k| vars.get(k).map(|v| v.to_string())).unwrap();
        assert_eq!(e.api_key.as_deref(), Some("envkey"));
        assert_eq!(
            e.origins,
            vec!["https://api.openalex.org", "https://b.example"]
        );
        assert_eq!(e.auth.as_deref(), Some("query:api_key"));
    }

    fn temp_config(body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "crw-connectors-keys-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn connectors_file_form_and_malformed_entry_isolated() {
        let path = temp_config(
            r#"
[client]
api_url = "https://api.fastcrw.com"
api_key = "client-key"

[connectors.openalex]
api_key = "filekey"
origins = ["https://api.openalex.org"]
auth = "query:api_key"

[connectors.broken]
api_key = "x"
origins = 5
"#,
        );
        let e = load_from("openalex", Some(&path), |_| None).unwrap();
        assert_eq!(e.api_key.as_deref(), Some("filekey"));
        assert!(load_from("broken", Some(&path), |_| None).is_none());
        assert!(load_from("absent", Some(&path), |_| None).is_none());

        // The same file through AppConfig's file pipeline: the broken entry
        // must not stop `client.api_key` from loading.
        let cfg: crate::config::AppConfig = config::Config::builder()
            .add_source(config::File::from(path.clone()))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(cfg.client.api_key.as_deref(), Some("client-key"));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn connectors_debug_redacts_keys() {
        let e = entry("supersecret", &["https://a.example"], Some("basic")).unwrap();
        let s = format!("{e:?}");
        assert!(!s.contains("supersecret"), "{s}");
        assert!(s.contains("REDACTED"));

        let c = crate::config::ClientConfig {
            api_key: Some("crwsecret".into()),
            ..Default::default()
        };
        let s = format!("{c:?}");
        assert!(!s.contains("crwsecret"), "{s}");
    }
}
