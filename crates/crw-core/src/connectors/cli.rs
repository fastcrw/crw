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
    let api_url = api_url
        .or(client.api_url)
        .unwrap_or_else(|| DEFAULT_CLOUD_API_URL.to_string());
    let api_key = api_key.or(client.api_key);
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

/// What a source needs and whether this machine has it, in words, so the
/// column reads the same with or without color.
fn key_status(c: &Connector) -> &'static str {
    if c.auth.kind == "none" {
        return "not needed";
    }
    match keys::load(&c.provider) {
        Some(e) if e.api_key.as_deref().is_some_and(|k| !k.trim().is_empty()) => {
            let origins_ok = c.origins().iter().all(|o| e.origins.contains(o));
            if origins_ok && e.auth.as_deref() == Some(c.auth.placement().as_str()) {
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
                c.provider.clone(),
                c.name.clone(),
                key_status(c).to_string(),
                c.licence.clone().unwrap_or_default(),
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

/// Where the key goes, in words.
fn placement_text(c: &Connector) -> String {
    let param = c.auth.param.as_deref().unwrap_or("").trim();
    match c.auth.kind.as_str() {
        "query" => format!("as the query parameter \"{param}\""),
        "header" => format!(
            "in the \"{param}\" header. If the source needs several headers, enter the key as a JSON object of header names to values, such as {{\"{param}\": \"...\", \"X-Other\": \"...\"}}; each pair is sent as its own header"
        ),
        _ => "as the HTTP Basic username".to_string(),
    }
}

/// The header names a key will be sent under, for a header-auth source. Never
/// the values.
fn header_names(c: &Connector, key: &str) -> Option<Vec<String>> {
    if c.auth.kind != "header" {
        return None;
    }
    Some(match header_set(key) {
        Some(set) => set.into_iter().map(|(name, _)| name).collect(),
        None => vec![c.auth.param.as_deref().unwrap_or("").trim().to_string()],
    })
}

fn set_key(manifest: &Manifest, slug: &str, yes: bool) -> Result<(), String> {
    let Some(connector) = manifest.connector(slug) else {
        return Err(format!(
            "no data source named \"{slug}\" runs on this machine. See `{} list`.",
            super::connectors_command()
        ));
    };
    if connector.auth.kind == "none" {
        eprintln!("{} needs no key. Nothing stored.", connector.name);
        return Ok(());
    }

    let origins = connector.origins();
    eprintln!("{}: the key will be sent only to", connector.name);
    for origin in &origins {
        eprintln!("  {origin}");
    }
    eprintln!("{}.", placement_text(connector));
    if let Some(help) = &connector.key_help_url {
        eprintln!("Get a key: {help}");
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
        format!(
            "API key for {} (or a JSON object of headers)",
            connector.name
        )
    } else {
        format!("API key for {}", connector.name)
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
    if let Some(names) = header_names(connector, key) {
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
        "Stored the key for {} in {}. Agents can use it right away.",
        connector.name,
        path.display()
    );
    if std::env::var_os(format!(
        "CRW_CONNECTORS__{}__API_KEY",
        slug.to_ascii_uppercase().replace('-', "_")
    ))
    .is_some()
    {
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
    fn connectors_cli_header_names_never_show_values() {
        let c = header_connector();
        assert_eq!(
            header_names(&c, r#"{"X-Auth-Email":"me@example.com","X-Auth-Key":"k1"}"#).unwrap(),
            vec!["X-Auth-Email", "X-Auth-Key"]
        );
        assert_eq!(header_names(&c, "plain").unwrap(), vec!["X-Auth-Key"]);
        assert!(placement_text(&c).contains("JSON object of header names to values"));
    }
}
