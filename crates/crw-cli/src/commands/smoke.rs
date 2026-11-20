//! Smoke subcommand: deterministic, offline-by-default sanity checks across
//! the cli/server/mcp surfaces.
//!
//! Bare `crw smoke` is exactly `crw smoke --offline --surface cli`: it never
//! touches the network and can never spend hosted credits. It resolves
//! config and the capability snapshot through [`super::diag`], the same
//! module `crw doctor` uses, so the two commands can't drift.
//!
//! Exit codes match `crw doctor`: 0 when everything passed (warnings are
//! informational), 1 when a check failed, 2 on a usage/config ambiguity or a
//! refused billable run.

use super::diag::{self, CheckResult, Target};
use crate::teardown::CmdError;
use clap::{Args, ValueEnum};
use crw_core::config::AppConfig;
use std::time::Duration;

#[derive(Clone, Copy, ValueEnum)]
pub enum Surface {
    Cli,
    Server,
    Mcp,
    All,
}

#[derive(Args)]
pub struct SmokeArgs {
    /// Run against local fixtures only. Deterministic, no network, never
    /// billable. This is the default when neither this nor `--live` is
    /// given.
    #[arg(long, conflicts_with = "live")]
    pub offline: bool,

    /// Run a real network smoke test against this URL instead of the
    /// offline fixture.
    #[arg(long, value_name = "URL", conflicts_with = "offline")]
    pub live: Option<String>,

    /// Which surface(s) to smoke-test.
    #[arg(long, value_enum, default_value = "cli")]
    pub surface: Surface,

    /// Allow a smoke test that would spend hosted credits. Only relevant to
    /// `--live` when the resolved config points at the managed fastCRW API:
    /// default smoke never spends credits, and refuses to without this flag.
    #[arg(long)]
    pub allow_billable: bool,

    /// Emit machine-readable JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}

/// Kept inside the crate, not in the workspace `tests/fixtures`: `smoke` is a
/// shipped subcommand, so its fixture has to travel in the published `.crate`
/// tarball. `tests/journeys.rs` serves this same file from disk.
const FIXTURE_HTML: &str = include_str!("fixtures/simple.html");

pub async fn run(args: SmokeArgs) -> Result<(), CmdError> {
    let config = match AppConfig::load() {
        Ok(config) => config,
        Err(error) => {
            let message = format!("config failed to parse: {error}");
            if args.json {
                let body = serde_json::json!({ "error": message, "exitCode": 2 });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&body).expect("error body always serializes")
                );
            } else {
                eprintln!("error: {message}");
                eprintln!("hint: fix the config syntax, then run `crw doctor`");
            }
            return Err(CmdError::code_only(2));
        }
    };

    // Same resolver `crw doctor` uses: smoke never takes its own --target,
    // it just diagnoses whatever the config would resolve to.
    let target = match diag::resolve_target(&config, None) {
        Ok(t) => t,
        Err(msg) => {
            if args.json {
                let body = serde_json::json!({ "error": msg, "exitCode": 2 });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&body).expect("error body always serializes")
                );
            } else {
                eprintln!("error: {msg}");
                eprintln!("hint: run `crw doctor` first to see the exact ambiguity");
            }
            return Err(CmdError::code_only(2));
        }
    };

    let managed_target = config
        .client
        .api_url
        .as_deref()
        .is_some_and(diag::is_managed_api_url);
    if args.live.is_some() && managed_target && !args.allow_billable {
        let msg = "--live against a cloud-resolved config would spend hosted credits; \
             pass --allow-billable to proceed";
        if args.json {
            let body = serde_json::json!({ "error": msg, "exitCode": 2 });
            println!(
                "{}",
                serde_json::to_string_pretty(&body).expect("error body always serializes")
            );
        } else {
            eprintln!("error: {msg}");
        }
        return Err(CmdError::code_only(2));
    }

    let surfaces = match args.surface {
        Surface::All => vec![Surface::Cli, Surface::Server, Surface::Mcp],
        other => vec![other],
    };

    let mut checks = Vec::with_capacity(surfaces.len());
    for surface in surfaces {
        checks.push(match surface {
            Surface::Cli => cli_surface_check(args.live.as_deref(), &config, target).await,
            Surface::Server => server_surface_check(args.live.as_deref(), &config, target).await,
            Surface::Mcp => mcp_surface_check(args.live.as_deref(), &config, target).await,
            Surface::All => unreachable!("expanded above"),
        });
    }

    let exit_code = diag::print_report("crw smoke", target, &checks, args.json);
    if exit_code == 0 {
        Ok(())
    } else {
        Err(CmdError::code_only(exit_code))
    }
}

/// Serves the embedded fixture over plain HTTP to every connection accepted,
/// on an ephemeral loopback port. The fixture is compiled INTO the binary
/// (`include_str!`) rather than read from disk at runtime, so an installed
/// `crw` works identically to a checkout build.
///
/// ponytail: hand-rolled instead of a real HTTP server crate, a GET arrives
/// in one TCP read on loopback, and this only ever serves one canned
/// response. Mirrors `tests/journeys.rs`'s fixture server.
fn spawn_fixture_server() -> std::io::Result<String> {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                FIXTURE_HTML.len(),
                FIXTURE_HTML
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    Ok(format!("http://{addr}"))
}

/// `--live` against a `Cloud`-resolved config must actually exercise the
/// hosted API (that's the whole point of gating it behind
/// `--allow-billable`) rather than silently falling back to the local
/// embedded engine, which would spend no credits and prove nothing about
/// the cloud path.
async fn cli_surface_check(
    live_url: Option<&str>,
    config: &AppConfig,
    target: Target,
) -> CheckResult {
    if let Some(url) = live_url
        && target == Target::Cloud
    {
        return remote_scrape_check("smoke.cli.scrape", url, config).await;
    }
    local_scrape_check(live_url).await
}

async fn remote_scrape_check(check_id: &'static str, url: &str, config: &AppConfig) -> CheckResult {
    let Some(api_url) = config.client.api_url.as_deref() else {
        return CheckResult::fail(
            check_id,
            "resolved to a remote API but [client].api_url is unset",
            "set CRW_API_URL or run `crw setup --cloud`",
        );
    };
    let redacted_api = diag::redact_url(api_url);
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return CheckResult::fail(
                check_id,
                format!("could not build an HTTP client: {e}"),
                "check local TLS/cert setup",
            );
        }
    };
    let mut req = client
        .post(format!("{}/v1/scrape", api_url.trim_end_matches('/')))
        .json(&serde_json::json!({ "url": url }));
    if let Some(key) = &config.client.api_key {
        req = req.bearer_auth(key);
    }
    match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            let suffix = if diag::is_managed_api_url(api_url) {
                " (billable)"
            } else {
                ""
            };
            CheckResult::pass(check_id, format!("{redacted_api} scraped {url}{suffix}"))
        }
        Ok(resp) => CheckResult::fail(
            check_id,
            format!("{redacted_api} responded {} for {url}", resp.status()),
            "check [client].api_key and the URL, or https://fastcrw.com/dashboard for an outage",
        ),
        Err(e) => CheckResult::fail(
            check_id,
            format!("{redacted_api} unreachable: {e}"),
            "check your network and [client].api_url",
        ),
    }
}

async fn local_scrape_check(live_url: Option<&str>) -> CheckResult {
    use crw_core::config::{ExtractionConfig, RendererConfig, RendererMode, StealthConfig};
    use crw_core::types::ScrapeRequest;
    use crw_crawl::single::scrape_url;
    use crw_renderer::FallbackRenderer;

    let target_url = match live_url {
        Some(u) => u.to_string(),
        None => match spawn_fixture_server() {
            Ok(base) => base,
            Err(e) => {
                return CheckResult::fail(
                    "smoke.cli.scrape",
                    format!("could not start the local fixture server: {e}"),
                    "retry; this only binds a loopback port",
                );
            }
        },
    };

    let http_cfg = RendererConfig {
        mode: RendererMode::None,
        http_timeout_ms: Some(5_000),
        ..Default::default()
    };
    let renderer = match FallbackRenderer::new(
        &http_cfg,
        concat!("crw-smoke/", env!("CARGO_PKG_VERSION")),
        None,
        &StealthConfig::default(),
    ) {
        Ok(r) => std::sync::Arc::new(r),
        Err(e) => {
            return CheckResult::fail(
                "smoke.cli.scrape",
                format!("could not build the HTTP renderer: {e}"),
                "check crawler/renderer config in config.toml",
            );
        }
    };

    let req = ScrapeRequest {
        url: target_url.clone(),
        ..Default::default()
    };
    let deadline = crw_core::Deadline::from_request_ms(5_000);
    match scrape_url(
        &req,
        &renderer,
        None,
        &ExtractionConfig::default(),
        "crw-smoke",
        false,
        None,
        deadline,
    )
    .await
    {
        Ok(data) => {
            let len = data.markdown.as_deref().map(str::len).unwrap_or(0);
            if len > 0 {
                CheckResult::pass(
                    "smoke.cli.scrape",
                    format!("scraped {target_url} ({len} chars markdown)"),
                )
            } else {
                CheckResult::fail(
                    "smoke.cli.scrape",
                    format!("scraped {target_url} but markdown came back empty"),
                    "check the extraction path for a regression",
                )
            }
        }
        Err(e) => CheckResult::fail(
            "smoke.cli.scrape",
            format!("scrape of {target_url} failed: {e}"),
            "check network/renderer config, or file a bug if this is the offline fixture",
        ),
    }
}

async fn server_surface_check(
    live_url: Option<&str>,
    config: &AppConfig,
    target: Target,
) -> CheckResult {
    if let Some(url) = live_url
        && target == Target::Cloud
    {
        return remote_scrape_check("smoke.server.scrape", url, config).await;
    }

    #[cfg(feature = "mcp-embedded")]
    {
        let target_url = match live_url {
            Some(url) => url.to_string(),
            None => match spawn_fixture_server() {
                Ok(url) => url,
                Err(error) => {
                    return CheckResult::fail(
                        "smoke.server.scrape",
                        format!("could not start the local fixture server: {error}"),
                        "retry; this only binds a loopback port",
                    );
                }
            },
        };
        let mut config = AppConfig::default();
        config.renderer.mode = crw_core::config::RendererMode::None;
        config.search.enabled = false;
        config.server.host = "127.0.0.1".to_string();
        config.server.port = 0;

        let state = match crw_server::state::AppState::new(config) {
            Ok(s) => s,
            Err(e) => {
                return CheckResult::fail(
                    "smoke.server.boot",
                    format!("could not build server state: {e}"),
                    "check server/renderer config",
                );
            }
        };
        let app = crw_server::app::create_app(state);
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(l) => l,
            Err(e) => {
                return CheckResult::fail(
                    "smoke.server.boot",
                    format!("could not bind a loopback port: {e}"),
                    "check local network permissions",
                );
            }
        };
        let addr = match listener.local_addr() {
            Ok(a) => a,
            Err(e) => {
                return CheckResult::fail(
                    "smoke.server.boot",
                    format!("could not read the bound loopback address: {e}"),
                    "retry",
                );
            }
        };
        let serve_task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                serve_task.abort();
                return CheckResult::fail(
                    "smoke.server.boot",
                    format!("could not build an HTTP client: {e}"),
                    "check local TLS/cert setup",
                );
            }
        };
        let result = client
            .post(format!("http://{addr}/v1/scrape"))
            .json(&serde_json::json!({ "url": &target_url }))
            .send()
            .await;
        serve_task.abort();

        match result {
            Ok(resp) if resp.status().is_success() => CheckResult::pass(
                "smoke.server.scrape",
                format!(
                    "router boots and /v1/scrape fetched {target_url} ({})",
                    resp.status()
                ),
            ),
            Ok(resp) => CheckResult::fail(
                "smoke.server.scrape",
                format!("/v1/scrape responded {} for {target_url}", resp.status()),
                "check crw-server routing/middleware",
            ),
            Err(e) => CheckResult::fail(
                "smoke.server.scrape",
                format!("/v1/scrape failed for {target_url}: {e}"),
                "check crw-server routing/middleware",
            ),
        }
    }
    #[cfg(not(feature = "mcp-embedded"))]
    {
        CheckResult::skip(
            "smoke.server.boot",
            "this binary was built without the embedded server (--no-default-features)",
        )
    }
}

async fn mcp_surface_check(
    live_url: Option<&str>,
    config: &AppConfig,
    target: Target,
) -> CheckResult {
    if let Some(url) = live_url {
        return mcp_live_scrape_check(url, config, target).await;
    }

    let search_available = config.search.enabled && config.search.resolve_backend_url().is_some();
    let req = crw_core::mcp::JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(serde_json::json!(1)),
        method: "tools/list".to_string(),
        params: serde_json::json!({}),
    };
    match crw_core::mcp::handle_protocol_method(
        "crw-mcp",
        env!("CARGO_PKG_VERSION"),
        &req,
        false,
        search_available,
        config.mcp_extract_available(),
        config.mcp.default_max_length,
    ) {
        crw_core::mcp::ProtocolResult::Response(resp) => {
            let count = resp
                .result
                .as_ref()
                .and_then(|r| r.get("tools"))
                .and_then(|t| t.as_array())
                .map(Vec::len)
                .unwrap_or(0);
            if count > 0 {
                CheckResult::pass(
                    "smoke.mcp.protocol",
                    format!("tools/list returned {count} tool(s)"),
                )
            } else {
                CheckResult::fail(
                    "smoke.mcp.protocol",
                    "tools/list returned zero tools",
                    "check crw-mcp-proto's tool_definitions()",
                )
            }
        }
        _ => CheckResult::fail(
            "smoke.mcp.protocol",
            "tools/list did not produce a protocol response",
            "check crw-mcp-proto's handle_protocol_method()",
        ),
    }
}

async fn mcp_live_scrape_check(url: &str, config: &AppConfig, target: Target) -> CheckResult {
    let result = if target == Target::Cloud {
        let Some(api_url) = config.client.api_url.as_deref() else {
            return CheckResult::fail(
                "smoke.mcp.scrape",
                "resolved to a remote API but [client].api_url is unset",
                "set CRW_API_URL or run `crw setup --cloud`",
            );
        };
        let client = match reqwest::Client::builder()
            .redirect(crw_core::url_safety::safe_redirect_policy())
            .timeout(Duration::from_secs(30))
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                return CheckResult::fail(
                    "smoke.mcp.scrape",
                    format!("could not build an HTTP client: {error}"),
                    "check local TLS/cert setup",
                );
            }
        };
        crate::commands::mcp::proxy_call_tool(
            &client,
            api_url,
            &config.client.api_key,
            "crw_scrape",
            serde_json::json!({ "url": url }),
        )
        .await
    } else {
        #[cfg(feature = "mcp-embedded")]
        {
            let mut local_config = AppConfig::default();
            local_config.renderer.mode = crw_core::config::RendererMode::None;
            local_config.search.enabled = false;
            match crw_server::state::AppState::new(local_config) {
                Ok(state) => {
                    crw_server::routes::mcp::call_tool(
                        &state,
                        "crw_scrape",
                        serde_json::json!({ "url": url }),
                    )
                    .await
                }
                Err(error) => Err(format!("could not build embedded MCP state: {error}")),
            }
        }
        #[cfg(not(feature = "mcp-embedded"))]
        {
            return CheckResult::skip(
                "smoke.mcp.scrape",
                "local MCP smoke needs the embedded engine (--no-default-features disabled)",
            );
        }
    };

    match result {
        Ok(value) if contains_nonempty_markdown(&value) => CheckResult::pass(
            "smoke.mcp.scrape",
            format!("crw_scrape fetched {url} through the MCP tool path"),
        ),
        Ok(_) => CheckResult::fail(
            "smoke.mcp.scrape",
            format!("crw_scrape fetched {url} but returned no markdown"),
            "check MCP response shaping and the scrape backend",
        ),
        Err(error) => CheckResult::fail(
            "smoke.mcp.scrape",
            format!("crw_scrape failed for {url}: {error}"),
            "check the MCP backend, target URL, and remote credentials",
        ),
    }
}

fn contains_nonempty_markdown(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            (key == "markdown" && value.as_str().is_some_and(|text| !text.is_empty()))
                || contains_nonempty_markdown(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(contains_nonempty_markdown),
        _ => false,
    }
}
