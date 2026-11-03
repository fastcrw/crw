//! `crw connectors` (and `crw-mcp connectors`): data sources that run on this
//! machine with the user's own key. `list` shows them and whether a key is
//! stored; `set-key` stores one, after the user approves the origins and the
//! placement it will be sent with. Both binaries run this one module.
//!
//! The key is read from a no-echo prompt or from stdin, never from argv, so it
//! does not land in shell history or `ps` output.

use super::DataToolContext;
use super::execute::header_set;
use super::keys::{self, ConnectorEntry};
use super::manifest::{Connector, Manifest};
use crate::user_config::{UserConfig, write_user_config};
use clap::{Args, Subcommand};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Password};
use reqwest::header::{HeaderName, HeaderValue};
use std::collections::BTreeMap;
use std::io::{IsTerminal, Read};
use std::sync::Arc;

const DEFAULT_CLOUD_API_URL: &str = "https://api.fastcrw.com";

#[derive(Args)]
pub struct ConnectorsArgs {
    #[command(subcommand)]
    command: ConnectorsCommand,
}

#[derive(Subcommand)]
enum ConnectorsCommand {
    /// List the data sources that run on this machine and their key status
    List,
    /// Store your own API key for one data source (read from a hidden prompt or stdin)
    SetKey {
        /// Provider id, as shown by `connectors list`
        slug: String,
        /// Approve the listed origins without asking (required when the key comes from stdin)
        #[arg(long, short)]
        yes: bool,
    },
}

/// Run the command. `api_url` / `api_key` come from the binary's own flags;
/// when absent, `client.*` from the config file (or `CRW_API_KEY`) is used.
/// The error is the line to print.
pub async fn run(
    args: ConnectorsArgs,
    api_url: Option<String>,
    api_key: Option<String>,
) -> Result<(), String> {
    let manifest = fetch_manifest(api_url, api_key).await?;
    match args.command {
        ConnectorsCommand::List => {
            list(&manifest);
            Ok(())
        }
        ConnectorsCommand::SetKey { slug, yes } => set_key(&manifest, &slug, yes),
    }
}

async fn fetch_manifest(
    api_url: Option<String>,
    api_key: Option<String>,
) -> Result<Arc<Manifest>, String> {
    let client = crate::AppConfig::load()
        .map(|c| c.client)
        .unwrap_or_default();
    let (api_url, api_key) = endpoint(api_url, api_key, client);
    let has_key = api_key.is_some();
    if std::io::stderr().is_terminal() {
        eprintln!("Loading data sources from {api_url} ...");
    }
    let ctx = DataToolContext::new(api_url, api_key, false);
    ctx.manifest().await.map_err(|e| {
        let hint = match (has_key, super::running_as_crw_mcp()) {
            (true, _) => "",
            (false, true) => {
                "\nhint: no fastCRW API key is configured; pass --api-key <your fastCRW API key> or set CRW_API_KEY"
            }
            (false, false) => "\nhint: no fastCRW API key is configured; run `crw setup` first",
        };
        format!("{e}{hint}")
    })
}

/// The API to ask and the key to send it. The config file's key goes only to
/// the config file's (or the default) URL: a URL passed on the command line
/// gets only a key passed the same way, so a stored key never reaches a host
/// the user just typed.
fn endpoint(
    api_url: Option<String>,
    api_key: Option<String>,
    client: crate::config::ClientConfig,
) -> (String, Option<String>) {
    match api_url {
        Some(url) => (url, api_key),
        None => (
            client
                .api_url
                .unwrap_or_else(|| DEFAULT_CLOUD_API_URL.to_string()),
            api_key.or(client.api_key),
        ),
    }
}

/// What a source needs and whether this machine has it, in words, so the
/// column reads the same with or without color.
fn key_status(c: &Connector) -> &'static str {
    if c.auth.kind == "none" {
        return "not needed";
    }
    key_status_for(c, keys::load(&c.provider))
}

/// [`key_status`] for a given stored entry, with the same origin and
/// placement checks a call makes before it sends the key.
fn key_status_for(c: &Connector, entry: Option<ConnectorEntry>) -> &'static str {
    match entry {
        Some(e) if e.api_key.as_deref().is_some_and(|k| !k.trim().is_empty()) => {
            let origins_ok = c
                .origins()
                .iter()
                .all(|o| url::Url::parse(o).is_ok_and(|u| e.approves_origin(&u.origin())));
            if origins_ok && e.approves_placement(&c.auth.placement()) {
                "stored"
            } else {
                "stored, approve again"
            }
        }
        _ if c.key_required => "required, missing",
        _ => "optional, not set",
    }
}

fn list(manifest: &Manifest) {
    if manifest.connectors.is_empty() {
        eprintln!(
            "No data sources run on this machine right now. Sources served by fastCRW still work through crw_get_data."
        );
        return;
    }
    let rows: Vec<[String; 4]> = manifest
        .connectors
        .iter()
        .map(|c| {
            [
                shown(&c.provider),
                shown(&c.name),
                key_status(c).to_string(),
                licence_cell(c.licence.as_deref().unwrap_or("")),
            ]
        })
        .collect();
    let header = ["PROVIDER", "NAME", "KEY", "LICENCE"].map(String::from);
    let widths: Vec<usize> = (0..4)
        .map(|i| {
            std::iter::once(&header)
                .chain(&rows)
                .map(|r| console::measure_text_width(&r[i]))
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in std::iter::once(&header).chain(&rows) {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, w)| {
                console::pad_str(cell, *w, console::Alignment::Left, None).into_owned()
            })
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
    if rows
        .iter()
        .any(|r| r[2] != "not needed" && r[2] != "stored")
    {
        eprintln!(
            "\nStore a key: {} set-key <provider>",
            super::connectors_command()
        );
    }
}

/// A licence is often a paragraph; the table shows its start.
const LICENCE_WIDTH: usize = 60;

fn licence_cell(licence: &str) -> String {
    console::truncate_str(shown(licence).trim(), LICENCE_WIDTH, "...").into_owned()
}

/// Manifest text as it may reach a terminal: control characters (escape
/// sequences, carriage returns) removed, so a definition cannot rewrite what
/// the approval screen shows.
fn shown(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// Where the key goes, in words.
fn placement_text(c: &Connector) -> String {
    let param = shown(c.auth.param.as_deref().unwrap_or("").trim());
    match c.auth.kind.as_str() {
        "query" => format!("as the query parameter \"{param}\""),
        "header" => format!(
            "in the \"{param}\" header. If the source needs several headers, enter the key as a JSON object of header names to values, such as {{\"{param}\": \"...\", \"X-Other\": \"...\"}}; each pair is sent as its own header"
        ),
        _ => "as the HTTP Basic username".to_string(),
    }
}

/// The header names a key will be sent under, for a header-auth source (never
/// the values), or why it must not be stored: a key every call would refuse,
/// by the same header checks the executor runs.
fn header_names(c: &Connector, key: &str) -> Result<Option<Vec<String>>, String> {
    if c.auth.kind != "header" {
        return Ok(None);
    }
    let pairs = match header_set(key) {
        Some(set) => set,
        None if key.trim_start().starts_with('{') => {
            return Err(
                "the key starts with \"{\" but is not a flat JSON object of non-empty strings; nothing stored"
                    .into(),
            );
        }
        None => vec![(
            c.auth.param.as_deref().unwrap_or("").trim().to_string(),
            key.to_string(),
        )],
    };
    for (name, value) in &pairs {
        if HeaderName::from_bytes(name.as_bytes()).is_err() {
            return Err(format!(
                "\"{name}\" is not a valid header name; nothing stored"
            ));
        }
        if HeaderValue::from_str(value).is_err() {
            return Err(format!(
                "the value for \"{name}\" has characters a header cannot carry; nothing stored"
            ));
        }
    }
    Ok(Some(pairs.into_iter().map(|(name, _)| name).collect()))
}

fn set_key(manifest: &Manifest, slug: &str, yes: bool) -> Result<(), String> {
    let Some(connector) = manifest.connector(slug) else {
        return Err(format!(
            "no data source named \"{slug}\" runs on this machine. See `{} list`.",
            super::connectors_command()
        ));
    };
    let name = shown(&connector.name);
    if connector.auth.kind == "none" {
        eprintln!("{name} needs no key. Nothing stored.");
        return Ok(());
    }

    let origins = connector.origins();
    eprintln!("{name}: the key will be sent only to");
    for origin in &origins {
        eprintln!("  {origin}");
    }
    eprintln!("{}.", placement_text(connector));
    if let Some(help) = &connector.key_help_url {
        eprintln!("Get a key: {}", shown(help));
    }

    let interactive = std::io::stdin().is_terminal();
    if !yes {
        if !interactive {
            return Err("the key comes from stdin, so approve the origins above with --yes".into());
        }
        let approved = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("Approve these origins for your key?")
            .default(false)
            .interact()
            .map_err(|_| "cancelled; nothing stored".to_string())?;
        if !approved {
            eprintln!("Not approved. Nothing stored.");
            return Ok(());
        }
    }

    let prompt = if connector.auth.kind == "header" {
        format!("API key for {name} (or a JSON object of headers)")
    } else {
        format!("API key for {name}")
    };
    let key = if interactive {
        Password::with_theme(&ColorfulTheme::default())
            .with_prompt(prompt)
            .interact()
            .map_err(|_| "cancelled; nothing stored".to_string())?
    } else {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("could not read the key from stdin: {e}"))?;
        buf
    };
    let key = key.trim();
    if key.is_empty() {
        return Err("no key given; nothing stored".into());
    }
    if let Some(names) = header_names(connector, key)? {
        eprintln!("Headers it will send: {}", names.join(", "));
    }

    let entry = ConnectorEntry {
        api_key: Some(key.to_string()),
        origins,
        auth: Some(connector.auth.placement()),
    };
    let path = write_user_config(UserConfig {
        connectors: Some(BTreeMap::from([(slug.to_string(), entry)])),
        ..Default::default()
    })?;
    eprintln!(
        "Stored the key for {name} in {}. Agents can use it right away.",
        path.display()
    );
    if keys::env_overrides(slug) {
        eprintln!(
            "note: a CRW_CONNECTORS__ environment variable for this source is set and takes precedence."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_connector() -> Connector {
        serde_json::from_value(serde_json::json!({
            "provider": "fca",
            "name": "FCA Register",
            "baseUrl": "https://register.fca.org.uk",
            "userAgent": "crw",
            "auth": {"type": "header", "param": "X-Auth-Key"},
            "keyRequired": true,
            "capabilities": []
        }))
        .unwrap()
    }

    #[test]
    fn connectors_cli_strips_control_characters_from_manifest_text() {
        assert_eq!(shown("FCA\u{1b}[2K\rRegister\u{7}"), "FCA[2KRegister");
        assert_eq!(licence_cell("CC0\u{1b}]0;x\u{7} 1.0"), "CC0]0;x 1.0");
        let mut c = header_connector();
        c.auth.param = Some("X-Key\u{1b}[1A".into());
        let text = placement_text(&c);
        assert!(!text.chars().any(char::is_control), "{text:?}");
        assert!(text.contains("\"X-Key[1A\" header"), "{text}");
    }

    #[test]
    fn connectors_cli_licence_cell_shows_the_start() {
        // The docs' `connectors list` sample shows exactly these cells.
        assert_eq!(
            licence_cell(
                "FRED provides data and data services to the public for non-commercial, educational, and personal uses subject to a few prohibitions."
            ),
            "FRED provides data and data services to the public for no..."
        );
        assert_eq!(
            licence_cell(
                "Open Library\u{2019}s APIs exist to support the open book ecosystem and human-centered discovery."
            ),
            "Open Library\u{2019}s APIs exist to support the open book ecosys..."
        );
        assert_eq!(licence_cell("CC0 1.0"), "CC0 1.0");
    }

    #[test]
    fn connectors_cli_config_key_never_follows_a_flag_url() {
        let client = || crate::config::ClientConfig {
            api_url: Some("https://api.fastcrw.com".into()),
            api_key: Some("config-key".into()),
            ..Default::default()
        };
        let flag = Some("https://other.example".to_string());
        assert_eq!(
            endpoint(flag.clone(), None, client()),
            ("https://other.example".into(), None)
        );
        assert_eq!(
            endpoint(flag, Some("flag-key".into()), client()),
            ("https://other.example".into(), Some("flag-key".into()))
        );
        assert_eq!(
            endpoint(None, None, client()),
            ("https://api.fastcrw.com".into(), Some("config-key".into()))
        );
        assert_eq!(
            endpoint(None, Some("flag-key".into()), client()),
            ("https://api.fastcrw.com".into(), Some("flag-key".into()))
        );
        let no_url = crate::config::ClientConfig {
            api_key: Some("config-key".into()),
            ..Default::default()
        };
        assert_eq!(
            endpoint(None, None, no_url),
            (DEFAULT_CLOUD_API_URL.into(), Some("config-key".into()))
        );
    }

    #[test]
    fn connectors_cli_key_status_compares_origins_like_a_call() {
        let c = header_connector();
        let entry = |origins: &[&str], auth: &str| {
            Some(ConnectorEntry {
                api_key: Some("k".into()),
                origins: origins.iter().map(|o| o.to_string()).collect(),
                auth: Some(auth.into()),
            })
        };
        // Spelled differently, same origin: a call would send the key.
        for origin in [
            "https://register.fca.org.uk",
            "https://register.fca.org.uk/",
            "https://REGISTER.fca.org.uk:443",
        ] {
            assert_eq!(
                key_status_for(&c, entry(&[origin], " header:X-Auth-Key ")),
                "stored",
                "{origin}"
            );
        }
        assert_eq!(
            key_status_for(&c, entry(&["https://other.example"], "header:X-Auth-Key")),
            "stored, approve again"
        );
        assert_eq!(
            key_status_for(&c, entry(&["https://register.fca.org.uk"], "header:Other")),
            "stored, approve again"
        );
        assert_eq!(key_status_for(&c, None), "required, missing");
    }

    #[test]
    fn connectors_cli_header_names_never_show_values() {
        let c = header_connector();
        assert_eq!(
            header_names(&c, r#"{"X-Auth-Email":"me@example.com","X-Auth-Key":"k1"}"#)
                .unwrap()
                .unwrap(),
            vec!["X-Auth-Email", "X-Auth-Key"]
        );
        assert_eq!(
            header_names(&c, "plain").unwrap().unwrap(),
            vec!["X-Auth-Key"]
        );
        assert!(placement_text(&c).contains("JSON object of header names to values"));
    }

    #[test]
    fn connectors_cli_refuses_header_keys_every_call_would_refuse() {
        let c = header_connector();
        for (key, why) in [
            (r#"{"X-Auth-Key":"k1",}"#, "not a flat JSON object"),
            (r#"{"X-Auth-Key":1}"#, "not a flat JSON object"),
            ("{oops", "not a flat JSON object"),
            ("{}", "not a flat JSON object"),
            (r#"{"X Bad":"v"}"#, "not a valid header name"),
            (r#"{"X-Auth-Key":"line\u000abreak"}"#, "cannot carry"),
            ("line\nbreak", "cannot carry"),
        ] {
            let err = header_names(&c, key).unwrap_err();
            assert!(
                err.contains(why) && err.ends_with("nothing stored"),
                "{key}: {err}"
            );
            assert!(!err.contains("k1") && !err.contains("break"), "{err}");
        }
        let mut bad_param = header_connector();
        bad_param.auth.param = Some("X Api Key".into());
        assert!(header_names(&bad_param, "k").is_err());
        let mut query = header_connector();
        query.auth.kind = "query".into();
        assert_eq!(header_names(&query, "{oops").unwrap(), None);
    }
}
