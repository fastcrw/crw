//! Cloud setup flow for CRW.

use crate::commands::setup::config_file::{self, ClientSection, UserConfig};
use crate::commands::setup::ui::{self, SetupError};
use console::style;
use dialoguer::{Input, Select};
use serde::Deserialize;

const API_BASE_URL: &str = "https://api.fastcrw.com";
const DASHBOARD_URL: &str = "https://fastcrw.com/dashboard";

/// Account info returned from `GET /v1/account/balance` (the SaaS balance
/// endpoint). Only the total-credits field is needed to show a friendly count;
/// unknown fields are ignored, and a missing value degrades to "validated"
/// without a number rather than a misleading "0 credits".
#[derive(Debug, Deserialize)]
struct AccountInfo {
    #[serde(rename = "totalCreditsAvailable")]
    total_credits_available: Option<i64>,
}

/// API key validation result.
#[derive(Debug)]
pub enum ApiKeyStatus {
    Valid { credits: i64 },
    Invalid,
    NetworkError(String),
}

/// Non-interactive cloud setup for `crw setup --api-key <key>` (and the
/// `curl … | CRW_API_KEY=… sh` installer pass-through). Validates the key
/// against the CRW API, persists it to `~/.config/crw/config.toml` pointed at
/// api.fastcrw.com, and prints a summary — no prompts, no LLM step, no
/// shell-rc question. This gives brew / curl / cargo the same one-command
/// cloud connect that `npx crw-mcp install --api-key` gives agents.
pub async fn run_with_key(api_key: String) -> Result<(), SetupError> {
    ui::print_section_header("CLOUD SETUP");

    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        return Err(SetupError::Other("API key is empty".to_string()));
    }

    print!("  ");
    match validate_api_key(&api_key).await {
        ApiKeyStatus::Valid { credits } => {
            if credits >= 0 {
                ui::print_success(&format!(
                    "API key validated ({} credits remaining)",
                    credits
                ));
            } else {
                ui::print_success("API key validated");
            }
            println!();
        }
        ApiKeyStatus::Invalid => {
            return Err(SetupError::Other(
                "Invalid API key — copy it exactly from https://fastcrw.com/dashboard".to_string(),
            ));
        }
        ApiKeyStatus::NetworkError(e) => {
            return Err(SetupError::Other(format!(
                "Could not reach fastcrw.com to validate the key ({e}) — check your connection and retry"
            )));
        }
    }

    finish_cloud_setup(&api_key)
}

/// Run the cloud setup flow.
pub async fn run() -> Result<(), SetupError> {
    ui::print_section_header("CLOUD SETUP");

    ui::print_step(1, 1, "Connect your CRW API key");

    println!("  Visit: {}", style(DASHBOARD_URL).cyan().underlined());
    println!();
    println!("  1. Sign up (GitHub/Google, takes 10 seconds)");
    println!("  2. Copy your API key from the dashboard");
    println!();

    let api_key = get_api_key().await?;
    finish_cloud_setup(&api_key)
}

/// Persist the only state cloud mode needs. LLM credentials are deliberately
/// absent: AI features ask for a provider when the user first invokes one.
/// This also keeps interactive and `--api-key` setup behavior identical after
/// validation — paste one key, save it, done.
fn finish_cloud_setup(api_key: &str) -> Result<(), SetupError> {
    let cfg_path = config_file::write_user_config(build_user_config(api_key))?;
    ui::print_success("Connected to CRW Cloud");
    ui::print_detail(&format!("Saved {}", cfg_path.display()));
    if cloud_env_conflicts(
        api_key,
        non_empty_env("CRW_API_URL").as_deref(),
        non_empty_env("CRW_API_KEY").as_deref(),
        non_empty_env("CRW_SEARCH_BACKEND_URL").as_deref(),
        non_empty_env("CRW_SEARXNG_URL").as_deref(),
    ) {
        println!();
        ui::print_warning("Existing CRW_* environment variables override this Cloud setup.");
        ui::print_detail(
            "Remove the old export or run `crw setup --reset-shell`, then open a new shell.",
        );
    }

    let quick_start = ["crw search \"rust tutorials\"  # Search with Cloud"];
    ui::print_completion_banner(&quick_start, &[]);

    Ok(())
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn cloud_env_conflicts(
    saved_api_key: &str,
    env_api_url: Option<&str>,
    env_api_key: Option<&str>,
    env_search_url: Option<&str>,
    legacy_search_url: Option<&str>,
) -> bool {
    env_api_url.is_some_and(|url| url.trim_end_matches('/') != API_BASE_URL)
        || env_api_key.is_some_and(|key| key != saved_api_key)
        || env_search_url.is_some()
        || legacy_search_url.is_some()
}

/// Get and validate API key from user.
async fn get_api_key() -> Result<String, SetupError> {
    loop {
        let api_key: String = Input::with_theme(&ui::select_style())
            .with_prompt("  Paste your API key")
            .validate_with(|input: &String| {
                if input.trim().is_empty() {
                    Err("API key cannot be empty")
                } else if !input.starts_with("fc-")
                    && !input.starts_with("sk-")
                    && !input.starts_with("crw_")
                {
                    Err("API key should start with 'fc-', 'sk-', or 'crw_'")
                } else {
                    Ok(())
                }
            })
            .interact_text()
            .map_err(ui::handle_dialoguer_error)?;

        let api_key = api_key.trim().to_string();

        // Validate API key
        print!("  ");
        match validate_api_key(&api_key).await {
            ApiKeyStatus::Valid { credits } => {
                if credits >= 0 {
                    ui::print_success(&format!(
                        "API key validated ({} credits remaining)",
                        credits
                    ));
                } else {
                    ui::print_success("API key validated");
                }
                println!();
                return Ok(api_key);
            }
            ApiKeyStatus::Invalid => {
                ui::print_error("Invalid API key");
                println!();
                println!("  The API rejected this key. Try these steps:");
                println!("  1. Check for extra spaces (copy exactly from dashboard)");
                println!("  2. Ensure key hasn't been revoked");
                println!();

                let choice = Select::with_theme(&ui::select_style())
                    .with_prompt("  What would you like to do?")
                    .items(["Try again", "Get a new key (opens browser)", "Cancel setup"])
                    .default(0)
                    .interact()
                    .map_err(ui::handle_dialoguer_error)?;

                match choice {
                    0 => continue,
                    1 => {
                        open_browser(DASHBOARD_URL);
                        continue;
                    }
                    2 => return Err(SetupError::Cancelled),
                    _ => unreachable!(),
                }
            }
            ApiKeyStatus::NetworkError(err) => {
                ui::print_warning(&format!("Could not verify API key: {}", err));
                println!();

                let choice = Select::with_theme(&ui::select_style())
                    .with_prompt("  What would you like to do?")
                    .items(["Retry verification", "Continue anyway (key not verified)"])
                    .default(0)
                    .interact()
                    .map_err(ui::handle_dialoguer_error)?;

                match choice {
                    0 => continue,
                    1 => {
                        ui::print_warning("Continuing with unverified API key");
                        return Ok(api_key);
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
}

/// Validate API key against the API.
async fn validate_api_key(key: &str) -> ApiKeyStatus {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => return ApiKeyStatus::NetworkError(e.to_string()),
    };

    let resp = match client
        .get(format!("{}/v1/account/balance", API_BASE_URL))
        .header("Authorization", format!("Bearer {}", key))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return ApiKeyStatus::NetworkError(e.to_string()),
    };

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return ApiKeyStatus::Invalid;
    }

    if !resp.status().is_success() {
        return ApiKeyStatus::NetworkError(format!("HTTP {}", resp.status()));
    }

    match resp.json::<AccountInfo>().await {
        // No count in the body ⇒ -1, which callers render as "validated" with
        // no number (never a misleading "0 credits").
        Ok(info) => ApiKeyStatus::Valid {
            credits: info.total_credits_available.unwrap_or(-1),
        },
        Err(_) => {
            // If we got a 200 but can't parse, assume it's valid
            ApiKeyStatus::Valid { credits: -1 }
        }
    }
}

/// Build the `UserConfig` we'll persist to `~/.config/crw/config.toml`.
///
/// Only sections the wizard actually touched are filled in. Anything else
/// (search, etc.) is left as `None` so a previous run's value survives the
/// merge in `config_file::write_user_config`.
fn build_user_config(api_key: &str) -> UserConfig {
    UserConfig {
        rest: Default::default(),
        connectors: None,
        client: Some(ClientSection {
            rest: Default::default(),
            api_url: Some(API_BASE_URL.to_string()),
            api_key: Some(api_key.to_string()),
            telemetry: None,
        }),
        search: None,
        extraction: None,
    }
}

/// Open URL in default browser.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", url])
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The core "connects to SaaS" contract of the non-interactive path: the
    // persisted config points the CLI at api.fastcrw.com with the caller's key
    // and no LLM leg. If this regresses, `crw setup --api-key` / the installer
    // pass-through would silently write a config that doesn't reach cloud.
    #[test]
    fn build_user_config_wires_cloud_with_key() {
        let cfg = build_user_config("crw_live_test_key");
        let client = cfg.client.expect("client section");
        assert_eq!(client.api_url.as_deref(), Some(API_BASE_URL));
        assert_eq!(client.api_key.as_deref(), Some("crw_live_test_key"));
        assert!(
            cfg.extraction.is_none(),
            "no LLM leg in non-interactive path"
        );
    }

    #[test]
    fn cloud_env_conflict_only_flags_values_that_change_routing() {
        assert!(!cloud_env_conflicts(
            "fc-same",
            Some("https://api.fastcrw.com/"),
            Some("fc-same"),
            None,
            None
        ));
        assert!(cloud_env_conflicts(
            "fc-new",
            None,
            Some("fc-old"),
            None,
            None
        ));
        assert!(cloud_env_conflicts(
            "fc-new",
            None,
            None,
            Some("http://127.0.0.1:8080"),
            None
        ));
        assert!(cloud_env_conflicts(
            "fc-new",
            None,
            None,
            None,
            Some("http://127.0.0.1:8080")
        ));
    }
}
