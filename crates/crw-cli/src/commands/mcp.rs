//! MCP subcommand — start the Model Context Protocol server.
//!
//! Supports two modes:
//! - **Embedded (default)** — Self-contained scraping engine. No external server needed.
//! - **Proxy** — Forwards tool calls to a remote CRW server over HTTP.

use crate::teardown::CmdError;
use clap::Args;
use crw_core::mcp::{
    JsonRpcRequest, JsonRpcResponse, ProtocolResult, handle_protocol_method, tool_result_response,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing_subscriber::EnvFilter;

#[cfg(feature = "mcp-embedded")]
use crw_renderer::browser;

const SERVER_NAME: &str = "crw-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Args)]
pub struct McpArgs {
    /// Remote CRW server URL. Enables proxy mode.
    /// Without this flag, runs in embedded mode (self-contained).
    #[arg(long, env = "CRW_API_URL")]
    pub api_url: Option<String>,

    /// API key for remote server authentication.
    #[arg(long, env = "CRW_API_KEY")]
    pub api_key: Option<String>,

    /// Config file path, overriding config.local.toml. Read in both modes:
    /// proxy mode uses its `client.*` and `[mcp]` keys too.
    #[arg(long, env = "CRW_CONFIG")]
    pub config: Option<String>,

    /// Hide credit-billing fields (`creditCost`/`creditsUsed`) from tool
    /// responses. Saves context tokens for self-hosted deployments, where
    /// credits are unused bookkeeping. Equivalent to setting `[mcp]`
    /// `hide_credits = true` in the config file, which works in both modes;
    /// this flag is the one-off override. In proxy mode the strip runs
    /// client-side, since a remote's REST body is not shaped by local config.
    #[arg(long, env = "CRW_MCP__HIDE_CREDITS")]
    pub hide_credits: bool,
}

// --- Backend ---

enum Backend {
    Proxy {
        client: reqwest::Client,
        base_url: String,
        api_key: Option<String>,
        hide_credits: bool,
        /// State for `crw_find_data` / `crw_get_data` (manifest, pacing).
        data: Box<crw_core::connectors::DataToolContext>,
    },
    #[cfg(feature = "mcp-embedded")]
    Embedded { state: crw_server::state::AppState },
}

impl Backend {
    async fn call_tool(&self, tool_name: &str, args: Value) -> Result<Value, String> {
        match self {
            Backend::Proxy { data, .. } if crw_core::connectors::is_data_tool(tool_name) => {
                crw_core::connectors::call_data_tool(tool_name, args, data).await
            }
            Backend::Proxy {
                client,
                base_url,
                api_key,
                ..
            } => proxy_call_tool(client, base_url, api_key, tool_name, args).await,
            #[cfg(feature = "mcp-embedded")]
            Backend::Embedded { state } => {
                crw_server::routes::mcp::call_tool(state, tool_name, args).await
            }
        }
    }

    fn is_proxy(&self) -> bool {
        matches!(self, Backend::Proxy { .. })
    }

    /// Whether `crw_search` should be advertised (proxy: yes; embedded: only with a
    /// configured SearXNG backend).
    fn search_available(&self) -> bool {
        match self {
            Backend::Proxy { .. } => true,
            #[cfg(feature = "mcp-embedded")]
            Backend::Embedded { state } => state.searxng.is_some(),
        }
    }

    /// Whether to strip credit fields at this dispatch layer. Proxy mode only:
    /// the embedded backend strips inside `call_tool` (driven by server
    /// config), and re-stripping here would be a wasted no-op walk.
    fn hide_credits(&self) -> bool {
        match self {
            Backend::Proxy { hide_credits, .. } => *hide_credits,
            #[cfg(feature = "mcp-embedded")]
            Backend::Embedded { .. } => false,
        }
    }

    async fn handle_request(&self, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
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

        match req.method.as_str() {
            "tools/call" => {
                let id = req.id.unwrap_or(Value::Null);
                let tool_name = req
                    .params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                // Unknown tool name → JSON-RPC -32602 (not an isError result).
                if !crw_core::mcp::is_known_tool(tool_name, self.is_proxy()) {
                    return Some(JsonRpcResponse::error(
                        id,
                        -32602,
                        format!("unknown tool: {tool_name}"),
                    ));
                }

                let arguments = req.params.get("arguments").cloned().unwrap_or(json!({}));

                // Bound the result at the MCP layer before it reaches context.
                let hide_credits = self.hide_credits();
                let result = self.call_tool(tool_name, arguments.clone()).await.map(|v| {
                    let mut v = crw_core::mcp::apply_bounds(tool_name, &arguments, v);
                    if hide_credits {
                        crw_core::mcp::strip_tool_credit_fields(tool_name, &mut v);
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

const TIMEOUT_SCRAPE: std::time::Duration = std::time::Duration::from_secs(120);
const TIMEOUT_CRAWL_KICKOFF: std::time::Duration = std::time::Duration::from_secs(120);
const TIMEOUT_CRAWL_STATUS: std::time::Duration = std::time::Duration::from_secs(30);
const TIMEOUT_MAP: std::time::Duration = std::time::Duration::from_secs(180);
const TIMEOUT_SEARCH: std::time::Duration = std::time::Duration::from_secs(120);

pub(crate) async fn proxy_call_tool(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &Option<String>,
    tool_name: &str,
    args: Value,
) -> Result<Value, String> {
    // Strip MCP-only control args (maxLength, crw_map's limit) before forwarding;
    // bounds are applied locally to the response by apply_bounds in the dispatch.
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

    serde_json::from_str(&body).map_err(|e| format!("invalid JSON response: {e}"))
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

pub async fn run(args: McpArgs) -> Result<(), CmdError> {
    // Log to stderr so stdout stays clean for MCP protocol
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "crw=info".parse().unwrap()),
        )
        .init();

    // Resolve api_url / api_key with the standard precedence chain:
    //   1. CLI flag / env (already merged by clap)
    //   2. `client.api_url` / `client.api_key` in ~/.config/crw/config.toml
    //   3. None — falls through to embedded mode below
    let flag_hide_credits = args.hide_credits;
    // Point `AppConfig::load()` at `--config` before anything reads config —
    // `resolve_client_credentials` loads it too, so setting this later would
    // leave that first load reading the default chain instead of the named
    // file. Applies to both modes: the file also carries `client.*`.
    // SAFETY: single-threaded startup, before any other thread reads the env.
    if let Some(ref config_path) = args.config {
        unsafe { std::env::set_var("CRW_CONFIG", config_path) };
    }
    let (resolved_api_url, resolved_api_key, config_hide_credits) =
        resolve_client_credentials(args.api_url, args.api_key);
    // Flag/env or the config file turns it on. Both are needed: proxy mode
    // never builds an `AppConfig`, and the flag never reaches `AppConfig::load`.
    let hide_credits = flag_hide_credits || config_hide_credits;

    let backend = if let Some(api_url) = resolved_api_url {
        tracing::info!("Starting {SERVER_NAME} v{SERVER_VERSION} (proxy mode)");
        tracing::info!("API URL: {api_url}");

        let client = reqwest::Client::builder()
            .redirect(crw_core::url_safety::safe_redirect_policy())
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client build failed");

        let data = Box::new(crw_core::connectors::DataToolContext::new(
            api_url.clone(),
            resolved_api_key.clone(),
            crw_core::connectors::telemetry_enabled(),
        ));

        Backend::Proxy {
            client,
            base_url: api_url,
            api_key: resolved_api_key,
            hide_credits,
            data,
        }
    } else {
        #[cfg(feature = "mcp-embedded")]
        {
            tracing::info!("Starting {SERVER_NAME} v{SERVER_VERSION} (embedded mode)");

            // `--config` was already exported to CRW_CONFIG above.
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

            let backend = Backend::Embedded { state };
            run_stdio_loop(backend).await;
            drop(_browser_guards);
            return Ok(());
        }

        #[cfg(not(feature = "mcp-embedded"))]
        {
            if crw_core::config::env_var_truthy("CRW_LOCAL") {
                tracing::error!(
                    "CRW_LOCAL asks for a local engine, but this build has no \
                     'mcp-embedded' feature and cannot run one. Unset CRW_LOCAL to proxy \
                     to a remote CRW server, or install a full build."
                );
            } else {
                tracing::error!(
                    "Embedded mode not available (compiled without 'mcp-embedded' feature). \
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
            Ok(0) => break,
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

    // stdin closed: let the last usage pings land before the process exits.
    if let Backend::Proxy { data, .. } = &backend {
        data.drain(std::time::Duration::from_secs(2)).await;
    }
}

/// Resolve proxy-mode credentials. CLI / env values (already merged by clap)
/// win; otherwise consult `client.{api_url,api_key}` from
/// `~/.config/crw/config.toml`. This is what lets `crw setup --cloud` -> fresh
/// shell -> `crw mcp` start in proxy mode without a manual `source ~/.zshrc`.
/// Also returns `hide_credits`: the config file is read even when `--api-url`
/// was passed, because `[mcp] hide_credits` is independent of where the backend
/// lives and proxy mode builds no `AppConfig` of its own.
fn resolve_client_credentials(
    cli_url: Option<String>,
    cli_key: Option<String>,
) -> (Option<String>, Option<String>, bool) {
    let cfg = crw_core::config::AppConfig::load().ok();
    let hide_credits = cfg.as_ref().is_some_and(|c| c.mcp.hide_credits);
    // Same guard as the standalone `crw-mcp`, for the same reason: a
    // `client.api_url` left behind by `crw setup` would otherwise turn a run
    // the user asked to keep local into a billed cloud call. Behaviour and
    // rationale are covered by the tests on that copy; this file has no test
    // module, and adding one would need an env lock it does not have.
    if crw_core::config::env_var_truthy("CRW_LOCAL") {
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
