//! `CrwBrowse` — the rmcp server. The `#[tool]` methods on this struct are
//! thin wrappers; their bodies live in `crate::tools::<name>::handle()`. Tool
//! logic, input/output types, and unit tests sit alongside their handler in
//! the corresponding `tools/` submodule.
//!
//! Walking skeleton: a single default session is created lazily on the first
//! tool call. Multi-session + `session.new`/`session.close` tools land later
//! in Phase 2 (see ROADMAP).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
};
use tokio::sync::RwLock;

use crw_renderer::cdp_conn::CdpConnection;

use crate::session::{BrowserSession, SessionRegistry};
use crate::tools;

/// Startup configuration for the server.
#[derive(Debug, Clone)]
pub struct BrowseConfig {
    pub ws_url: String,
    pub page_timeout: Duration,
    /// Optional Chrome/Chromium CDP endpoint used as a fallback for
    /// operations Lightpanda implements as no-ops (notably
    /// `Page.captureScreenshot`, which returns fake bytes on Lightpanda
    /// v0.2.9). Tools that require Chrome return `NOT_IMPLEMENTED` when
    /// this is `None`.
    pub chrome_ws_url: Option<String>,
    /// Optional directory where screenshot `path` outputs may be created.
    /// When unset, screenshots must be returned inline as base64.
    pub screenshot_dir: Option<PathBuf>,
}

impl Default for BrowseConfig {
    fn default() -> Self {
        Self {
            ws_url: "ws://localhost:9222".to_string(),
            page_timeout: Duration::from_secs(30),
            chrome_ws_url: None,
            screenshot_dir: None,
        }
    }
}

#[derive(Clone)]
pub struct CrwBrowse {
    config: Arc<BrowseConfig>,
    registry: Arc<SessionRegistry>,
    default_session: Arc<RwLock<Option<Arc<BrowserSession>>>>,
    /// Lazily-opened Chrome/Chromium CDP connection used for
    /// screenshot/PDF fallback. `None` until the first tool that needs it
    /// initialises it via [`Self::ensure_chrome_connection`]. Behind a
    /// `RwLock<Option<Arc<...>>>` so concurrent readers share a single
    /// connection without serialising on a write lock.
    chrome_conn: Arc<RwLock<Option<Arc<CdpConnection>>>>,
    #[allow(dead_code)] // read by the #[tool_handler] generated glue
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl CrwBrowse {
    pub fn new(config: BrowseConfig) -> Self {
        Self {
            config: Arc::new(config),
            registry: Arc::new(SessionRegistry::new()),
            default_session: Arc::new(RwLock::new(None)),
            chrome_conn: Arc::new(RwLock::new(None)),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Navigate the browser to the given URL and wait for the page to load. \
                       Only `http` and `https` schemes are accepted; any other scheme \
                       (file://, data:, javascript:, blob:, etc.) returns `INVALID_ARGS`. \
                       Creates a default session on first call. Response includes `session` \
                       (4-char token), `url`, `data.status` (HTTP status, 0 if the network \
                       event was missed — see `warnings`), and `elapsed_ms`. `timeout_ms` \
                       is capped at 120000; values above are clamped and a `warnings` \
                       entry reports the clamp."
    )]
    pub async fn goto(
        &self,
        Parameters(input): Parameters<tools::goto::GotoInput>,
    ) -> Result<CallToolResult, McpError> {
        tools::goto::handle(self, input).await
    }

    #[tool(
        description = "Snapshot the current page as an indented accessibility tree. \
                       Each line is `@e<N> role: name`, with 2-space indentation to \
                       show parent/child structure. `@e<N>` ref tokens are stable \
                       within one snapshot and accepted by interaction tools \
                       (`click`, `fill`, ...) until the next `tree` call replaces \
                       them. Reduce `max_nodes` for large pages to save tokens; \
                       values above 5000 are clamped. Requires a prior `goto` call."
    )]
    pub async fn tree(
        &self,
        Parameters(input): Parameters<tools::tree::TreeInput>,
    ) -> Result<CallToolResult, McpError> {
        tools::tree::handle(self, input).await
    }

    #[tool(
        description = "Run a JavaScript expression on the current page and return \
                       its value. `Runtime.evaluate` is invoked with \
                       `returnByValue=true` and `awaitPromise=true`, so async \
                       expressions resolve before returning. If the expression \
                       throws, returns `INVALID_EXPRESSION` with the message. \
                       Requires a prior `goto` call."
    )]
    pub async fn evaluate(
        &self,
        Parameters(input): Parameters<tools::evaluate::EvaluateInput>,
    ) -> Result<CallToolResult, McpError> {
        tools::evaluate::handle(self, input).await
    }

    #[tool(
        description = "Read visible text from the page. Without `selector`, returns \
                       `document.body.innerText` (collapsed whitespace, hidden \
                       elements removed). With `selector`, returns just the matched \
                       element's `innerText`; missing element → `ELEMENT_NOT_FOUND`. \
                       Output capped at 50KB; oversized text is truncated with `…` \
                       and `truncated: true` in the response."
    )]
    pub async fn text(
        &self,
        Parameters(input): Parameters<tools::text::TextInput>,
    ) -> Result<CallToolResult, McpError> {
        tools::text::handle(self, input).await
    }

    #[tool(
        description = "Read raw HTML from the page. Without `selector`, returns the \
                       full `<html>...</html>` (`document.documentElement.outerHTML`). \
                       With `selector`, returns just the matched element's `outerHTML`; \
                       missing element → `ELEMENT_NOT_FOUND`. Output capped at 200KB; \
                       oversized HTML is truncated and `truncated: true` is set."
    )]
    pub async fn html(
        &self,
        Parameters(input): Parameters<tools::html::HtmlInput>,
    ) -> Result<CallToolResult, McpError> {
        tools::html::handle(self, input).await
    }

    #[tool(
        description = "Drain the session's console-message ring buffer. The buffer is \
                       captured from `Runtime.consoleAPICalled` events and holds up to \
                       200 entries (oldest dropped on overflow). Optional `level` \
                       filters by severity (`error`, `warning`, `log`, ...). Set \
                       `clear: true` to wipe the buffer after the snapshot."
    )]
    pub async fn console(
        &self,
        Parameters(input): Parameters<tools::console::ConsoleInput>,
    ) -> Result<CallToolResult, McpError> {
        tools::console::handle(self, input).await
    }

    #[tool(
        description = "Drain the session's network ring buffer. Captured from \
                       `Network.requestWillBeSent` and `Network.responseReceived` \
                       events; up to 500 entries kept (oldest dropped on overflow). \
                       `filter`: `all` (default), `failed` (status >= 400), \
                       `requests`, or `responses`. Set `clear: true` to wipe \
                       after the snapshot."
    )]
    pub async fn network(
        &self,
        Parameters(input): Parameters<tools::network::NetworkInput>,
    ) -> Result<CallToolResult, McpError> {
        tools::network::handle(self, input).await
    }

    #[tool(description = "Read or write browser storage. \
                       `action` ∈ {`get`,`set`,`clear`}, \
                       `kind` ∈ {`cookie`,`local`,`session`}. \
                       `set` requires `key` and `value`; `get` returns all entries \
                       for the chosen kind on the current origin. Cookies are \
                       handled via `Network.getCookies`/`setCookie`; local and \
                       session storage ride over `Runtime.evaluate`.")]
    pub async fn storage(
        &self,
        Parameters(input): Parameters<tools::storage::StorageInput>,
    ) -> Result<CallToolResult, McpError> {
        tools::storage::handle(self, input).await
    }

    #[tool(
        description = "Click an element. Pass either `selector` (CSS) or `ref` \
                       (`@e<N>` from `tree`); exactly one is required. The click \
                       is dispatched as a synthetic `click` event so framework \
                       handlers (React, Vue) fire identically to a real user \
                       click. No coordinate translation is performed, so scrolled \
                       or covered elements still receive the click."
    )]
    pub async fn click(
        &self,
        Parameters(input): Parameters<tools::click::ClickInput>,
    ) -> Result<CallToolResult, McpError> {
        tools::click::handle(self, input).await
    }

    #[tool(description = "Set an input element's value and dispatch `input` + \
                       `change` events so framework listeners fire. Pass either \
                       `selector` (CSS) or `ref` (`@e<N>` from `tree`); exactly \
                       one is required. The response includes the element's \
                       post-write `value` so the LLM can verify the assignment \
                       took effect (some controlled inputs intercept writes).")]
    pub async fn fill(
        &self,
        Parameters(input): Parameters<tools::fill::FillInput>,
    ) -> Result<CallToolResult, McpError> {
        tools::fill::handle(self, input).await
    }

    #[tool(description = "Type characters into the currently focused element by \
                       dispatching `Input.dispatchKeyEvent` per character. Use \
                       `click` first to focus the target element. `delay_ms` \
                       inserts a per-character pause (default 0, max 1000); \
                       text length is capped at 4096 bytes.")]
    pub async fn type_text(
        &self,
        Parameters(input): Parameters<tools::type_text::TypeTextInput>,
    ) -> Result<CallToolResult, McpError> {
        tools::type_text::handle(self, input).await
    }

    #[tool(description = "Block until a condition is met or the timeout fires. \
                       Pass `selector` (CSS) to wait for an element; \
                       `condition: visible` (default for selectors) requires \
                       the element to be in the rendered tree, `condition: \
                       present` accepts any DOM match. Without a selector, \
                       `condition` may be `load` (`Page.loadEventFired`) or \
                       `networkidle` (500ms of network silence). Default \
                       timeout is 5000ms; capped at 120000ms.")]
    pub async fn wait(
        &self,
        Parameters(input): Parameters<tools::wait::WaitInput>,
    ) -> Result<CallToolResult, McpError> {
        tools::wait::handle(self, input).await
    }

    #[tool(
        description = "Capture the current page as PNG/JPEG. Requires the server \
                       to be started with `--chrome-ws-url`; otherwise returns \
                       `NOT_IMPLEMENTED` (Lightpanda's `Page.captureScreenshot` \
                       returns fake bytes). The Chrome fallback opens a separate \
                       browser at the same URL — cookies/auth/scroll are NOT \
                       transferred from the Lightpanda session. Pass `path` to \
                       write to disk, or omit it to receive the image inline as \
                       base64."
    )]
    pub async fn screenshot(
        &self,
        Parameters(input): Parameters<tools::screenshot::ScreenshotInput>,
    ) -> Result<CallToolResult, McpError> {
        tools::screenshot::handle(self, input).await
    }

    #[tool(description = "Execute a sequence of tool calls in one request. Each \
                       action is a JSON object whose `act` names the tool \
                       (`goto`, `tree`, `evaluate`, `text`, `html`, `storage`, \
                       `click`, `fill`, `type_text`, `wait`); remaining fields \
                       are the tool's input. Steps run sequentially; the first \
                       error aborts the script and remaining steps are reported \
                       as `skipped`. Capped at 50 actions per call.")]
    pub async fn script(
        &self,
        Parameters(input): Parameters<tools::script::ScriptInput>,
    ) -> Result<CallToolResult, McpError> {
        tools::script::handle(self, input).await
    }
}

// Internal accessors used by tool handlers in `crate::tools::*`. These are
// `pub(crate)` rather than public to keep the embedder API surface small —
// tools are not meant to be authored outside this crate.
impl CrwBrowse {
    pub(crate) fn config(&self) -> &BrowseConfig {
        &self.config
    }

    pub(crate) async fn default_session_get(&self) -> Option<Arc<BrowserSession>> {
        let session = self.default_session.read().await.clone()?;
        // Heartbeat: any tool entry that resolves a session counts as
        // activity, so the TTL sweeper can't kill an actively-used session.
        session.touch().await;
        Some(session)
    }

    pub(crate) async fn ensure_default_session(
        &self,
    ) -> Result<Arc<BrowserSession>, crw_core::error::CrwError> {
        if let Some(s) = self.default_session.read().await.clone()
            && !s.is_closing.load(std::sync::atomic::Ordering::SeqCst)
        {
            s.touch().await;
            return Ok(s);
        }
        let mut slot = self.default_session.write().await;
        if let Some(s) = slot.clone()
            && !s.is_closing.load(std::sync::atomic::Ordering::SeqCst)
        {
            s.touch().await;
            return Ok(s);
        }
        let conn = CdpConnection::connect(&self.config.ws_url, Duration::from_secs(10)).await?;
        let session = self.registry.insert(Arc::new(conn))?;
        *slot = Some(session.clone());
        Ok(session)
    }

    /// Lazily open a CDP connection to the configured Chrome endpoint.
    /// Returns `None` if neither `chrome_ws_url` nor `ws_url` is set —
    /// the caller should surface `NOT_IMPLEMENTED` in that case.
    ///
    /// Auto-fallback: when `chrome_ws_url` is not configured, we reuse
    /// `ws_url`. The screenshot tool's docstring says both flags can
    /// point at the same Chrome instance — making that the implicit
    /// default removes the "had to learn two flags" footgun that R3
    /// dogfood agents repeatedly hit. Lightpanda misconfiguration is
    /// caught downstream by the screenshot byte-count sanity check.
    pub(crate) async fn ensure_chrome_connection(
        &self,
    ) -> Result<Option<Arc<CdpConnection>>, crw_core::error::CrwError> {
        let url = match self.config.chrome_ws_url.clone().or_else(|| {
            if self.config.ws_url.is_empty() {
                None
            } else {
                Some(self.config.ws_url.clone())
            }
        }) {
            Some(u) => u,
            None => return Ok(None),
        };
        if let Some(c) = self.chrome_conn.read().await.clone()
            && !c.is_closed()
        {
            return Ok(Some(c));
        }
        // Connect WITHOUT holding the write lock. `CdpConnection::connect`
        // can take up to 10s (TCP + WS handshake + Target.getTargets), and
        // holding the write lock across that await starves every other
        // session creation in flight. Cost of releasing: under racy startup
        // two callers may both reach this branch and both connect; the
        // double-check below keeps the first to install, and the loser's
        // Arc<CdpConnection> is dropped and cleanly closed by Drop.
        let new_conn = Arc::new(CdpConnection::connect(&url, Duration::from_secs(10)).await?);
        let mut slot = self.chrome_conn.write().await;
        if let Some(c) = slot.clone()
            && !c.is_closed()
        {
            return Ok(Some(c));
        }
        *slot = Some(new_conn.clone());
        Ok(Some(new_conn))
    }
}

#[tool_handler]
impl ServerHandler for CrwBrowse {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "Interactive browser automation over CDP. Call `goto` to navigate, \
             then `tree` to inspect the rendered accessibility tree."
                    .to_string(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Boolean subschemas (`"items": true`) are valid JSON Schema but
    /// llama.cpp's grammar builder rejects the whole request over them.
    /// `additionalProperties: true` is exempt: llama.cpp accepts it.
    fn assert_no_bool_subschema(v: &Value, path: &str) {
        let children: Vec<(String, &Value)> = match v {
            Value::Object(o) => o.iter().map(|(k, c)| (k.clone(), c)).collect(),
            Value::Array(a) => a
                .iter()
                .enumerate()
                .map(|(i, c)| (i.to_string(), c))
                .collect(),
            _ => return,
        };
        for (key, child) in children {
            let child_path = format!("{path}/{key}");
            let schemas: Vec<&Value> = match (key.as_str(), child) {
                ("items" | "not" | "contains" | "additionalItems", s) => vec![s],
                (
                    "properties" | "$defs" | "definitions" | "patternProperties",
                    Value::Object(m),
                ) => m.values().collect(),
                ("anyOf" | "oneOf" | "allOf" | "prefixItems", Value::Array(a)) => {
                    a.iter().collect()
                }
                _ => vec![],
            };
            for s in schemas {
                assert!(s.is_object(), "{child_path} holds a boolean schema");
            }
            assert_no_bool_subschema(child, &child_path);
        }
    }

    #[test]
    fn tool_input_schemas_have_no_boolean_subschemas() {
        let tools = CrwBrowse::tool_router().list_all();
        assert!(!tools.is_empty());
        for tool in tools {
            let schema = Value::Object((*tool.input_schema).clone());
            assert_no_bool_subschema(&schema, &tool.name);
        }
    }

    #[test]
    fn script_actions_schema_constrains_act() {
        let tools = CrwBrowse::tool_router().list_all();
        let script = tools
            .iter()
            .find(|t| t.name == "script")
            .expect("script tool");
        let items = &script.input_schema["properties"]["actions"]["items"];
        assert_eq!(items["required"], serde_json::json!(["act"]));
        assert_eq!(items["additionalProperties"], serde_json::json!(true));
        assert_eq!(
            items["properties"]["act"]["enum"],
            serde_json::json!(crate::tools::script::ACTS)
        );
    }
}
