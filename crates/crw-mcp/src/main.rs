//! MCP (Model Context Protocol) server for the CRW web scraper.
//!
//! Supports two modes:
//!
//! - **Embedded (default)** — Self-contained scraping engine. No external server needed.
//! - **Proxy** — Forwards tool calls to a remote CRW server over HTTP.
//!
//! Mode selection: a truthy `CRW_LOCAL` forces embedded mode, whatever else is
//! configured. Otherwise proxy mode is used when an API URL resolves from
//! `--api-url`, `CRW_API_URL`, `CRW_CLIENT__API_URL` or `client.api_url` in the
//! config file; failing that, embedded mode (requires the `embedded` feature,
//! on by default).
//!
//! # Tools
//!
//! - `crw_scrape` — scrape a single URL
//! - `crw_crawl` — start an async BFS crawl
//! - `crw_check_crawl_status` — poll crawl job status
//! - `crw_map` — discover URLs on a website
//! - `crw_search` — web search (embedded: only when a search backend is configured; proxy: forwards to the remote API)
//! - `crw_extract` — start an async multi-URL structured extraction job
//! - `crw_check_extract_status` — poll extract job status
//! - `crw_cancel_extract` — idempotently cancel an extract job
//! - `crw_parse_file` — parse a local PDF (base64) to markdown
//!
//! # Usage
//!
//! ```bash
//! # Embedded mode (default — no server needed)
//! crw-mcp
//!
//! # Proxy mode — connect to a remote server
//! crw-mcp --api-url https://api.fastcrw.com --api-key crw_live_xxx
//! ```

mod teardown;

use clap::Parser;
use crw_core::mcp::{
    JsonRpcRequest, JsonRpcResponse, ProtocolResult, handle_protocol_method, tool_result_response,
};
use serde_json::{Value, json};
use teardown::{CmdError, finish, install_signal_teardown};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[cfg(feature = "embedded")]
use crw_renderer::browser;

const SERVER_NAME: &str = "crw-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

// --- CLI ---

#[derive(Parser)]
#[command(name = "crw-mcp", about = "MCP server for the CRW web scraper")]
struct Cli {
    /// Remote CRW server URL. Enables proxy mode.
    /// Without this flag, runs in embedded mode (self-contained).
    /// A truthy `CRW_LOCAL` overrides this and forces embedded mode.
    #[arg(long, env = "CRW_API_URL")]
    api_url: Option<String>,

    /// API key for remote server authentication.
    #[arg(long, env = "CRW_API_KEY")]
    api_key: Option<String>,

    /// Config file path, overriding config.local.toml. Read in both modes:
    /// proxy mode uses its `client.*` and `[mcp]` keys too.
    #[arg(long, env = "CRW_CONFIG")]
    config: Option<String>,

    /// Hide credit-billing fields (`creditCost`/`creditsUsed`) from tool
    /// responses. Saves context tokens for self-hosted deployments, where
    /// credits are unused bookkeeping. Equivalent to setting `[mcp]`
    /// `hide_credits = true` in the config file, which works in both modes;
    /// this flag is the one-off override. In proxy mode the strip runs
    /// client-side, since a remote's REST body is not shaped by local config.
    #[arg(long, env = "CRW_MCP__HIDE_CREDITS")]
    hide_credits: bool,
}

// --- Backend ---

enum Backend {
    Proxy {
        client: reqwest::Client,
        base_url: String,
        api_key: Option<String>,
        hide_credits: bool,
    },
    #[cfg(feature = "embedded")]
    Embedded { state: crw_server::state::AppState },
}

impl Backend {
    async fn call_tool(&self, tool_name: &str, args: Value) -> Result<Value, String> {
        match self {
            Backend::Proxy {
                client,
                base_url,
                api_key,
                ..
            } => proxy_call_tool(client, base_url, api_key, tool_name, args).await,
            #[cfg(feature = "embedded")]
            Backend::Embedded { state } => {
                crw_server::routes::mcp::call_tool(state, tool_name, args).await
            }
        }
    }

    fn is_proxy(&self) -> bool {
        matches!(self, Backend::Proxy { .. })
    }

    /// Whether `crw_search` should be advertised. Proxy: yes (the remote decides).
    /// Embedded: only if a SearXNG backend is configured, so a no-backend install
    /// doesn't advertise a tool that only returns `search_disabled`.
    fn search_available(&self) -> bool {
        match self {
            Backend::Proxy { .. } => true,
            #[cfg(feature = "embedded")]
            Backend::Embedded { state } => state.searxng.is_some(),
        }
    }

    /// Whether to strip credit fields at this dispatch layer. Proxy mode only:
    /// the embedded backend strips inside `call_tool` (driven by server
    /// config), and re-stripping here would be a wasted no-op walk.
    fn hide_credits(&self) -> bool {
        match self {
            Backend::Proxy { hide_credits, .. } => *hide_credits,
            #[cfg(feature = "embedded")]
            Backend::Embedded { .. } => false,
        }
    }

    async fn handle_request(&self, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
        // Handle common protocol methods via shared logic.
        match handle_protocol_method(
            SERVER_NAME,
            SERVER_VERSION,
            &req,
            self.is_proxy(),
            self.search_available(),
        ) {
            ProtocolResult::Response(resp) => return Some(resp),
            ProtocolResult::Notification => return None,
            ProtocolResult::NotHandled => {}
        }

        // Only remaining method: tools/call
        match req.method.as_str() {
            "tools/call" => {
                let id = req.id.unwrap_or(Value::Null);
                let tool_name = req
                    .params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                // Unknown tool name → JSON-RPC -32602 (not an isError result).
                if !crw_core::mcp::is_known_tool(tool_name) {
                    return Some(JsonRpcResponse::error(
                        id,
                        -32602,
                        format!("unknown tool: {tool_name}"),
                    ));
                }

                let arguments = req.params.get("arguments").cloned().unwrap_or(json!({}));

                // Bound the result at the MCP layer before it reaches context.
                // Works for both embedded and proxy backends.
                let hide_credits = self.hide_credits();
                let result = self.call_tool(tool_name, arguments.clone()).await.map(|v| {
                    let mut v = crw_core::mcp::apply_bounds(tool_name, &arguments, v);
                    if hide_credits {
                        crw_core::mcp::strip_credit_fields(&mut v);
                    }
                    v
                });
                Some(tool_result_response(id, tool_name, result))
            }

            _ => {
                if let Some(id) = req.id {
                    Some(JsonRpcResponse::error(
                        id,
                        -32601,
                        format!("method not found: {}", req.method),
                    ))
                } else {
                    None
                }
            }
        }
    }
}

// --- Proxy mode HTTP dispatch ---

// Per-endpoint timeouts. /map and /crawl can take longer because the engine
// fetches sitemaps and discovers links across many pages; scrape/search are
// single-page and capped lower. status is a cheap polling GET.
//
// Keep these aligned with crw-saas/src/lib/crw-client.ts so a saas-fronted
// MCP doesn't trip its own client before the upstream responds.
const TIMEOUT_SCRAPE: std::time::Duration = std::time::Duration::from_secs(120);
const TIMEOUT_CRAWL_KICKOFF: std::time::Duration = std::time::Duration::from_secs(120);
const TIMEOUT_CRAWL_STATUS: std::time::Duration = std::time::Duration::from_secs(30);
const TIMEOUT_MAP: std::time::Duration = std::time::Duration::from_secs(180);
const TIMEOUT_SEARCH: std::time::Duration = std::time::Duration::from_secs(120);

async fn proxy_call_tool(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &Option<String>,
    tool_name: &str,
    args: Value,
) -> Result<Value, String> {
    // Strip MCP-only control args (maxLength, crw_map's limit) so a strict upstream
    // doesn't reject unknown body fields; bounds are applied locally to the response
    // by apply_bounds in the dispatch layer.
    let args = crw_core::mcp::strip_mcp_only_args(tool_name, args);

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    if let Some(key) = api_key {
        headers.insert(
            "authorization",
            format!("Bearer {key}")
                .parse()
                .map_err(|e| format!("invalid api key: {e}"))?,
        );
    }

    match tool_name {
        "crw_scrape" => {
            let resp = client
                .post(format!("{base_url}/v1/scrape"))
                .headers(headers)
                .timeout(TIMEOUT_SCRAPE)
                .json(&args)
                .send()
                .await
                .map_err(|e| {
                    format!(
                        "HTTP request failed: {}",
                        crw_core::error::reqwest_message(e)
                    )
                })?;
            parse_response(resp).await
        }
        "crw_crawl" => {
            let resp = client
                .post(format!("{base_url}/v1/crawl"))
                .headers(headers)
                .timeout(TIMEOUT_CRAWL_KICKOFF)
                .json(&args)
                .send()
                .await
                .map_err(|e| {
                    format!(
                        "HTTP request failed: {}",
                        crw_core::error::reqwest_message(e)
                    )
                })?;
            parse_response(resp).await
        }
        "crw_check_crawl_status" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("missing required parameter: id")?;
            let resp = client
                .get(format!("{base_url}/v1/crawl/{id}"))
                .headers(headers)
                .timeout(TIMEOUT_CRAWL_STATUS)
                .send()
                .await
                .map_err(|e| {
                    format!(
                        "HTTP request failed: {}",
                        crw_core::error::reqwest_message(e)
                    )
                })?;
            parse_response(resp).await
        }
        "crw_map" => {
            let resp = client
                .post(format!("{base_url}/v1/map"))
                .headers(headers)
                .timeout(TIMEOUT_MAP)
                .json(&args)
                .send()
                .await
                .map_err(|e| {
                    format!(
                        "HTTP request failed: {}",
                        crw_core::error::reqwest_message(e)
                    )
                })?;
            parse_response(resp).await
        }
        "crw_search" => {
            let resp = client
                .post(format!("{base_url}/v1/search"))
                .headers(headers)
                .timeout(TIMEOUT_SEARCH)
                .json(&args)
                .send()
                .await
                .map_err(|e| {
                    format!(
                        "HTTP request failed: {}",
                        crw_core::error::reqwest_message(e)
                    )
                })?;
            parse_response(resp).await
        }
        "crw_extract" => {
            let mut headers = headers;
            headers.insert("prefer", "respond-async".parse().unwrap());
            let resp = client
                .post(format!("{base_url}/v1/extract"))
                .headers(headers)
                .timeout(TIMEOUT_CRAWL_KICKOFF)
                .json(&args)
                .send()
                .await
                .map_err(|e| {
                    format!(
                        "HTTP request failed: {}",
                        crw_core::error::reqwest_message(e)
                    )
                })?;
            parse_response(resp).await
        }
        "crw_check_extract_status" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("missing required parameter: id")?;
            let resp = client
                .get(format!("{base_url}/v1/extract/{id}"))
                .headers(headers)
                .timeout(TIMEOUT_CRAWL_STATUS)
                .send()
                .await
                .map_err(|e| {
                    format!(
                        "HTTP request failed: {}",
                        crw_core::error::reqwest_message(e)
                    )
                })?;
            parse_response(resp).await
        }
        "crw_cancel_extract" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("missing required parameter: id")?;
            let resp = client
                .delete(format!("{base_url}/v1/extract/{id}"))
                .headers(headers)
                .timeout(TIMEOUT_CRAWL_STATUS)
                .send()
                .await
                .map_err(|e| {
                    format!(
                        "HTTP request failed: {}",
                        crw_core::error::reqwest_message(e)
                    )
                })?;
            parse_response(resp).await
        }
        "crw_parse_file" => {
            use base64::Engine;
            let b64 = args
                .get("contentBase64")
                .and_then(|v| v.as_str())
                .ok_or("missing required parameter: contentBase64")?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .map_err(|e| format!("invalid base64 in contentBase64: {e}"))?;
            let filename = args
                .get("filename")
                .and_then(|v| v.as_str())
                .unwrap_or("document.pdf")
                .to_string();
            // Forward the remaining fields (formats/jsonSchema/parsers/…) as the
            // multipart `options` JSON.
            let mut options = args.clone();
            if let Some(obj) = options.as_object_mut() {
                obj.remove("contentBase64");
                obj.remove("filename");
            }
            let part = reqwest::multipart::Part::bytes(bytes)
                .file_name(filename)
                .mime_str("application/pdf")
                .map_err(|e| format!("invalid part: {e}"))?;
            let form = reqwest::multipart::Form::new()
                .part("file", part)
                .text("options", options.to_string());
            // Multipart sets its own content-type/boundary — use auth-only headers.
            let mut mp_headers = reqwest::header::HeaderMap::new();
            if let Some(key) = api_key {
                mp_headers.insert(
                    "authorization",
                    format!("Bearer {key}")
                        .parse()
                        .map_err(|e| format!("invalid api key: {e}"))?,
                );
            }
            let resp = client
                .post(format!("{base_url}/v2/parse"))
                .headers(mp_headers)
                .timeout(TIMEOUT_SCRAPE)
                .multipart(form)
                .send()
                .await
                .map_err(|e| {
                    format!(
                        "HTTP request failed: {}",
                        crw_core::error::reqwest_message(e)
                    )
                })?;
            parse_response(resp).await
        }
        _ => Err(format!("unknown tool: {tool_name}")),
    }
}

async fn parse_response(resp: reqwest::Response) -> Result<Value, String> {
    let status = resp.status();
    let body = resp.text().await.map_err(|e| {
        format!(
            "failed to read response: {}",
            crw_core::error::reqwest_message(e)
        )
    })?;

    if !status.is_success() {
        return Err(format!("API error ({}): {}", status, truncate(&body, 500)));
    }

    let parsed: Value =
        serde_json::from_str(&body).map_err(|e| format!("invalid JSON response: {e}"))?;

    // A failure envelope at HTTP 200 is still a failure. An anti-bot wall the
    // renderer could not clear arrives this way, and deciding on the transport
    // alone would hand the agent `isError: false` with a body-less document,
    // which a model reports as "the page was empty" and then acts on. Surfacing
    // it as an error instead puts the real reason in the model's context, where
    // it can pick another URL or tell the user.
    //
    // Narrowed to the codes that carry NO readable body. `/v1/scrape` keeps the
    // page in the envelope on its `http_error` branch on purpose ("the caller
    // can still read the error page"), and collapsing that to an error string
    // threw away both the body and `metadata.statusCode` the model used to get
    // for an ordinary 404. `anti_bot` and `no_usable_content` are the branches
    // that call `clear_body()`, so there is genuinely nothing left to return.
    // Keyed on the CODE, not on a body probe. An earlier shape here asked whether
    // `data.markdown` was absent, but `ScrapeData` skips `None` fields when it
    // serializes, so for `formats:["html"]` or `["links"]` there is simply no
    // `markdown` key and every envelope read as empty, which put the
    // `http_error` body straight back in the bin this narrowing exists to keep it
    // out of. `anti_bot` and `no_usable_content` are exactly the branches that
    // call `clear_body()`, so the code alone is the honest discriminator; a
    // missing/unknown code keeps the old fail-closed behaviour.
    const NO_CONTENT_CODES: [&str; 2] = ["anti_bot", "no_usable_content"];
    let failed = parsed.get("success").and_then(Value::as_bool) == Some(false);
    let code_str = parsed.get("errorCode").and_then(Value::as_str);
    if failed && code_str.is_none_or(|c| NO_CONTENT_CODES.contains(&c)) {
        let reason = parsed
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("the request did not succeed");
        let code = code_str.map(|c| format!(" [{c}]")).unwrap_or_default();
        return Err(format!("{reason}{code}"));
    }

    Ok(parsed)
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let end = s.floor_char_boundary(max);
        &s[..end]
    }
}

// --- Main ---

#[tokio::main]
async fn main() {
    // Log to stderr so stdout stays clean for MCP protocol.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "crw_mcp=info".parse().unwrap()),
        )
        .init();

    // Install the signal teardown task *before* any browser spawn inside
    // `run()`, then route every exit (Ok/Err/signal/EOF) through `finish`
    // so `kill_all_browsers()` runs exactly once.
    install_signal_teardown();
    finish(run().await);
}

async fn run() -> Result<(), CmdError> {
    let cli = Cli::parse();

    // Read before the partial moves below (`resolve_client_credentials` takes
    // the api_* fields).
    let flag_hide_credits = cli.hide_credits;

    // Point `AppConfig::load()` at `--config` before anything reads config —
    // `resolve_client_credentials` loads it too, so setting this later would
    // leave that first load reading the default chain instead of the named
    // file. Applies to both modes: the file also carries `client.*`.
    // SAFETY: single-threaded startup, before any other thread reads the env.
    if let Some(ref config_path) = cli.config {
        unsafe { std::env::set_var("CRW_CONFIG", config_path) };
    }

    // Resolve api_url / api_key with the standard precedence chain:
    //   1. CLI flag / env (already merged by clap)
    //   2. `client.api_url` / `client.api_key` in ~/.config/crw/config.toml
    //   3. None — falls through to embedded mode
    let (resolved_api_url, resolved_api_key, config_hide_credits) =
        resolve_client_credentials(cli.api_url, cli.api_key);
    // Flag/env or the config file turns it on. Both are needed: proxy mode
    // never builds an `AppConfig`, and the flag never reaches `AppConfig::load`.
    let hide_credits = flag_hide_credits || config_hide_credits;

    let backend = if let Some(api_url) = resolved_api_url {
        tracing::info!("Starting {SERVER_NAME} v{SERVER_VERSION} (proxy mode)");
        tracing::info!("API URL: {api_url}");

        // Per-request timeouts are applied below in proxy_call_tool — do not set
        // a global .timeout() here, or it would cap long endpoints like /map.
        let client = reqwest::Client::builder()
            .redirect(crw_core::url_safety::safe_redirect_policy())
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client build failed");

        Backend::Proxy {
            client,
            base_url: api_url,
            api_key: resolved_api_key,
            hide_credits,
        }
    } else {
        #[cfg(feature = "embedded")]
        {
            tracing::info!("Starting {SERVER_NAME} v{SERVER_VERSION} (embedded mode)");

            // `--config` was already exported to CRW_CONFIG at the top of `run`.
            let mut config = crw_core::config::AppConfig::load().unwrap_or_else(|e| {
                tracing::warn!("Failed to load config, using defaults: {e}");
                crw_core::config::AppConfig {
                    server: Default::default(),
                    renderer: Default::default(),
                    crawler: Default::default(),
                    extraction: Default::default(),
                    auth: Default::default(),
                    request: Default::default(),
                    search: Default::default(),
                    map: Default::default(),
                    document: Default::default(),
                    client: Default::default(),
                    mcp: Default::default(),
                }
            });
            // The CLI flag forces the config knob on for this process.
            if hide_credits {
                config.mcp.hide_credits = true;
            }

            // Auto-spawn a headless browser for JS rendering.
            // Priority: LightPanda (native/Docker) → Chrome/Chromium.
            // Skip only if the user explicitly set a renderer via env var.
            // Config file renderers (e.g. config.default.toml) may reference
            // Docker services that aren't running locally, so we don't trust them.
            let user_configured_renderer = std::env::var("CRW_RENDERER__LIGHTPANDA__WS_URL")
                .is_ok()
                || std::env::var("CRW_RENDERER__CHROME__WS_URL").is_ok()
                || std::env::var("CRW_RENDERER__PLAYWRIGHT__WS_URL").is_ok();

            let _browser_guards = if !user_configured_renderer {
                let browsers = browser::spawn_all_headless().await;
                if browsers.is_empty() {
                    tracing::info!(
                        "No browser found — JS rendering disabled. Install LightPanda \
                         or Chrome for full SPA support, or point CRW_CHROME_PATH at an \
                         existing Chrome/Chromium executable."
                    );
                }
                let mut guards = Vec::new();
                for (guard, ws_url, kind) in browsers {
                    match kind {
                        browser::RendererKind::LightPanda => {
                            config.renderer.lightpanda =
                                Some(crw_core::config::CdpEndpoint { ws_url });
                        }
                        browser::RendererKind::Chrome => {
                            config.renderer.chrome = Some(crw_core::config::CdpEndpoint { ws_url });
                        }
                    }
                    guards.push(guard);
                }
                guards
            } else {
                tracing::info!("CDP renderer already configured — skipping auto-spawn");
                Vec::new()
            };

            let state = match crw_server::state::AppState::new(config) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to build application state: {e}");
                    return Err(CmdError::code_only(1));
                }
            };

            // Run the MCP loop. _browser_guards keeps browsers alive until shutdown.
            let backend = Backend::Embedded { state };
            run_stdio_loop(backend).await;

            // Drop browser guards explicitly (kills browser processes).
            drop(_browser_guards);
            return Ok(());
        }

        #[cfg(not(feature = "embedded"))]
        {
            if crw_core::config::env_var_truthy("CRW_LOCAL") {
                tracing::error!(
                    "CRW_LOCAL asks for a local engine, but this binary was compiled \
                     without the 'embedded' feature and cannot run one. Unset CRW_LOCAL \
                     to proxy to a remote CRW server, or install a full build."
                );
            } else {
                tracing::error!(
                    "Embedded mode not available (compiled without 'embedded' feature). \
                     Use --api-url to connect to a remote CRW server."
                );
            }
            return Err(CmdError::code_only(1));
        }
    };

    run_stdio_loop(backend).await;
    Ok(())
}

async fn run_stdio_loop(backend: Backend) {
    let mut stdout = tokio::io::stdout();
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                tracing::error!("stdin read error: {e}");
                break;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        tracing::debug!("← {} bytes: {:.200}", trimmed.len(), trimmed);

        let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                let err = JsonRpcResponse::error(Value::Null, -32700, format!("parse error: {e}"));
                let out = serde_json::to_string(&err).unwrap();
                tracing::debug!("→ {} bytes: {:.200}", out.len(), out);
                let _ = stdout.write_all(out.as_bytes()).await;
                let _ = stdout.write_all(b"\n").await;
                let _ = stdout.flush().await;
                continue;
            }
        };

        if let Some(resp) = backend.handle_request(req).await {
            let out = serde_json::to_string(&resp).unwrap();
            tracing::debug!("→ {} bytes: {:.200}", out.len(), out);
            let _ = stdout.write_all(out.as_bytes()).await;
            let _ = stdout.write_all(b"\n").await;
            let _ = stdout.flush().await;
        }
    }
}

/// Resolve proxy-mode credentials. A truthy `CRW_LOCAL` short-circuits to no
/// URL, so embedded mode is chosen whatever else is configured. Otherwise CLI /
/// env values (already merged by clap)
/// win; otherwise consult `client.{api_url,api_key}` from
/// `~/.config/crw/config.toml`. Mirrors the chain `crw mcp` uses, `CRW_LOCAL`
/// guard included, so the standalone binary and the subcommand pick the same
/// mode for the same environment.
/// Returns `(api_url, api_key, hide_credits)`. The config file is read even
/// when `--api-url` was passed: `[mcp] hide_credits` is independent of where
/// the backend lives, and proxy mode builds no `AppConfig` of its own, so this
/// is the only place the config knob can reach that backend.
fn resolve_client_credentials(
    cli_url: Option<String>,
    cli_key: Option<String>,
) -> (Option<String>, Option<String>, bool) {
    let cfg = crw_core::config::AppConfig::load().ok();
    let hide_credits = cfg.as_ref().is_some_and(|c| c.mcp.hide_credits);
    // `CRW_LOCAL` is the SDKs' "run the engine here, no cloud" opt-in, and they
    // spawn this binary to carry it out. Only the SDK used to read it, so a
    // `client.api_url` left behind by `crw setup` still selected proxy mode:
    // the run was billed, the URLs left the machine, and every tool answered
    // with the REST envelope instead of the flat payload the caller expected.
    //
    // It wins over every source, including the flag. `--api-url` and
    // `CRW_API_URL` arrive as the same clap `Option`, so "beat the env but not
    // the flag" is not expressible here, and of the two readings of a
    // contradictory request, the local one cannot spend the user's money.
    //
    // An explicitly passed key is handed back (harmless: only the proxy arm
    // reads one), but the config file's is not fetched. A local run carries no
    // cloud credential it never needed.
    if crw_core::config::env_var_truthy("CRW_LOCAL") {
        // Deliberately not logging the URL itself: a `client.api_url` may
        // carry userinfo, and this line lands in the MCP host's log file.
        if cli_url.is_some() || cfg.as_ref().is_some_and(|c| c.client.api_url.is_some()) {
            tracing::warn!("CRW_LOCAL is set: running embedded, ignoring the configured API URL");
        }
        return (None, cli_key, hide_credits);
    }
    if cli_url.is_some() {
        return (cli_url, cli_key, hide_credits);
    }
    match cfg {
        Some(cfg) => (
            cfg.client.api_url,
            cli_key.or(cfg.client.api_key),
            hide_credits,
        ),
        None => (None, cli_key, hide_credits),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // --- truncate ---

    #[test]
    fn truncate_shorter_than_max_returns_whole_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact_length_equals_max() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_longer_string_cuts_to_max() {
        assert_eq!(truncate("hello world", 5), "hello");
    }

    #[test]
    fn truncate_empty_string() {
        assert_eq!(truncate("", 5), "");
    }

    #[test]
    fn truncate_max_zero_returns_empty() {
        assert_eq!(truncate("hello", 0), "");
    }

    #[test]
    fn truncate_unicode_multibyte_boundary_never_panics() {
        // Each "é" is 2 bytes in UTF-8; max=3 lands mid-character.
        let s = "ééé";
        let out = truncate(s, 3);
        // floor_char_boundary rounds down, so we get exactly one full "é".
        assert_eq!(out, "é");
        assert!(out.len() <= 3);
    }

    #[test]
    fn truncate_emoji_boundary_never_panics() {
        // Each emoji is 4 bytes; max=2 lands mid-character, must round down to 0.
        let s = "🎉🎉";
        let out = truncate(s, 2);
        assert_eq!(out, "");
    }

    #[test]
    fn truncate_very_long_string() {
        let s = "a".repeat(10_000);
        let out = truncate(&s, 500);
        assert_eq!(out.len(), 500);
    }

    #[test]
    fn truncate_max_greater_than_len_by_one() {
        assert_eq!(truncate("hi", 3), "hi");
    }

    #[test]
    fn truncate_ascii_exact_char_boundary_unaffected() {
        // Pure ASCII: every byte offset is a valid char boundary.
        assert_eq!(truncate("abcdef", 3), "abc");
    }

    // --- Backend accessors (Proxy variant; no network involved) ---

    fn proxy_backend(hide_credits: bool) -> Backend {
        Backend::Proxy {
            client: reqwest::Client::new(),
            base_url: "https://example.invalid".to_string(),
            api_key: None,
            hide_credits,
        }
    }

    #[test]
    fn backend_proxy_is_proxy_true() {
        assert!(proxy_backend(false).is_proxy());
    }

    #[test]
    fn backend_proxy_search_available_always_true() {
        // Proxy defers the decision to the remote server.
        assert!(proxy_backend(false).search_available());
        assert!(proxy_backend(true).search_available());
    }

    #[test]
    fn backend_proxy_hide_credits_passthrough_false() {
        assert!(!proxy_backend(false).hide_credits());
    }

    #[test]
    fn backend_proxy_hide_credits_passthrough_true() {
        assert!(proxy_backend(true).hide_credits());
    }

    #[cfg(feature = "embedded")]
    #[tokio::test]
    async fn backend_embedded_is_proxy_false() {
        let config = crw_core::config::AppConfig {
            server: Default::default(),
            renderer: Default::default(),
            crawler: Default::default(),
            extraction: Default::default(),
            auth: Default::default(),
            request: Default::default(),
            search: Default::default(),
            map: Default::default(),
            document: Default::default(),
            client: Default::default(),
            mcp: Default::default(),
        };
        let state = crw_server::state::AppState::new(config).expect("default config builds");
        let backend = Backend::Embedded { state };
        assert!(!backend.is_proxy());
        // Default config has no search backend configured.
        assert!(!backend.search_available());
        // Embedded strips credits inside call_tool, not at this layer.
        assert!(!backend.hide_credits());
    }

    // --- handle_request (protocol methods; no network) ---

    fn req(method: &str, id: Option<Value>, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        }
    }

    #[tokio::test]
    async fn handle_request_invalid_jsonrpc_version_errors() {
        let backend = proxy_backend(false);
        let bad = JsonRpcRequest {
            jsonrpc: "1.0".to_string(),
            id: Some(json!(1)),
            method: "ping".to_string(),
            params: json!({}),
        };
        let resp = backend.handle_request(bad).await.expect("response");
        let err = resp.error.expect("error");
        assert_eq!(err.code, -32600);
        assert_eq!(resp.id, json!(1));
    }

    #[tokio::test]
    async fn handle_request_initialize_returns_server_info() {
        let backend = proxy_backend(false);
        let resp = backend
            .handle_request(req("initialize", Some(json!(1)), json!({})))
            .await
            .expect("response");
        let result = resp.result.expect("result");
        assert_eq!(result["serverInfo"]["name"], "crw-mcp");
        assert_eq!(result["serverInfo"]["version"], SERVER_VERSION);
        assert_eq!(result["capabilities"]["tools"]["listChanged"], false);
    }

    #[tokio::test]
    async fn handle_request_tools_list_includes_search_for_proxy() {
        let backend = proxy_backend(false);
        let resp = backend
            .handle_request(req("tools/list", Some(json!(1)), json!({})))
            .await
            .expect("response");
        let tools = resp.result.expect("result")["tools"]
            .as_array()
            .expect("tools array")
            .clone();
        assert!(tools.iter().any(|t| t["name"] == "crw_search"));
    }

    #[tokio::test]
    async fn handle_request_ping_returns_empty_object() {
        let backend = proxy_backend(false);
        let resp = backend
            .handle_request(req("ping", Some(json!(1)), json!({})))
            .await
            .expect("response");
        assert_eq!(resp.result.expect("result"), json!({}));
    }

    #[tokio::test]
    async fn handle_request_notification_initialized_returns_none() {
        let backend = proxy_backend(false);
        let resp = backend
            .handle_request(req("notifications/initialized", None, json!({})))
            .await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn handle_request_notification_cancelled_returns_none() {
        let backend = proxy_backend(false);
        let resp = backend
            .handle_request(req("notifications/cancelled", None, json!({})))
            .await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn handle_request_unknown_method_with_id_errors() {
        let backend = proxy_backend(false);
        let resp = backend
            .handle_request(req("totally/unknown", Some(json!(7)), json!({})))
            .await
            .expect("response");
        let err = resp.error.expect("error");
        assert_eq!(err.code, -32601);
        assert!(err.message.contains("totally/unknown"));
    }

    #[tokio::test]
    async fn handle_request_unknown_method_without_id_returns_none() {
        let backend = proxy_backend(false);
        let resp = backend
            .handle_request(req("totally/unknown", None, json!({})))
            .await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn handle_request_tools_call_unknown_tool_errors_without_network() {
        // Unknown tool names short-circuit before the backend is ever
        // dispatched, so this must not attempt any HTTP request.
        let backend = proxy_backend(false);
        let resp = backend
            .handle_request(req(
                "tools/call",
                Some(json!(9)),
                json!({ "name": "crw_not_a_real_tool", "arguments": {} }),
            ))
            .await
            .expect("response");
        let err = resp.error.expect("error");
        assert_eq!(err.code, -32602);
        assert!(err.message.contains("crw_not_a_real_tool"));
    }

    #[tokio::test]
    async fn handle_request_tools_call_missing_name_treated_as_unknown() {
        let backend = proxy_backend(false);
        let resp = backend
            .handle_request(req(
                "tools/call",
                Some(json!(9)),
                json!({ "arguments": {} }),
            ))
            .await
            .expect("response");
        let err = resp.error.expect("error");
        assert_eq!(err.code, -32602);
    }

    // --- resolve_client_credentials / Cli env precedence ---
    //
    // These mutate real process env vars (CRW_*), so every test below
    // acquires ENV_LOCK first and clears the relevant vars, and points
    // CRW_USER_CONFIG_DIR at a scratch dir so the developer's own
    // ~/.config/crw/config.toml is never read.

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn clear_crw_env() {
        // SAFETY: serialized by ENV_LOCK; no other thread touches env vars
        // for the duration a guard is held.
        unsafe {
            std::env::remove_var("CRW_API_URL");
            std::env::remove_var("CRW_API_KEY");
            std::env::remove_var("CRW_CLIENT__API_URL");
            std::env::remove_var("CRW_CLIENT__API_KEY");
            std::env::remove_var("CRW_MCP__HIDE_CREDITS");
            std::env::remove_var("CRW_CONFIG");
            std::env::remove_var("CRW_LOCAL");
        }
    }

    /// Scratch dir standing in for `~/.config/crw` for the duration of one
    /// test. Honored by `crw_core::config::user_config_path()` via
    /// `CRW_USER_CONFIG_DIR`, so the real per-user config on this machine is
    /// never touched.
    struct ScratchConfigDir(std::path::PathBuf);

    impl ScratchConfigDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "crw-mcp-test-{}-{tag}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            unsafe { std::env::set_var("CRW_USER_CONFIG_DIR", &dir) };
            Self(dir)
        }

        fn write_config(&self, toml: &str) {
            std::fs::write(self.0.join("config.toml"), toml).unwrap();
        }
    }

    impl Drop for ScratchConfigDir {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("CRW_USER_CONFIG_DIR") };
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resolve_credentials_no_env_no_config_returns_all_none() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let _dir = ScratchConfigDir::new("no-config");

        let (url, key, hide) = resolve_client_credentials(None, None);
        assert_eq!(url, None);
        assert_eq!(key, None);
        assert!(!hide);
    }

    // --- CRW_LOCAL forces embedded mode ---
    //
    // `CRW_LOCAL` is the SDKs' local opt-in and they spawn this binary to carry
    // it out. Every source of an api_url has to lose to it, or the "local" run
    // goes to the cloud and gets billed.

    #[test]
    fn resolve_credentials_crw_local_beats_env_client_api_url() {
        // `CRW_CLIENT__API_URL` reaches the resolver by a different route than
        // the flag: figment folds it into `cfg.client.api_url`.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let _dir = ScratchConfigDir::new("local-beats-env");
        // SAFETY: serialized by ENV_LOCK.
        unsafe {
            std::env::set_var("CRW_CLIENT__API_URL", "https://env.example");
            std::env::set_var("CRW_LOCAL", "1");
        }

        let (url, _key, _hide) = resolve_client_credentials(None, None);
        assert_eq!(url, None);
        clear_crw_env();
    }

    #[test]
    fn resolve_credentials_crw_local_beats_config_file_client_api_url() {
        // The route no SDK-side fix can reach: `crw setup` writes this file, so
        // the user never typed a URL anywhere in the CRW_LOCAL run.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let dir = ScratchConfigDir::new("local-beats-config");
        dir.write_config(
            "[client]\napi_url = \"https://from-config.example\"\napi_key = \"cfg-key\"\n",
        );
        // SAFETY: serialized by ENV_LOCK.
        unsafe {
            std::env::set_var("CRW_LOCAL", "1");
        }

        let (url, key, _hide) = resolve_client_credentials(None, None);
        assert_eq!(url, None);
        // The config file's cloud key is deliberately left behind: only the
        // proxy arm reads a key, and a run the user asked to keep local has no
        // business carrying a cloud credential.
        assert_eq!(key, None);
        clear_crw_env();
    }

    #[test]
    fn resolve_credentials_crw_local_beats_an_explicit_cli_url() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let _dir = ScratchConfigDir::new("local-beats-cli");
        // SAFETY: serialized by ENV_LOCK.
        unsafe {
            std::env::set_var("CRW_LOCAL", "1");
        }

        let (url, key, _hide) = resolve_client_credentials(
            Some("https://cli.example".to_string()),
            Some("cli-key".to_string()),
        );
        assert_eq!(url, None);
        assert_eq!(key, Some("cli-key".to_string()));
        clear_crw_env();
    }

    #[test]
    fn resolve_credentials_crw_local_still_reports_hide_credits() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let dir = ScratchConfigDir::new("local-hide-credits");
        // The url matters: without it this test passes even with the guard
        // deleted, and would prove nothing about the local path.
        dir.write_config(
            "[client]\napi_url = \"https://from-config.example\"\n\n[mcp]\nhide_credits = true\n",
        );
        // SAFETY: serialized by ENV_LOCK.
        unsafe {
            std::env::set_var("CRW_LOCAL", "1");
        }

        let (url, _key, hide) = resolve_client_credentials(None, None);
        assert_eq!(url, None);
        assert!(hide);
        clear_crw_env();
    }

    #[test]
    fn resolve_credentials_falsy_crw_local_leaves_proxy_mode_alone() {
        for falsy in ["0", ""] {
            let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            clear_crw_env();
            let dir = ScratchConfigDir::new("local-falsy");
            dir.write_config("[client]\napi_url = \"https://from-config.example\"\n");
            // SAFETY: serialized by ENV_LOCK.
            unsafe {
                std::env::set_var("CRW_LOCAL", falsy);
            }

            let (url, _key, _hide) = resolve_client_credentials(None, None);
            assert_eq!(
                url,
                Some("https://from-config.example".to_string()),
                "CRW_LOCAL={falsy:?} must not opt into local"
            );
            clear_crw_env();
        }
    }

    #[test]
    fn resolve_credentials_cli_url_wins_and_skips_config_lookup() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let dir = ScratchConfigDir::new("cli-url-wins");
        dir.write_config("[client]\napi_url = \"https://from-config.example\"\n");

        let (url, key, _hide) =
            resolve_client_credentials(Some("https://cli.example".to_string()), None);
        assert_eq!(url, Some("https://cli.example".to_string()));
        assert_eq!(key, None);
    }

    #[test]
    fn resolve_credentials_falls_back_to_config_file_client_section() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let dir = ScratchConfigDir::new("config-fallback");
        dir.write_config(
            "[client]\napi_url = \"https://from-config.example\"\napi_key = \"cfg-key\"\n",
        );

        let (url, key, _hide) = resolve_client_credentials(None, None);
        assert_eq!(url, Some("https://from-config.example".to_string()));
        assert_eq!(key, Some("cfg-key".to_string()));
    }

    #[test]
    fn resolve_credentials_cli_key_wins_over_config_key() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let dir = ScratchConfigDir::new("cli-key-wins");
        dir.write_config(
            "[client]\napi_url = \"https://from-config.example\"\napi_key = \"cfg-key\"\n",
        );

        let (_url, key, _hide) = resolve_client_credentials(None, Some("cli-key".to_string()));
        assert_eq!(key, Some("cli-key".to_string()));
    }

    #[test]
    fn resolve_credentials_no_config_but_cli_key_present() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let _dir = ScratchConfigDir::new("cli-key-only");

        let (url, key, hide) = resolve_client_credentials(None, Some("k".to_string()));
        assert_eq!(url, None);
        assert_eq!(key, Some("k".to_string()));
        assert!(!hide);
    }

    #[test]
    fn resolve_credentials_hide_credits_from_config_applies_even_with_cli_url() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let dir = ScratchConfigDir::new("hide-credits-cfg");
        dir.write_config("[mcp]\nhide_credits = true\n");

        let (_url, _key, hide) =
            resolve_client_credentials(Some("https://cli.example".to_string()), None);
        assert!(
            hide,
            "hide_credits is read from config independent of cli_url"
        );
    }

    #[test]
    fn resolve_credentials_env_client_api_url_overrides_config_file() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let dir = ScratchConfigDir::new("env-overrides-file");
        dir.write_config("[client]\napi_url = \"https://from-config.example\"\n");
        unsafe { std::env::set_var("CRW_CLIENT__API_URL", "https://from-env.example") };

        let (url, _key, _hide) = resolve_client_credentials(None, None);
        assert_eq!(url, Some("https://from-env.example".to_string()));

        unsafe { std::env::remove_var("CRW_CLIENT__API_URL") };
    }

    #[test]
    fn resolve_credentials_env_mcp_hide_credits_true() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let _dir = ScratchConfigDir::new("env-hide-credits");
        unsafe { std::env::set_var("CRW_MCP__HIDE_CREDITS", "true") };

        let (_url, _key, hide) = resolve_client_credentials(None, None);
        assert!(hide);

        unsafe { std::env::remove_var("CRW_MCP__HIDE_CREDITS") };
    }

    #[test]
    fn resolve_credentials_malformed_config_file_degrades_to_none_not_panic() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let dir = ScratchConfigDir::new("malformed-config");
        dir.write_config("this is not valid toml [[[");

        let (url, key, hide) = resolve_client_credentials(None, Some("k".to_string()));
        // AppConfig::load() fails -> .ok() -> None -> graceful fallback, cli_key kept.
        assert_eq!(url, None);
        assert_eq!(key, Some("k".to_string()));
        assert!(!hide);
    }

    // --- Cli (clap) parsing / env precedence ---

    #[test]
    fn cli_parse_no_args_defaults_to_none_and_false() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let cli = Cli::parse_from(["crw-mcp"]);
        assert_eq!(cli.api_url, None);
        assert_eq!(cli.api_key, None);
        assert_eq!(cli.config, None);
        assert!(!cli.hide_credits);
    }

    #[test]
    fn cli_parse_explicit_flags() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let cli = Cli::parse_from([
            "crw-mcp",
            "--api-url",
            "https://api.example",
            "--api-key",
            "secret",
            "--hide-credits",
        ]);
        assert_eq!(cli.api_url, Some("https://api.example".to_string()));
        assert_eq!(cli.api_key, Some("secret".to_string()));
        assert!(cli.hide_credits);
    }

    #[test]
    fn cli_parse_reads_env_when_flag_absent() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        unsafe {
            std::env::set_var("CRW_API_URL", "https://env.example");
            std::env::set_var("CRW_MCP__HIDE_CREDITS", "true");
        }

        let cli = Cli::parse_from(["crw-mcp"]);
        assert_eq!(cli.api_url, Some("https://env.example".to_string()));
        assert!(cli.hide_credits);

        clear_crw_env();
    }

    #[test]
    fn cli_parse_explicit_flag_wins_over_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        unsafe { std::env::set_var("CRW_API_URL", "https://env.example") };

        let cli = Cli::parse_from(["crw-mcp", "--api-url", "https://flag.example"]);
        assert_eq!(cli.api_url, Some("https://flag.example".to_string()));

        clear_crw_env();
    }

    #[test]
    fn cli_parse_config_flag_sets_config_path() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let cli = Cli::parse_from(["crw-mcp", "--config", "/tmp/some-config.toml"]);
        assert_eq!(cli.config, Some("/tmp/some-config.toml".to_string()));
    }

    #[test]
    fn cli_parse_rejects_unknown_flag() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let result = Cli::try_parse_from(["crw-mcp", "--not-a-real-flag"]);
        assert!(result.is_err());
    }

    #[test]
    fn cli_parse_hide_credits_boolean_flag_is_false_by_default_without_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_crw_env();
        let cli = Cli::parse_from(["crw-mcp"]);
        assert!(!cli.hide_credits);
    }
}
