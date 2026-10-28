//! Local setup flow for CRW.

use crate::commands::setup::browser::{self, BrowserEngine};
use crate::commands::setup::config_file::{self, SearchSection, UserConfig};
use crate::commands::setup::docker::{self, DockerStatus};
use crate::commands::setup::searxng;
use crate::commands::setup::ui::{self, SetupError, SummaryItem};
use dialoguer::Select;

/// Run the local setup flow.
///
/// `non_interactive` skips every dialoguer prompt and picks the
/// least-surprising default at each step instead: no Docker check (it would
/// only feed the search-backend prompt this path never shows, and `docker
/// info` can hang against a stuck daemon), no auto-install of a search
/// backend. `~/.config/crw/config.toml` is still written the same way as the
/// interactive path. LLM setup is intentionally demand-driven by the first
/// `--summary` or `--extract` invocation, not part of local setup.
pub async fn run(non_interactive: bool) -> Result<(), SetupError> {
    ui::print_section_header("LOCAL SETUP");

    println!("  Basic local scraping already works without setup.");
    println!("  Add only the optional browser and search capabilities you need.");
    println!();

    let platform = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    ui::print_success(&format!("Platform: {} {}", platform, arch));
    println!();

    // Step 1: Browser engine
    ui::print_step(1, 2, "Browser Engine (for JS rendering)");

    println!("  Plain pages need no browser. JavaScript-heavy sites can use");
    println!("  Chrome/Chromium or the experimental LightPanda renderer.");
    println!();

    let (browser_engine, browser_installed) = setup_browser(non_interactive).await?;

    println!();

    // Step 2: Search engine
    ui::print_step(2, 2, "Search Engine (for web search)");

    println!("  To use `crw search` without Cloud, CRW can run a private");
    println!("  local search backend in Docker.");
    println!();

    let search_backend_url = if non_interactive {
        ui::print_info("Skipping search backend setup (--non-interactive).");
        None
    } else {
        prompt_searxng_setup().await?
    };

    println!();

    // Always persist canonical state to ~/.config/crw/config.toml.
    let cfg_path =
        config_file::write_local_user_config(build_user_config(search_backend_url.as_deref()))?;
    ui::print_success(&format!("Saved {}", cfg_path.display()));
    println!();

    let cloud_env_override = cloud_env_override_present();
    if cloud_env_override {
        ui::print_warning("CRW_API_URL or CRW_API_KEY is still set in this shell.");
        ui::print_detail("Environment variables override Local config.");
        ui::print_detail(
            "Remove the export or run `crw setup --reset-shell`, then open a new shell.",
        );
        println!();
    }

    let (browser_status, browser_ok) = browser_status_label(browser_engine, browser_installed);

    let summary_items = vec![
        SummaryItem::new("Browser Engine", browser_status, browser_ok),
        SummaryItem::new(
            "Search Engine",
            search_backend_url.as_deref().unwrap_or("Not configured"),
            search_backend_url.is_some(),
        ),
    ];
    ui::print_summary("Configuration Summary", &summary_items);

    let mut quick_start = vec!["crw example.com              # Scrape (HTTP)"];

    if browser_installed {
        quick_start.push("crw example.com --js         # Scrape with JavaScript");
    }

    if search_backend_url.is_some() && !cloud_env_override {
        quick_start.push("crw search \"rust tutorials\"  # Web search");
    }

    ui::print_completion_banner(&quick_start, &["Documentation: https://docs.fastcrw.com"]);

    Ok(())
}

fn cloud_env_override_present() -> bool {
    ["CRW_API_URL", "CRW_API_KEY"].iter().any(|name| {
        std::env::var(name)
            .ok()
            .is_some_and(|value| !value.is_empty())
    })
}

/// Extract version number from docker version string.
fn extract_version(full: &str) -> &str {
    // "Docker version 24.0.5, build ..." -> "24.0.5"
    full.split_whitespace()
        .nth(2)
        .map(|s| s.trim_end_matches(','))
        .unwrap_or(full)
}

/// Handle Docker not running scenario.
async fn handle_docker_not_running() -> Result<bool, SetupError> {
    println!();
    println!("  Please start Docker Desktop and try again.");
    println!();

    let choice = Select::with_theme(&ui::select_style())
        .with_prompt("  What would you like to do?")
        .items([
            "Retry (I just started Docker)",
            "Continue without search (skip search backend)",
            "Exit",
        ])
        .default(0)
        .interact_opt()
        .map_err(ui::handle_dialoguer_error)?
        .ok_or(SetupError::Cancelled)?;

    match choice {
        0 => {
            // Retry
            let status = docker::check_docker().await;
            if status.is_ready() {
                ui::print_success("Docker is now running");
                Ok(true)
            } else {
                ui::print_error("Docker still not running");
                Ok(false)
            }
        }
        1 => Ok(false),
        2 => Err(SetupError::Cancelled),
        _ => unreachable!(),
    }
}

/// Handle Docker not found scenario.
async fn handle_docker_not_found() -> Result<bool, SetupError> {
    let instructions = docker::docker_install_instructions();
    let mut lines = vec![
        "Docker is required for local search setup",
        "",
        "Docker runs the search engine in a container.",
        "Without it, you can still scrape but not search.",
        "",
        "Install Docker:",
    ];
    for inst in &instructions {
        lines.push(inst);
    }

    ui::print_info_box(&lines);

    let choice = Select::with_theme(&ui::select_style())
        .with_prompt("  What would you like to do?")
        .items([
            "Continue without Docker (skip search backend)",
            "Exit and install Docker first",
        ])
        .default(0)
        .interact_opt()
        .map_err(ui::handle_dialoguer_error)?
        .ok_or(SetupError::Cancelled)?;

    match choice {
        0 => Ok(false),
        1 => Err("Please install Docker and run 'crw setup' again.".into()),
        _ => unreachable!(),
    }
}

/// Return the (label, ok) pair shown for the Browser Engine summary row.
///
/// Pure — no I/O — so the displayed states stay unit-testable.
fn browser_status_label(engine: BrowserEngine, installed: bool) -> (&'static str, bool) {
    match (engine, installed) {
        (BrowserEngine::Chrome, true) => ("Chrome (detected)", true),
        (BrowserEngine::LightPanda, true) => ("LightPanda (installed, experimental)", true),
        (BrowserEngine::Chrome, false) => ("Chrome (unavailable)", false),
        (BrowserEngine::LightPanda, false) => ("LightPanda (install failed)", false),
        (BrowserEngine::None, _) => ("Not installed (HTTP available)", false),
    }
}

/// Detect an existing browser without asking. Only offer a download when no
/// browser exists, because the runtime discovers browsers automatically and
/// does not persist a user-selected preference.
async fn setup_browser(non_interactive: bool) -> Result<(BrowserEngine, bool), SetupError> {
    if let Some(path) = browser::detect_chrome() {
        ui::print_success(&format!("Chrome detected at {}", path.display()));
        return Ok((BrowserEngine::Chrome, true));
    }
    if let Some(path) = browser::detect_lightpanda() {
        ui::print_success(&format!("LightPanda detected at {}", path.display()));
        ui::print_detail("Experimental; install Chrome if a site times out.");
        return Ok((BrowserEngine::LightPanda, true));
    }

    if non_interactive {
        ui::print_info("No browser detected; continuing with basic HTTP scraping.");
        return Ok((BrowserEngine::None, false));
    }
    if browser::get_platform_info().is_none() {
        ui::print_warning("No supported browser was detected.");
        ui::print_detail("Install Chrome/Chromium to add JavaScript rendering.");
        return Ok((BrowserEngine::None, false));
    }

    let choice = Select::with_theme(&ui::select_style())
        .with_prompt("  No browser detected. Install LightPanda?")
        .items([
            "Install LightPanda (experimental, ~50MB)",
            "Not now — keep basic HTTP scraping",
        ])
        .default(1)
        .interact_opt()
        .map_err(ui::handle_dialoguer_error)?
        .ok_or(SetupError::Cancelled)?;

    if choice == 1 {
        ui::print_info("Skipping browser installation");
        return Ok((BrowserEngine::None, false));
    }

    ui::print_warning("LightPanda is experimental and may timeout on some sites.");
    ui::print_info("Downloading LightPanda...");
    let installed = match browser::download_lightpanda().await {
        Ok(_) => true,
        Err(error) => {
            ui::print_error(&format!("Download failed: {error}"));
            handle_download_failure().await?
        }
    };
    Ok((BrowserEngine::LightPanda, installed))
}

/// Handle download failure.
async fn handle_download_failure() -> Result<bool, SetupError> {
    let choice = Select::with_theme(&ui::select_style())
        .with_prompt("  What would you like to do?")
        .items(["Retry download", "Continue with basic HTTP scraping"])
        .default(0)
        .interact_opt()
        .map_err(ui::handle_dialoguer_error)?
        .ok_or(SetupError::Cancelled)?;

    match choice {
        0 => {
            // Retry
            match browser::download_lightpanda().await {
                Ok(_) => Ok(true),
                Err(e) => {
                    ui::print_error(&format!("Download failed again: {}", e));
                    Ok(false)
                }
            }
        }
        1 => Ok(false),
        _ => unreachable!(),
    }
}

/// Prompt for SearXNG setup.
async fn prompt_searxng_setup() -> Result<Option<String>, SetupError> {
    let status = searxng::check_status();

    // If already running, just return the URL
    if let searxng::SearxngStatus::Running { url } = &status {
        ui::print_success(&format!("Search backend already running at {}", url));
        return Ok(Some(url.clone()));
    }

    let items = vec![
        "Yes, using Docker (recommended)\n      • Auto-managed container\n      • ~500MB disk space\n      • Starts automatically when needed",
        "No, I'll set it up myself\n      • Manual setup required\n      • Set CRW_SEARCH_BACKEND_URL to your instance",
        "Skip (no search feature)\n      • crw search command won't work\n      • Scraping still works fine",
    ];

    let choice = Select::with_theme(&ui::select_style())
        .with_prompt("  Set up a search backend for web search?")
        .items(&items)
        .default(0)
        .interact_opt()
        .map_err(ui::handle_dialoguer_error)?
        .ok_or(SetupError::Cancelled)?;

    match choice {
        0 => {
            if !ensure_docker_ready().await? {
                return Ok(None);
            }
            searxng::pull_image().await?;
            let url = searxng::start_container().await?;
            Ok(Some(url))
        }
        1 => {
            ui::print_info(
                "Set up a search backend and export CRW_SEARCH_BACKEND_URL when it is ready",
            );
            Ok(None)
        }
        2 => {
            ui::print_info("Skipping search backend setup");
            Ok(None)
        }
        _ => unreachable!(),
    }
}

/// Check Docker only after the user chooses local search. Browser-only users
/// never need to install, start, or troubleshoot Docker.
async fn ensure_docker_ready() -> Result<bool, SetupError> {
    match docker::check_docker().await {
        DockerStatus::Running { version } => {
            ui::print_success(&format!("Docker: found ({})", extract_version(&version)));
            if let Some(disk) = docker::get_available_disk_space() {
                ui::print_detail(&format!("Disk space: {}GB available", disk));
            }
            Ok(true)
        }
        DockerStatus::NotRunning { version } => {
            ui::print_error(&format!(
                "Docker: found but not running ({})",
                extract_version(&version)
            ));
            handle_docker_not_running().await
        }
        DockerStatus::NotFound => {
            ui::print_error("Docker: not found");
            handle_docker_not_found().await
        }
    }
}

/// Build the `UserConfig` for `~/.config/crw/config.toml`. Only fills in
/// sections setup actually touched; everything else stays `None` so
/// `merge_config` preserves prior values across re-runs.
fn build_user_config(search_backend_url: Option<&str>) -> UserConfig {
    UserConfig {
        rest: Default::default(),
        connectors: None,
        client: None,
        search: search_backend_url.map(|url| SearchSection {
            rest: Default::default(),
            search_backend_url: Some(url.to_string()),
        }),
        extraction: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_setup_leaves_llm_configuration_demand_driven() {
        let cfg = build_user_config(Some("http://127.0.0.1:8080"));
        assert!(cfg.extraction.is_none());
        assert_eq!(
            cfg.search
                .and_then(|search| search.search_backend_url)
                .as_deref(),
            Some("http://127.0.0.1:8080")
        );
    }

    // ---- browser_status_label -----------------------------------------------

    #[test]
    fn status_chrome_detected() {
        assert_eq!(
            browser_status_label(BrowserEngine::Chrome, true),
            ("Chrome (detected)", true)
        );
    }

    #[test]
    fn status_lightpanda_installed() {
        assert_eq!(
            browser_status_label(BrowserEngine::LightPanda, true),
            ("LightPanda (installed, experimental)", true)
        );
    }

    #[test]
    fn status_chrome_install_failed_does_not_advertise_other_browser() {
        // Regression: previously masked install failure as "Chrome (available)".
        let (label, ok) = browser_status_label(BrowserEngine::Chrome, false);
        assert!(label.contains("unavailable"));
        assert!(!ok);
    }

    #[test]
    fn status_lightpanda_install_failed_reports_failure() {
        let (label, ok) = browser_status_label(BrowserEngine::LightPanda, false);
        assert!(label.contains("install failed"));
        assert!(!ok);
    }

    #[test]
    fn status_without_browser_keeps_http_available() {
        assert_eq!(
            browser_status_label(BrowserEngine::None, false),
            ("Not installed (HTTP available)", false)
        );
    }
}
