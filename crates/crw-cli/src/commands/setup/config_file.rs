//! Atomic writer for the per-user config file at `~/.config/crw/config.toml`.
//!
//! Setup populates a [`UserConfig`] in memory, merges it with whatever is
//! already on disk (so a re-run that only configures the LLM doesn't blow
//! away the SearXNG URL from a previous run), and writes the result via a
//! tmpfile + rename to keep the on-disk file atomic and 0600-owned.
//!
//! Hardening mirrors the SearXNG settings file: parent dir forced to 0700,
//! symlink targets refused, write goes through `O_NOFOLLOW` so a swap-in
//! between our check and our open cannot redirect the write elsewhere.

use crw_core::connectors::keys::ConnectorEntry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const USER_CONFIG_FILENAME: &str = "config.toml";
const USER_CONFIG_SUBDIR: &str = ".config/crw";

/// Subset of `crw-core::AppConfig` that `crw setup` knows how to populate.
///
/// We deliberately don't re-export the upstream struct — its tree is huge
/// and most fields belong to operators (rate limits, escalation policies,
/// etc). Setup is for end-user credentials and URLs only. Anything missing
/// here can still be edited by hand or set via env var.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<SearchSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction: Option<ExtractionSection>,
    /// `[connectors.<slug>]`: the user's own keys for data sources that run on
    /// this machine, written by `crw connectors set-key`. Setup never fills it
    /// but must carry it through, or a re-run would erase the keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connectors: Option<BTreeMap<String, ConnectorEntry>>,
    /// Every section this struct does not model (`[mcp]`, `[renderer]`, ...),
    /// kept verbatim so a write never drops what the user set by hand.
    #[serde(flatten)]
    pub rest: toml::Table,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// `false` turns off data-source usage counts; kept across re-runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<bool>,
    /// Keys this section does not model, kept verbatim across rewrites.
    #[serde(flatten)]
    pub rest: toml::Table,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchSection {
    /// `searxng_url` is the original spelling of this key. Existing
    /// `~/.config/crw/config.toml` files still carry it, so it stays accepted
    /// on read; we only ever write the new name.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "searxng_url"
    )]
    pub search_backend_url: Option<String>,
    /// Keys this section does not model, kept verbatim across rewrites.
    #[serde(flatten)]
    pub rest: toml::Table,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractionSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<LlmSection>,
    /// Keys this section does not model, kept verbatim across rewrites.
    #[serde(flatten)]
    pub rest: toml::Table,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub azure_api_version: Option<String>,
    /// Keys this section does not model, kept verbatim across rewrites.
    #[serde(flatten)]
    pub rest: toml::Table,
}

/// Resolve the path of the per-user config file. Honors
/// `$CRW_USER_CONFIG_DIR` so tests don't have to monkey-patch `$HOME`.
pub fn user_config_path() -> Result<PathBuf, String> {
    if let Ok(dir) = std::env::var("CRW_USER_CONFIG_DIR") {
        return Ok(PathBuf::from(dir).join(USER_CONFIG_FILENAME));
    }
    let home = std::env::var_os("HOME").ok_or("Could not determine home directory")?;
    Ok(PathBuf::from(home)
        .join(USER_CONFIG_SUBDIR)
        .join(USER_CONFIG_FILENAME))
}

/// Read the existing config file if present. Missing file → empty config.
/// Malformed TOML returns an error rather than silently dropping settings.
pub fn read_user_config(path: &Path) -> Result<UserConfig, String> {
    if !path.exists() {
        return Ok(UserConfig::default());
    }
    let raw =
        read_no_follow(path).map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    toml::from_str(&raw).map_err(|e| format!("Failed to parse {}: {}", path.display(), e))
}

/// Merge `update` into `base` field by field. Any `Some` in `update` wins;
/// `None` keeps the existing value. This is what makes the setup wizard
/// idempotent across sub-flows: configuring just the LLM later doesn't
/// blank out the SearXNG URL from the first run.
pub fn merge_config(base: UserConfig, update: UserConfig) -> UserConfig {
    UserConfig {
        client: merge_client(base.client, update.client),
        search: merge_search(base.search, update.search),
        extraction: merge_extraction(base.extraction, update.extraction),
        connectors: merge_connectors(base.connectors, update.connectors),
        rest: merge_rest(base.rest, update.rest),
    }
}

/// Unmodelled keys: the update's win, the base's stay.
fn merge_rest(mut base: toml::Table, update: toml::Table) -> toml::Table {
    base.extend(update);
    base
}

/// Per slug, an update replaces the whole entry: a key and the origins it was
/// approved for must never come from two different writes.
fn merge_connectors(
    base: Option<BTreeMap<String, ConnectorEntry>>,
    update: Option<BTreeMap<String, ConnectorEntry>>,
) -> Option<BTreeMap<String, ConnectorEntry>> {
    match (base, update) {
        (None, u) => u,
        (b, None) => b,
        (Some(mut b), Some(u)) => {
            b.extend(u);
            Some(b)
        }
    }
}

fn merge_client(
    base: Option<ClientSection>,
    update: Option<ClientSection>,
) -> Option<ClientSection> {
    match (base, update) {
        (None, u) => u,
        (b, None) => b,
        (Some(b), Some(u)) => Some(ClientSection {
            api_url: u.api_url.or(b.api_url),
            api_key: u.api_key.or(b.api_key),
            telemetry: u.telemetry.or(b.telemetry),
            rest: merge_rest(b.rest, u.rest),
        }),
    }
}

fn merge_search(
    base: Option<SearchSection>,
    update: Option<SearchSection>,
) -> Option<SearchSection> {
    match (base, update) {
        (None, u) => u,
        (b, None) => b,
        (Some(b), Some(u)) => Some(SearchSection {
            search_backend_url: u.search_backend_url.or(b.search_backend_url),
            rest: merge_rest(b.rest, u.rest),
        }),
    }
}

fn merge_extraction(
    base: Option<ExtractionSection>,
    update: Option<ExtractionSection>,
) -> Option<ExtractionSection> {
    match (base, update) {
        (None, u) => u,
        (b, None) => b,
        (Some(b), Some(u)) => Some(ExtractionSection {
            llm: merge_llm(b.llm, u.llm),
            rest: merge_rest(b.rest, u.rest),
        }),
    }
}

fn merge_llm(base: Option<LlmSection>, update: Option<LlmSection>) -> Option<LlmSection> {
    match (base, update) {
        (None, u) => u,
        (b, None) => b,
        (Some(b), Some(u)) => Some(LlmSection {
            provider: u.provider.or(b.provider),
            api_key: u.api_key.or(b.api_key),
            model: u.model.or(b.model),
            base_url: u.base_url.or(b.base_url),
            azure_api_version: u.azure_api_version.or(b.azure_api_version),
            rest: merge_rest(b.rest, u.rest),
        }),
    }
}

/// Write the merged config to disk atomically.
///
/// 1. Ensure parent dir exists at 0700.
/// 2. Refuse to follow a symlink at the destination.
/// 3. Read existing file, merge with `update`, serialize.
/// 4. Write to `config.toml.tmp` with 0600 + O_NOFOLLOW.
/// 5. Rename onto `config.toml` (atomic on POSIX).
///
/// Returns the final path so callers can show it to the user.
pub fn write_user_config(update: UserConfig) -> Result<PathBuf, String> {
    write_user_config_inner(update, false)
}

/// Write a Local-mode config and remove persisted Cloud routing credentials.
/// Selecting Local is an explicit mode switch; keeping `[client]` would make
/// `crw search` continue to prefer Cloud even after Local setup completed.
pub fn write_local_user_config(update: UserConfig) -> Result<PathBuf, String> {
    write_user_config_inner(update, true)
}

fn write_user_config_inner(
    update: UserConfig,
    clear_cloud_client: bool,
) -> Result<PathBuf, String> {
    let path = user_config_path()?;

    if let Some(parent) = path.parent() {
        ensure_secure_dir(parent)?;
    }
    reject_symlink(&path)?;

    let local_search_selection = clear_cloud_client.then(|| update.search.clone());
    let mut merged = merge_config(read_user_config(&path)?, update);
    if clear_cloud_client {
        // The routing credentials go; a telemetry opt-out and hand-set keys
        // are not routing credentials.
        merged.client = merged
            .client
            .map(|c| ClientSection {
                api_url: None,
                api_key: None,
                ..c
            })
            .filter(|c| c.telemetry.is_some() || !c.rest.is_empty());
    }
    if let Some(search) = local_search_selection {
        // In Local mode, None means the user explicitly skipped local search;
        // keeping a stale URL would contradict the completion summary. Other
        // `[search]` keys stay.
        let mut section = merged.search.take().unwrap_or_default();
        section.search_backend_url = search.and_then(|s| s.search_backend_url);
        merged.search =
            (section.search_backend_url.is_some() || !section.rest.is_empty()).then_some(section);
    }
    let body = render(&merged);

    let tmp = path.with_extension("toml.tmp");
    // The tmp path is in the same dir, so the same symlink-rejection logic applies.
    reject_symlink(&tmp)?;
    write_secret_file(&tmp, &body)?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        format!(
            "Failed to rename {} -> {}: {}",
            tmp.display(),
            path.display(),
            e
        )
    })?;
    set_secret_file_perms(&path)?;

    Ok(path)
}

/// Render the config with a header comment users can see when they `cat` it.
fn render(cfg: &UserConfig) -> String {
    let mut out = String::new();
    out.push_str("# Generated by `crw setup`. Re-run `crw setup` to update.\n");
    out.push_str("# Override individual values with CRW_* env vars (highest precedence).\n");
    out.push_str("# Hand-edits are preserved on re-run for keys you don't change.\n\n");
    // toml::to_string_pretty omits empty Option fields thanks to
    // skip_serializing_if on each Option field.
    let body = toml::to_string_pretty(cfg).expect("UserConfig should serialize");
    out.push_str(&body);
    out
}

// ---- Filesystem helpers ----------------------------------------------------
// Same hardening pattern as searxng.rs: enforce 0700 on the parent dir, refuse
// to follow symlinks, open the file with O_NOFOLLOW + mode 0o600 atomically.

#[cfg(unix)]
fn ensure_secure_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if dir.exists() {
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir, perms)
            .map_err(|e| format!("Failed to chmod 700 on {}: {}", dir.display(), e))?;
    } else {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_secure_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Failed to create {}: {}", dir.display(), e))
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "Refusing to use config file: {} is a symlink. Remove it and re-run.",
            path.display()
        )),
        _ => Ok(()),
    }
}

#[cfg(all(unix, target_os = "linux"))]
const fn nofollow_flag() -> i32 {
    0o400000
}
#[cfg(all(unix, not(target_os = "linux")))]
const fn nofollow_flag() -> i32 {
    0x0100
}

#[cfg(unix)]
fn write_secret_file(path: &Path, contents: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(nofollow_flag())
        .open(path)
        .map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
    f.write_all(contents.as_bytes())
        .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secret_file(path: &Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| format!("Failed to write {}: {}", path.display(), e))
}

#[cfg(unix)]
fn read_no_follow(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nofollow_flag())
        .open(path)?;
    let mut buf = String::new();
    f.read_to_string(&mut buf)?;
    Ok(buf)
}

#[cfg(not(unix))]
fn read_no_follow(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

#[cfg(unix)]
fn set_secret_file_perms(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)
        .map_err(|e| format!("Failed to chmod 600 on {}: {}", path.display(), e))
}

#[cfg(not(unix))]
fn set_secret_file_perms(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    // Tests mutate CRW_USER_CONFIG_DIR (process-global env), so they must run
    // serially even though `cargo test` is multi-threaded by default.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    fn isolated_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "crw-cfgfile-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        unsafe {
            std::env::set_var("CRW_USER_CONFIG_DIR", &dir);
        }
        dir
    }

    fn cleanup(dir: &Path) {
        unsafe {
            std::env::remove_var("CRW_USER_CONFIG_DIR");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn write_then_read_roundtrip() {
        let _g = env_lock();
        let dir = isolated_dir("roundtrip");
        let cfg = UserConfig {
            rest: Default::default(),
            connectors: None,
            client: Some(ClientSection {
                rest: Default::default(),
                api_url: Some("https://api.example.com".into()),
                api_key: Some("test-key".into()),
                telemetry: None,
            }),
            search: Some(SearchSection {
                rest: Default::default(),
                search_backend_url: Some("http://localhost:8080".into()),
            }),
            extraction: None,
        };
        let path = write_user_config(cfg.clone()).unwrap();
        let on_disk = read_user_config(&path).unwrap();
        assert_eq!(
            on_disk.client.unwrap().api_url.as_deref(),
            Some("https://api.example.com")
        );
        assert_eq!(
            on_disk.search.unwrap().search_backend_url.as_deref(),
            Some("http://localhost:8080")
        );
        cleanup(&dir);
    }

    #[test]
    fn second_write_merges_not_overwrites() {
        // Idempotency contract: writing just the LLM section must keep
        // an earlier search.search_backend_url intact.
        let _g = env_lock();
        let dir = isolated_dir("merge");

        let first = UserConfig {
            search: Some(SearchSection {
                rest: Default::default(),
                search_backend_url: Some("http://localhost:8080".into()),
            }),
            ..Default::default()
        };
        write_user_config(first).unwrap();

        let second = UserConfig {
            extraction: Some(ExtractionSection {
                rest: Default::default(),
                llm: Some(LlmSection {
                    provider: Some("deepseek".into()),
                    api_key: Some("sk-1".into()),
                    model: Some("deepseek-chat".into()),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        let path = write_user_config(second).unwrap();

        let merged = read_user_config(&path).unwrap();
        assert_eq!(
            merged.search.unwrap().search_backend_url.as_deref(),
            Some("http://localhost:8080"),
            "first run's searxng_url must survive second write"
        );
        let llm = merged.extraction.unwrap().llm.unwrap();
        assert_eq!(llm.provider.as_deref(), Some("deepseek"));
        cleanup(&dir);
    }

    #[test]
    fn setup_rerun_keeps_connector_keys_and_telemetry_opt_out() {
        let _g = env_lock();
        let dir = isolated_dir("connectors");
        let path = dir.join("config.toml");
        // As `crw connectors set-key` and a hand edit leave it.
        std::fs::write(
            &path,
            r#"
[client]
api_url = "https://api.fastcrw.com"
api_key = "old"
telemetry = false

[connectors.open-alex]
api_key = "user-key"
origins = ["https://api.openalex.org"]
auth = "query:api_key"

[mcp]
hide_credits = true

[renderer]
mode = "auto"

[renderer.chrome]
ws_url = "ws://127.0.0.1:9222"
"#,
        )
        .unwrap();
        let unmodelled_survive = || {
            let cfg = read_user_config(&path).unwrap();
            assert_eq!(cfg.rest["mcp"]["hide_credits"].as_bool(), Some(true));
            assert_eq!(cfg.rest["renderer"]["mode"].as_str(), Some("auto"));
            assert_eq!(
                cfg.rest["renderer"]["chrome"]["ws_url"].as_str(),
                Some("ws://127.0.0.1:9222")
            );
        };

        // `crw connectors set-key` writes only `[connectors.<slug>]`.
        write_user_config(UserConfig {
            connectors: Some(BTreeMap::from([(
                "dockerhub".to_string(),
                ConnectorEntry {
                    api_key: Some("dh".into()),
                    origins: vec!["https://hub.docker.com".into()],
                    auth: Some("header:Authorization".into()),
                },
            )])),
            ..Default::default()
        })
        .unwrap();
        unmodelled_survive();

        // `crw setup --cloud` writes only `[client]`.
        write_user_config(UserConfig {
            connectors: None,
            client: Some(ClientSection {
                rest: Default::default(),
                api_url: Some("https://api.fastcrw.com".into()),
                api_key: Some("new".into()),
                telemetry: None,
            }),
            ..Default::default()
        })
        .unwrap();
        let cfg = read_user_config(&path).unwrap();
        let client = cfg.client.unwrap();
        assert_eq!(client.api_key.as_deref(), Some("new"));
        assert_eq!(client.telemetry, Some(false));
        let entry = &cfg.connectors.as_ref().unwrap()["open-alex"];
        assert_eq!(entry.api_key.as_deref(), Some("user-key"));
        assert_eq!(entry.origins, vec!["https://api.openalex.org"]);
        assert_eq!(entry.auth.as_deref(), Some("query:api_key"));

        assert!(cfg.connectors.as_ref().unwrap().contains_key("dockerhub"));
        unmodelled_survive();

        // The reader in crw-core sees the same entry after the rewrite.
        let read = crw_core::connectors::keys::load("open-alex").unwrap();
        assert_eq!(read.api_key.as_deref(), Some("user-key"));

        // A local-mode switch drops the cloud routing credentials but keeps the
        // connector keys and the telemetry opt-out.
        write_local_user_config(UserConfig::default()).unwrap();
        let local = read_user_config(&path).unwrap();
        assert!(local.connectors.is_some());
        let client = local.client.expect("[client] kept for telemetry");
        assert_eq!(client.telemetry, Some(false));
        assert!(client.api_url.is_none() && client.api_key.is_none());
        unmodelled_survive();
        cleanup(&dir);
    }

    #[test]
    fn unmodelled_keys_inside_modelled_sections_survive_every_write() {
        let _g = env_lock();
        let dir = isolated_dir("nested-rest");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[search]
search_backend_url = "http://localhost:8080"
openalex_api_key = "nested-secret"

[extraction]
only_main_content = false

[extraction.llm]
provider = "anthropic"
max_tokens = 4096
"#,
        )
        .unwrap();
        let survive = |when: &str| {
            let cfg = read_user_config(&path).unwrap();
            let search = cfg.search.expect(when);
            assert_eq!(
                search.rest["openalex_api_key"].as_str(),
                Some("nested-secret"),
                "{when}"
            );
            let extraction = cfg.extraction.expect(when);
            assert_eq!(
                extraction.rest["only_main_content"].as_bool(),
                Some(false),
                "{when}"
            );
            let llm = extraction.llm.expect(when);
            assert_eq!(llm.rest["max_tokens"].as_integer(), Some(4096), "{when}");
        };

        // `crw connectors set-key`.
        write_user_config(UserConfig {
            connectors: Some(BTreeMap::from([(
                "openalex".to_string(),
                ConnectorEntry {
                    api_key: Some("k".into()),
                    origins: vec!["https://api.openalex.org".into()],
                    auth: Some("query:api_key".into()),
                },
            )])),
            ..Default::default()
        })
        .unwrap();
        survive("set-key");

        // `crw setup` touching each modelled section.
        write_user_config(UserConfig {
            client: Some(ClientSection {
                api_url: Some("https://api.fastcrw.com".into()),
                api_key: Some("c".into()),
                ..Default::default()
            }),
            search: Some(SearchSection {
                search_backend_url: Some("http://localhost:9090".into()),
                ..Default::default()
            }),
            extraction: Some(ExtractionSection {
                llm: Some(LlmSection {
                    provider: Some("deepseek".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap();
        survive("cloud and llm setup");

        // Local setup replaces the search URL, not the rest of `[search]`.
        write_local_user_config(UserConfig {
            search: Some(SearchSection {
                search_backend_url: Some("http://127.0.0.1:8080".into()),
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap();
        survive("local setup");
        cleanup(&dir);
    }

    #[test]
    fn connector_entry_update_replaces_the_whole_entry() {
        let entry = |key: &str, origin: &str| ConnectorEntry {
            api_key: Some(key.into()),
            origins: vec![origin.into()],
            auth: Some("query:api_key".into()),
        };
        let base = BTreeMap::from([
            ("a".to_string(), entry("k1", "https://a.example")),
            ("b".to_string(), entry("k2", "https://b.example")),
        ]);
        let update = BTreeMap::from([(
            "a".to_string(),
            ConnectorEntry {
                api_key: Some("k3".into()),
                ..Default::default()
            },
        )]);
        let merged = merge_connectors(Some(base), Some(update)).unwrap();
        assert!(
            merged["a"].origins.is_empty(),
            "no origins carried from the old key"
        );
        assert_eq!(merged["b"].api_key.as_deref(), Some("k2"));
    }

    #[test]
    fn local_mode_switch_removes_cloud_credentials() {
        let _g = env_lock();
        let dir = isolated_dir("local-mode-switch");

        write_user_config(UserConfig {
            connectors: None,
            client: Some(ClientSection {
                rest: Default::default(),
                api_url: Some("https://api.fastcrw.com".into()),
                api_key: Some("fc-test".into()),
                telemetry: None,
            }),
            ..Default::default()
        })
        .unwrap();

        let path = write_local_user_config(UserConfig {
            search: Some(SearchSection {
                rest: Default::default(),
                search_backend_url: Some("http://127.0.0.1:8080".into()),
            }),
            ..Default::default()
        })
        .unwrap();

        let switched = read_user_config(&path).unwrap();
        assert!(switched.client.is_none());
        assert_eq!(
            switched.search.unwrap().search_backend_url.as_deref(),
            Some("http://127.0.0.1:8080")
        );
        cleanup(&dir);
    }

    #[test]
    fn local_skip_removes_stale_search_backend() {
        let _g = env_lock();
        let dir = isolated_dir("local-search-skip");

        write_user_config(UserConfig {
            search: Some(SearchSection {
                rest: Default::default(),
                search_backend_url: Some("http://old-search:8080".into()),
            }),
            ..Default::default()
        })
        .unwrap();

        let path = write_local_user_config(UserConfig::default()).unwrap();
        let switched = read_user_config(&path).unwrap();
        assert!(switched.search.is_none());
        cleanup(&dir);
    }

    #[test]
    fn rewrite_replaces_changed_value() {
        // If the user picks a new LLM provider, the new value must win —
        // merge_llm uses `update.or(base)`, so update wins when both are Some.
        let _g = env_lock();
        let dir = isolated_dir("replace");
        write_user_config(UserConfig {
            extraction: Some(ExtractionSection {
                rest: Default::default(),
                llm: Some(LlmSection {
                    provider: Some("anthropic".into()),
                    api_key: Some("old-key".into()),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        })
        .unwrap();
        let path = write_user_config(UserConfig {
            extraction: Some(ExtractionSection {
                rest: Default::default(),
                llm: Some(LlmSection {
                    provider: Some("deepseek".into()),
                    api_key: Some("new-key".into()),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        })
        .unwrap();
        let llm = read_user_config(&path)
            .unwrap()
            .extraction
            .unwrap()
            .llm
            .unwrap();
        assert_eq!(llm.provider.as_deref(), Some("deepseek"));
        assert_eq!(llm.api_key.as_deref(), Some("new-key"));
        cleanup(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn file_is_0600_and_dir_is_0700() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let dir = isolated_dir("perms");
        let path = write_user_config(UserConfig {
            connectors: None,
            client: Some(ClientSection {
                rest: Default::default(),
                api_url: Some("u".into()),
                api_key: None,
                telemetry: None,
            }),
            ..Default::default()
        })
        .unwrap();
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
        cleanup(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_write_through_symlink() {
        let _g = env_lock();
        let dir = isolated_dir("sym");
        let real_target = std::env::temp_dir().join(format!(
            "crw-sym-target-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&real_target, "untouched").unwrap();
        let cfg_path = dir.join("config.toml");
        std::os::unix::fs::symlink(&real_target, &cfg_path).unwrap();
        let err = write_user_config(UserConfig::default()).unwrap_err();
        assert!(err.contains("symlink"), "got: {err}");
        // Real target must not have been touched.
        assert_eq!(std::fs::read_to_string(&real_target).unwrap(), "untouched");
        std::fs::remove_file(&real_target).ok();
        cleanup(&dir);
    }

    #[test]
    fn render_includes_header_comments() {
        let s = render(&UserConfig::default());
        assert!(s.starts_with("# Generated by `crw setup`"));
        assert!(s.contains("Override individual values"));
    }

    #[test]
    fn render_omits_empty_sections() {
        // Default UserConfig has all None — output should be just the header
        // plus an empty body, with no `[client]` or `[search]` headers.
        let s = render(&UserConfig::default());
        assert!(!s.contains("[client]"));
        assert!(!s.contains("[search]"));
        assert!(!s.contains("[extraction"));
    }

    #[test]
    fn user_config_path_honors_env_override() {
        let _g = env_lock();
        let dir = isolated_dir("path-override");
        let path = user_config_path().unwrap();
        assert_eq!(path, dir.join("config.toml"));
        cleanup(&dir);
    }

    #[test]
    fn user_config_path_falls_back_to_home_when_unset() {
        let _g = env_lock();
        unsafe {
            std::env::remove_var("CRW_USER_CONFIG_DIR");
        }
        let path = user_config_path();
        // On any machine running this test suite HOME is set, so this must
        // resolve, not error, and must land under ~/.config/crw.
        let path = path.expect("HOME should be set in the test environment");
        assert!(path.ends_with(".config/crw/config.toml"));
    }

    #[test]
    fn read_user_config_missing_file_returns_default() {
        let _g = env_lock();
        let dir = isolated_dir("missing-file");
        let cfg = read_user_config(&dir.join("does-not-exist.toml")).unwrap();
        assert!(cfg.client.is_none());
        assert!(cfg.search.is_none());
        assert!(cfg.extraction.is_none());
        cleanup(&dir);
    }

    #[test]
    fn read_user_config_malformed_toml_is_an_error() {
        let _g = env_lock();
        let dir = isolated_dir("malformed");
        let path = dir.join("config.toml");
        std::fs::write(&path, "this is not [ valid toml").unwrap();
        let err = read_user_config(&path).unwrap_err();
        assert!(err.contains("Failed to parse"), "got: {err}");
        cleanup(&dir);
    }

    #[test]
    fn read_user_config_accepts_legacy_searxng_url_alias() {
        let _g = env_lock();
        let dir = isolated_dir("alias");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[search]\nsearxng_url = \"http://localhost:8080\"\n").unwrap();
        let cfg = read_user_config(&path).unwrap();
        assert_eq!(
            cfg.search.unwrap().search_backend_url.as_deref(),
            Some("http://localhost:8080")
        );
        cleanup(&dir);
    }

    #[test]
    fn write_user_config_creates_missing_parent_dir() {
        let _g = env_lock();
        let dir = std::env::temp_dir().join(format!(
            "crw-cfgfile-autocreate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Deliberately do NOT create `dir` — write_user_config must create it.
        unsafe {
            std::env::set_var("CRW_USER_CONFIG_DIR", &dir);
        }
        assert!(!dir.exists());
        let path = write_user_config(UserConfig::default()).unwrap();
        assert!(path.exists());
        cleanup(&dir);
    }

    #[test]
    fn merge_config_update_none_keeps_base_untouched() {
        let base = UserConfig {
            rest: Default::default(),
            connectors: None,
            client: Some(ClientSection {
                rest: Default::default(),
                api_url: Some("https://api.example.com".into()),
                api_key: Some("k".into()),
                telemetry: None,
            }),
            search: Some(SearchSection {
                rest: Default::default(),
                search_backend_url: Some("http://localhost:8080".into()),
            }),
            extraction: None,
        };
        let merged = merge_config(base.clone(), UserConfig::default());
        assert_eq!(merged.client.unwrap().api_url, base.client.unwrap().api_url);
        assert_eq!(
            merged.search.unwrap().search_backend_url,
            base.search.unwrap().search_backend_url
        );
    }

    #[test]
    fn merge_config_base_none_takes_update_wholesale() {
        let update = UserConfig {
            connectors: None,
            client: Some(ClientSection {
                rest: Default::default(),
                api_url: Some("https://new.example.com".into()),
                api_key: None,
                telemetry: None,
            }),
            ..Default::default()
        };
        let merged = merge_config(UserConfig::default(), update);
        assert_eq!(
            merged.client.unwrap().api_url.as_deref(),
            Some("https://new.example.com")
        );
    }

    #[test]
    fn merge_llm_field_by_field_update_wins_only_when_some() {
        // update.api_key is None so base's key must survive; update.model is
        // Some so it must win — proves the merge is per-field, not per-section.
        let base = LlmSection {
            rest: Default::default(),
            provider: Some("anthropic".into()),
            api_key: Some("old-key".into()),
            model: Some("old-model".into()),
            base_url: None,
            azure_api_version: None,
        };
        let update = LlmSection {
            rest: Default::default(),
            provider: None,
            api_key: None,
            model: Some("new-model".into()),
            base_url: Some("https://custom.example.com".into()),
            azure_api_version: None,
        };
        let merged = merge_llm(Some(base), Some(update)).unwrap();
        assert_eq!(merged.provider.as_deref(), Some("anthropic"));
        assert_eq!(merged.api_key.as_deref(), Some("old-key"));
        assert_eq!(merged.model.as_deref(), Some("new-model"));
        assert_eq!(
            merged.base_url.as_deref(),
            Some("https://custom.example.com")
        );
    }

    #[test]
    fn merge_extraction_both_none_stays_none() {
        assert!(merge_extraction(None, None).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn reject_symlink_blocks_symlinked_config_path() {
        use std::os::unix::fs;
        let dir = std::env::temp_dir().join(format!("crw-cfgfile-sym-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("real.toml");
        std::fs::write(&target, "x").unwrap();
        let link = dir.join("link.toml");
        fs::symlink(&target, &link).unwrap();
        assert!(reject_symlink(&link).is_err());
        assert!(reject_symlink(&target).is_ok());
        assert!(reject_symlink(&dir.join("nope.toml")).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_user_config_round_trips_extraction_only() {
        let _g = env_lock();
        let dir = isolated_dir("extraction-only");
        let cfg = UserConfig {
            rest: Default::default(),
            connectors: None,
            client: None,
            search: None,
            extraction: Some(ExtractionSection {
                rest: Default::default(),
                llm: Some(LlmSection {
                    rest: Default::default(),
                    provider: Some("openai".into()),
                    api_key: Some("sk-test".into()),
                    model: Some("gpt-5".into()),
                    base_url: None,
                    azure_api_version: None,
                }),
            }),
        };
        let path = write_user_config(cfg).unwrap();
        let on_disk = read_user_config(&path).unwrap();
        assert!(on_disk.client.is_none());
        assert!(on_disk.search.is_none());
        let llm = on_disk.extraction.unwrap().llm.unwrap();
        assert_eq!(llm.provider.as_deref(), Some("openai"));
        assert_eq!(llm.model.as_deref(), Some("gpt-5"));
        cleanup(&dir);
    }
}
