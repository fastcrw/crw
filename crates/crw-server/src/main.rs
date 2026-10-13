use clap::{Parser, Subcommand};
use crw_core::config::AppConfig;
use crw_server::state::AppState;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "crw-server", about = "CRW web scraper API server")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Download LightPanda and create a local config for JS rendering
    Setup,
}

#[tokio::main]
async fn main() {
    // If spawned as a PDF sandbox worker, handle it and exit BEFORE clap parses
    // args (the worker sentinel is not a valid subcommand).
    crw_crawl::pdf::run_sandbox_worker_if_invoked();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Setup) => {
            crw_server::setup::run_setup().await;
        }
        None => {
            run_server().await;
        }
    }
}

/// Filter used when `RUST_LOG` is unset (an explicit `RUST_LOG` always wins).
/// `html5ever` warns "foster parenting not implemented" once per token of a
/// mis-nested `<table>` page: 27,354 lines in 14 days of prod, 62% of all WARN
/// lines, with no engine signal in any of them.
const DEFAULT_LOG_FILTER: &str = "info,html5ever=error";

async fn run_server() {
    // Initialize tracing. ANSI colours only on a terminal: under docker the
    // escape codes were 30% of the log bytes and split tokens, so a multi-word
    // search such as `ERROR tower_http` never matched.
    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER)),
        )
        .init();

    // Eagerly register Prometheus metrics so alert rules see present series at boot.
    crw_core::metrics::init();

    // Load configuration.
    let config = match AppConfig::load() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to load configuration: {e}");
            std::process::exit(1);
        }
    };

    // Process-wide reserved-lane limits (PDF / extract / LLM) are installed by
    // `AppState::new` (below) so the `crw serve` / embedded CLI entry points get
    // them too — see `state.rs`. All idempotent (first-call-wins).

    let addr = format!("{}:{}", config.server.host, config.server.port);
    tracing::info!("Starting CRW on {addr}");
    tracing::info!("Renderer mode: {:?}", config.renderer.mode);
    tracing::info!(
        "Renderer render_js_default: {:?}",
        config.renderer.render_js_default
    );
    if let Some(lp) = &config.renderer.lightpanda {
        tracing::info!("Lightpanda CDP: {}", lp.ws_url);
    }
    if let Some(ch) = &config.renderer.chrome {
        tracing::info!("Chrome CDP: {}", ch.ws_url);
    }
    if std::env::var("CRW_CDP_URL").is_ok() {
        tracing::warn!(
            "CRW_CDP_URL is set but is only honored by `crw` (CLI). \
             In server/MCP mode use [renderer.lightpanda.ws_url] / [renderer.chrome.ws_url] \
             or CRW_RENDERER__LIGHTPANDA__WS_URL / CRW_RENDERER__CHROME__WS_URL."
        );
    }

    if config.extraction.llm.is_some() {
        tracing::info!("LLM structured extraction: enabled");
    }

    // Issue #90: make the search subsystem's configured state visible at boot.
    // Three states (disabled / enabled-but-unconfigured / enabled) otherwise
    // collapse to a single request-time error. The host is origin-sanitized so
    // a credentialed `search_backend_url` never reaches the logs.
    let (search_level, search_msg) = crw_server::diagnostics::search_startup_status(&config.search);
    match search_level {
        tracing::Level::WARN => tracing::warn!("{search_msg}"),
        _ => tracing::info!("{search_msg}"),
    }

    // Boot guard: when SaaS fronts opencore it sets `CRW_DISABLE_SERVER_LLM_KEY=1`
    // to prevent the most common ops mistake — leaving a server-wide key
    // configured behind the SaaS, which would leak the org's key to every
    // user. Refuse to boot if both are set.
    let disable_server_key = std::env::var("CRW_DISABLE_SERVER_LLM_KEY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if disable_server_key
        && config
            .extraction
            .llm
            .as_ref()
            .is_some_and(|c| !c.api_key.is_empty())
    {
        tracing::error!(
            "CRW_DISABLE_SERVER_LLM_KEY=1 but [extraction.llm].api_key is also configured. \
             This is forbidden in SaaS-fronted deploys (refusing to boot)."
        );
        std::process::exit(1);
    }

    // Optional boot guard (off by default): when `CRW_REQUIRE_MANAGED_MODEL_PREFIX`
    // is set, the configured LLM model must live in this project's own `crw-`
    // namespace. SaaS-fronted deploys enable it to catch a misconfigured model
    // before any request is served. Self-hosters leave it unset and can use any
    // model. Skipped when no LLM is configured.
    let require_managed_prefix = std::env::var("CRW_REQUIRE_MANAGED_MODEL_PREFIX")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if !managed_model_prefix_ok(
        require_managed_prefix,
        config.extraction.llm.as_ref().map(|c| c.model.as_str()),
    ) {
        tracing::error!(
            "[extraction.llm].model must use the `crw-` namespace when \
             CRW_REQUIRE_MANAGED_MODEL_PREFIX is set (check CRW_EXTRACTION__LLM__MODEL); \
             refusing to boot."
        );
        std::process::exit(1);
    }

    let state = match AppState::new(config) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to build application state: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!(
        "JS renderers in fallback order: {:?}",
        state.renderer.js_renderer_names()
    );
    if state.renderer.js_renderer_names().is_empty() {
        tracing::warn!("No CDP renderer active — JS rendering disabled");
    }

    // Issue #90: one-shot, non-fatal reachability probe so a misconfigured or
    // down SearXNG is *spoken* at boot instead of failing silently on the first
    // search. Bundled-compose users are already gated by `depends_on:
    // searxng condition: service_healthy`, so this mainly helps operators who
    // point `CRW_SEARCH__SEARXNG_URL` at an external host. The origin is
    // sanitized; the probe hits the origin's `/healthz` (the same path the
    // compose healthcheck uses) and is bounded by its own short timeout —
    // never `connect_timeout`, which wouldn't bound a stalled response.
    if state.config.search.enabled
        && let Some(raw_url) = state
            .config
            .search
            .resolve_backend_url()
            .map(str::to_string)
    {
        let origin = crw_server::diagnostics::sanitize_url_origin(&raw_url);
        tokio::spawn(async move {
            let probe = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build();
            match probe {
                Ok(client) => {
                    let healthz = format!("{origin}/healthz");
                    match client.get(&healthz).send().await {
                        Ok(resp) if resp.status().is_success() => {
                            tracing::info!("search: SearXNG reachable at {origin}");
                        }
                        Ok(resp) => {
                            tracing::warn!(
                                "search: SearXNG at {origin} answered /healthz with {} — \
                                 search calls may fail until it is healthy",
                                resp.status()
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                "search: configured host {origin} UNREACHABLE at startup \
                                 ({}) — search calls will fail until it resolves",
                                e.without_url()
                            );
                        }
                    }
                }
                Err(e) => tracing::warn!("search: could not build startup probe client: {e}"),
            }
        });
    }

    // Issue #35 transparency: when auto-extension widens the implicit deadline
    // beyond the operator's `deadline_ms_default`, log the effective values so
    // operators can correlate "request took longer than my SLO" against the
    // ladder budget instead of suspecting a bug.
    let baseline_default_ms = state.config.request.deadline_ms_default;
    let ladder_min_ms = state.config.renderer.min_deadline_for_full_ladder_ms();
    let effective_default_ms = state.config.effective_deadline_ms(None, None);
    let baseline_outer_secs = state.config.server.request_timeout_secs;
    let effective_outer_secs = state.config.effective_request_timeout_secs();
    if state.config.request.auto_extend_deadline_for_ladder
        && effective_default_ms > baseline_default_ms
    {
        tracing::info!(
            deadline_ms_default = baseline_default_ms,
            ladder_min_ms,
            effective_default_ms,
            outer_timeout_secs_baseline = baseline_outer_secs,
            outer_timeout_secs_effective = effective_outer_secs,
            "request.auto_extend_deadline_for_ladder is on; default request \
             deadline auto-raised so the configured renderer ladder \
             (http+lightpanda+chrome+overhead) can run uncrushed. Set \
             request.auto_extend_deadline_for_ladder = false to enforce the \
             baseline cap."
        );
    }

    // Capture handles before `state` is consumed by `create_app` so we can
    // drain the chrome browser-context pool after the HTTP server quiesces.
    let renderer = std::sync::Arc::clone(&state.renderer);
    let pool_drain =
        std::time::Duration::from_secs(state.config.renderer.chrome_pool.shutdown_drain_secs);
    let api_keys_empty = state.config.auth.api_keys.is_empty();

    let app = crw_server::app::create_app(state);

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind to {addr}: {e}");
            std::process::exit(1);
        }
    };

    tracing::info!("CRW ready at http://{addr}");
    if api_keys_empty {
        tracing::warn!(
            "no [auth] api_keys configured; the API is UNAUTHENTICATED and open to \
             anyone who can reach it. Set [auth] api_keys or CRW_AUTH__API_KEYS to secure it."
        );
    }

    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());

    if let Err(e) = server.await {
        tracing::error!("Server error: {e}");
        std::process::exit(1);
    }

    // HTTP layer is quiesced; now drain the chrome pool (no-op when disabled).
    renderer.shutdown_chrome_pool(pool_drain).await;

    tracing::info!("Server shut down gracefully");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("Received Ctrl+C, shutting down..."),
        _ = terminate => tracing::info!("Received SIGTERM, shutting down..."),
    }
}

/// Decide whether the configured LLM model satisfies the optional managed-prefix
/// guard. Returns `true` (boot allowed) when the guard is off, when no LLM is
/// configured, or when the model uses the `crw-` namespace. Returns `false`
/// (refuse boot) only when the guard is on AND a non-`crw-` model is configured.
fn managed_model_prefix_ok(require: bool, model: Option<&str>) -> bool {
    if !require {
        return true;
    }
    match model {
        None => true,
        Some(m) => m.starts_with("crw-"),
    }
}

#[cfg(test)]
mod tests {
    use super::managed_model_prefix_ok;

    #[test]
    fn guard_off_allows_any_model() {
        assert!(managed_model_prefix_ok(false, Some("gpt-4o")));
        assert!(managed_model_prefix_ok(false, Some("crw-managed")));
        assert!(managed_model_prefix_ok(false, None));
    }

    #[test]
    fn guard_on_allows_crw_prefixed_model() {
        assert!(managed_model_prefix_ok(true, Some("crw-managed")));
    }

    #[test]
    fn guard_on_rejects_non_crw_model() {
        assert!(!managed_model_prefix_ok(true, Some("gpt-4o")));
    }

    #[test]
    fn guard_on_skips_when_no_model_configured() {
        assert!(managed_model_prefix_ok(true, None));
    }

    /// `EnvFilter::new` drops a directive it cannot parse with only a warning,
    /// so a typo here would silently bring the noise back. `try_new` fails loudly.
    #[test]
    fn default_log_filter_parses_and_quiets_html5ever() {
        let filter = tracing_subscriber::EnvFilter::try_new(super::DEFAULT_LOG_FILTER)
            .expect("the default filter must parse");
        let shown = filter.to_string();
        assert!(shown.contains("html5ever=error"), "{shown}");
    }
}
