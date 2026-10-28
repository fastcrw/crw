//! `crw connectors`: data sources that run on this machine with the user's own
//! key. `list` shows them and whether a key is stored; `set-key` stores one,
//! after the user approves the origins and the placement it will be sent with.
//!
//! The key is read from a no-echo prompt or from stdin, never from argv, so it
//! does not land in shell history or `ps` output.

use super::setup::config_file::{UserConfig, write_user_config};
use crate::teardown::CmdError;
use clap::{Args, Subcommand};
use crw_core::connectors::DataToolContext;
use crw_core::connectors::keys::{self, ConnectorEntry};
use crw_core::connectors::manifest::{Connector, Manifest};
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
        /// Provider id, as shown by `crw connectors list`
        slug: String,
        /// Approve the listed origins without asking (required when the key comes from stdin)
        #[arg(long, short)]
        yes: bool,
    },
}

pub async fn run(args: ConnectorsArgs) -> Result<(), CmdError> {
    let manifest = fetch_manifest().await?;
    match args.command {
        ConnectorsCommand::List => {
            list(&manifest);
            Ok(())
        }
        ConnectorsCommand::SetKey { slug, yes } => set_key(&manifest, &slug, yes),
    }
}

fn fail(msg: impl Into<String>) -> CmdError {
    CmdError {
        code: 1,
        msg: Some(format!("error: {}", msg.into())),
    }
}

async fn fetch_manifest() -> Result<Arc<Manifest>, CmdError> {
    let client = crw_core::AppConfig::load()
        .map(|c| c.client)
        .unwrap_or_default();
    let api_url = client
        .api_url
        .unwrap_or_else(|| DEFAULT_CLOUD_API_URL.to_string());
    let has_key = client.api_key.is_some();
    if std::io::stderr().is_terminal() {
        eprintln!("Loading data sources from {api_url} ...");
    }
    let ctx = DataToolContext::new(api_url, client.api_key, false);
    ctx.manifest().await.map_err(|e| {
        let hint = if has_key {
            ""
        } else {
            "\nhint: no fastCRW API key is configured; run `crw setup` first"
        };
        fail(format!("{e}{hint}"))
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
        eprintln!("\nStore a key: crw connectors set-key <provider>");
    }
}

/// Where the key goes, in words.
fn placement_text(c: &Connector) -> String {
    let param = c.auth.param.as_deref().unwrap_or("").trim();
    match c.auth.kind.as_str() {
        "query" => format!("as the query parameter \"{param}\""),
        "header" => format!("in the \"{param}\" header"),
        _ => "as the HTTP Basic username".to_string(),
    }
}

fn set_key(manifest: &Manifest, slug: &str, yes: bool) -> Result<(), CmdError> {
    let Some(connector) = manifest.connector(slug) else {
        return Err(fail(format!(
            "no data source named \"{slug}\" runs on this machine. See `crw connectors list`."
        )));
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
            return Err(fail(
                "the key comes from stdin, so approve the origins above with --yes",
            ));
        }
        let approved = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("Approve these origins for your key?")
            .default(false)
            .interact()
            .map_err(|_| fail("cancelled; nothing stored"))?;
        if !approved {
            eprintln!("Not approved. Nothing stored.");
            return Ok(());
        }
    }

    let key = if interactive {
        Password::with_theme(&ColorfulTheme::default())
            .with_prompt(format!("API key for {}", connector.name))
            .interact()
            .map_err(|_| fail("cancelled; nothing stored"))?
    } else {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| fail(format!("could not read the key from stdin: {e}")))?;
        buf
    };
    let key = key.trim();
    if key.is_empty() {
        return Err(fail("no key given; nothing stored"));
    }

    let entry = ConnectorEntry {
        api_key: Some(key.to_string()),
        origins,
        auth: Some(connector.auth.placement()),
    };
    let path = write_user_config(UserConfig {
        connectors: Some(BTreeMap::from([(slug.to_string(), entry)])),
        ..Default::default()
    })
    .map_err(fail)?;
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
