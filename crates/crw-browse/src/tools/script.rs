//! `script` — execute a sequence of tool calls in one request.
//!
//! Each action is dispatched to the handler that backs the corresponding
//! single-call tool, so behaviour is identical to making N separate MCP
//! calls — but the round-trips collapse into one. This is useful for
//! deterministic flows (login → click → wait → assert) where the LLM
//! doesn't need to inspect intermediate state.
//!
//! Semantics:
//! - Steps run **sequentially**; `script` is not parallel.
//! - On the first step that returns an error response, every following
//!   step is reported as `skipped` and the script stops. We do NOT roll
//!   back successful steps — interactions like `click` are inherently
//!   irreversible from the server side.
//! - The outer response always serialises with `ok: true` (the script
//!   itself ran); per-step success is reflected in each entry's `ok`.

use std::time::Instant;

use rmcp::{ErrorData as McpError, model::CallToolResult, schemars};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{ErrorCode, ErrorResponse};
use crate::response::ToolResponse;
use crate::server::CrwBrowse;
use crate::tools::common::{err_result, no_session_err, ok_result};
use crate::tools::{
    click, console, evaluate, fill, goto, html, network, storage, text, tree, type_text, wait,
};

/// Cap on actions per script. Limits the worst-case time a single MCP call
/// can pin a session.
const MAX_ACTIONS: usize = 50;

/// `act` values accepted by [`dispatch`].
pub(crate) const ACTS: [&str; 12] = [
    "goto",
    "tree",
    "evaluate",
    "text",
    "html",
    "storage",
    "click",
    "fill",
    "type_text",
    "wait",
    "console",
    "network",
];

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ScriptInput {
    /// Ordered list of actions to execute. Each action is a JSON object
    /// with an `act` discriminator naming the tool to call; remaining
    /// fields are forwarded to that tool's input.
    #[schemars(schema_with = "actions_schema")]
    pub actions: Vec<Value>,
}

/// Explicit item schema: `Vec<Value>` would derive `"items": true`, a boolean
/// schema that llama.cpp's grammar builder rejects. `additionalProperties`
/// must stay `true`, grammar-constrained backends forbid unlisted keys
/// otherwise and the per-tool fields could not be forwarded.
fn actions_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": { "act": { "type": "string", "enum": ACTS } },
            "required": ["act"],
            "additionalProperties": true
        }
    })
}

#[derive(Debug, Serialize)]
pub struct StepOutcome {
    pub step: usize,
    pub act: String,
    pub ok: bool,
    /// Time spent in this step (sub-tool wall clock).
    pub elapsed_ms: u64,
    /// Sub-tool's response payload, parsed back to JSON. `null` if parsing
    /// failed (sub-tool emitted non-JSON content — should not happen).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Populated on `ok=false`: error code from the sub-tool's response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    /// `true` when the script aborted before reaching this step.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[serde(default)]
    pub skipped: bool,
}

#[derive(Debug, Serialize)]
pub struct ScriptData {
    pub total: usize,
    pub completed: usize,
    pub aborted: bool,
    pub steps: Vec<StepOutcome>,
}

pub async fn handle(server: &CrwBrowse, input: ScriptInput) -> Result<CallToolResult, McpError> {
    let started = Instant::now();
    if input.actions.is_empty() {
        return Ok(err_result(&ErrorResponse::new(
            ErrorCode::InvalidArgs,
            "actions must not be empty",
        )));
    }
    if input.actions.len() > MAX_ACTIONS {
        return Ok(err_result(&ErrorResponse::new(
            ErrorCode::InvalidArgs,
            format!(
                "too many actions: {}, max {MAX_ACTIONS}",
                input.actions.len()
            ),
        )));
    }

    let total = input.actions.len();
    let mut steps: Vec<StepOutcome> = Vec::with_capacity(total);
    let mut aborted = false;
    let mut completed = 0usize;

    for (idx, action) in input.actions.into_iter().enumerate() {
        let step_n = idx + 1;
        let act = action
            .get("act")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if aborted {
            steps.push(StepOutcome {
                step: step_n,
                act,
                ok: false,
                elapsed_ms: 0,
                data: None,
                error: None,
                skipped: true,
            });
            continue;
        }

        let step_started = Instant::now();
        let dispatched = dispatch(server, &act, action).await;
        let elapsed_ms = step_started.elapsed().as_millis() as u64;

        match dispatched {
            Err(err) => {
                aborted = true;
                steps.push(StepOutcome {
                    step: step_n,
                    act,
                    ok: false,
                    elapsed_ms,
                    data: None,
                    error: Some(serde_json::to_value(&err).unwrap_or(Value::Null)),
                    skipped: false,
                });
            }
            Ok(result) => {
                let (ok_step, data, err) = parse_call_result(&result);
                if ok_step {
                    completed += 1;
                } else {
                    aborted = true;
                }
                steps.push(StepOutcome {
                    step: step_n,
                    act,
                    ok: ok_step,
                    elapsed_ms,
                    data,
                    error: err,
                    skipped: false,
                });
            }
        }
    }

    // Fetch the session once so we don't double-touch it. Cheap clone of the
    // `Arc`; both fields are derived from the same handle so we don't see a
    // skew between the two reads (e.g. session goes away between them).
    let post_session = server.default_session_get().await;
    let session_short_id = post_session
        .as_ref()
        .map(|s| s.short_id.clone())
        .unwrap_or_else(|| "----".to_string());
    let last_url = match post_session.as_ref() {
        Some(s) => s.last_url().await,
        None => None,
    };

    let payload = ToolResponse::new(
        session_short_id,
        last_url,
        ScriptData {
            total,
            completed,
            aborted,
            steps,
        },
    )
    .with_elapsed_ms(started.elapsed().as_millis() as u64);
    Ok(ok_result(&payload))
}

/// Dispatch a single action JSON to the matching tool's handler. Returns
/// `Err(ErrorResponse)` on validation failures (unknown act, malformed
/// input); `Ok(CallToolResult)` for everything else, including sub-tool
/// errors (which arrive as `is_error: true` results).
/// Deserialize a sub-tool's input from the script step's JSON, mapping
/// serde failures to a structured `INVALID_ARGS` error. Replaces the old
/// `parse!` macro: a generic fn is testable and avoids the `.clone()` the
/// macro needed because serde_json takes the value by-value.
#[allow(clippy::result_large_err)]
fn parse_action<T: serde::de::DeserializeOwned>(
    act: &str,
    action: Value,
) -> Result<T, ErrorResponse> {
    serde_json::from_value::<T>(action).map_err(|e| {
        ErrorResponse::new(
            ErrorCode::InvalidArgs,
            format!("step `{act}`: malformed input: {e}"),
        )
    })
}

/// Dispatch a single action JSON to the matching tool's handler. Returns
/// `Err(ErrorResponse)` on validation failures (unknown act, malformed
/// input); `Ok(CallToolResult)` for everything else, including sub-tool
/// errors (which arrive as `is_error: true` results).
///
/// Valid `act` values: `goto`, `tree`, `evaluate`, `text`, `html`, `storage`,
/// `click`, `fill`, `type_text`, `wait`, `console`, `network`. The
/// `screenshot` tool is intentionally excluded — it returns base64 image
/// bytes that don't fit the per-step JSON shape this script tool relies on.
async fn dispatch(
    server: &CrwBrowse,
    act: &str,
    action: Value,
) -> Result<CallToolResult, ErrorResponse> {
    // Pre-flight: every action except `goto` requires a session.
    if act != "goto" && server.default_session_get().await.is_none() {
        return Err(no_session_err());
    }

    let result = match act {
        "goto" => goto::handle(server, parse_action(act, action)?)
            .await
            .map_err(|e| internal_err(act, e))?,
        "tree" => tree::handle(server, parse_action(act, action)?)
            .await
            .map_err(|e| internal_err(act, e))?,
        "evaluate" => evaluate::handle(server, parse_action(act, action)?)
            .await
            .map_err(|e| internal_err(act, e))?,
        "text" => text::handle(server, parse_action(act, action)?)
            .await
            .map_err(|e| internal_err(act, e))?,
        "html" => html::handle(server, parse_action(act, action)?)
            .await
            .map_err(|e| internal_err(act, e))?,
        "storage" => storage::handle(server, parse_action(act, action)?)
            .await
            .map_err(|e| internal_err(act, e))?,
        "click" => click::handle(server, parse_action(act, action)?)
            .await
            .map_err(|e| internal_err(act, e))?,
        "fill" => fill::handle(server, parse_action(act, action)?)
            .await
            .map_err(|e| internal_err(act, e))?,
        "type_text" => type_text::handle(server, parse_action(act, action)?)
            .await
            .map_err(|e| internal_err(act, e))?,
        "wait" => wait::handle(server, parse_action(act, action)?)
            .await
            .map_err(|e| internal_err(act, e))?,
        "console" => console::handle(server, parse_action(act, action)?)
            .await
            .map_err(|e| internal_err(act, e))?,
        "network" => network::handle(server, parse_action(act, action)?)
            .await
            .map_err(|e| internal_err(act, e))?,
        "" => {
            return Err(ErrorResponse::new(
                ErrorCode::InvalidArgs,
                "action missing `act` field",
            ));
        }
        other => {
            return Err(ErrorResponse::new(
                ErrorCode::InvalidArgs,
                format!(
                    "unknown act `{other}` — expected one of goto, tree, evaluate, text, \
                     html, storage, click, fill, type_text, wait, console, network"
                ),
            ));
        }
    };
    Ok(result)
}

fn internal_err(act: &str, e: McpError) -> ErrorResponse {
    ErrorResponse::new(
        ErrorCode::Internal,
        format!("step `{act}` raised internal MCP error: {e}"),
    )
}

/// Parse a sub-tool's `CallToolResult` back into structured fields:
/// `(ok, data, error)`. Sub-tools always emit a single `text` content with
/// JSON; we read that and split into the two payload shapes.
fn parse_call_result(result: &CallToolResult) -> (bool, Option<Value>, Option<Value>) {
    let is_error = result.is_error.unwrap_or(false);
    let ok = !is_error;
    let mut payload: Option<Value> = None;
    if let Some(first) = result.content.first()
        && let Some(text) = first.as_text().map(|t| &t.text)
    {
        payload = serde_json::from_str::<Value>(text).ok();
    }
    if ok {
        let data = payload.as_ref().and_then(|v| v.get("data")).cloned();
        (true, data, None)
    } else {
        (false, None, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{BrowseConfig, CrwBrowse};

    fn err_text(result: &CallToolResult) -> String {
        assert_eq!(result.is_error, Some(true), "expected is_error=true");
        result
            .content
            .first()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn dispatch_includes_console_and_network() {
        let server = CrwBrowse::new(BrowseConfig::default());
        for act in ["console", "network"] {
            let err = dispatch(&server, act, serde_json::json!({"act": act}))
                .await
                .expect_err("no session → Err");
            assert!(
                !err.message.contains("unknown act"),
                "{act} must be in dispatch table, got: {}",
                err.message
            );
            assert!(
                err.message.to_lowercase().contains("session"),
                "{act}: expected session error, got: {}",
                err.message
            );
        }
    }

    #[tokio::test]
    async fn parse_action_reports_act_in_error() {
        let bad: Result<crate::tools::wait::WaitInput, ErrorResponse> =
            parse_action("wait", serde_json::json!({"timeout_ms": "not-a-number"}));
        let err = bad.expect_err("must fail");
        assert!(err.message.contains("step `wait`"), "got: {}", err.message);
        assert!(
            err.message.contains("malformed input"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn handle_rejects_empty_actions() {
        let server = CrwBrowse::new(BrowseConfig::default());
        let res = handle(&server, ScriptInput { actions: vec![] })
            .await
            .expect("handle");
        let body = err_text(&res);
        assert!(
            body.contains("must not be empty"),
            "expected empty rejection, got: {body}"
        );
    }

    #[tokio::test]
    async fn handle_rejects_too_many_actions() {
        let server = CrwBrowse::new(BrowseConfig::default());
        let actions: Vec<Value> = (0..(MAX_ACTIONS + 1))
            .map(|_| serde_json::json!({"act":"tree"}))
            .collect();
        let res = handle(&server, ScriptInput { actions })
            .await
            .expect("handle");
        let body = err_text(&res);
        assert!(
            body.contains("too many actions"),
            "expected oversize rejection, got: {body}"
        );
    }
}
