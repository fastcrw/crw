//! OpenAI Responses API transport shared by text generation and structured
//! extraction.
//!
//! The Responses wire format is not Chat Completions with a different path:
//! function tools are flat objects, generated calls live in `output`, and token
//! usage uses `input_tokens` / `output_tokens`. Keep that translation isolated
//! here so OpenAI-compatible Chat Completions providers remain unchanged.

use crate::llm::{LlmCallResult, shared_client};
use crate::pricing;
use crw_core::config::LlmConfig;
use crw_core::error::{CrwError, CrwResult};
use crw_core::types::LlmUsage;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Resolve a Responses endpoint from either a versioned API base or a complete
/// endpoint. A base such as `https://gateway.example/v1` therefore becomes
/// `https://gateway.example/v1/responses`, while a complete endpoint is idempotent.
///
/// The `/responses` suffix check runs on the PATH only: a query string or
/// fragment must be preserved and re-attached after the path, never treated as
/// part of it. Azure-style Responses endpoints carry a required
/// `?api-version=…`, and appending after it (`…?api-version=x/responses`)
/// corrupts the parameter instead of building a path.
pub(crate) fn responses_url(base_url: Option<&str>) -> String {
    let raw = base_url.unwrap_or(DEFAULT_BASE_URL);
    let split = raw.find(['?', '#']).unwrap_or(raw.len());
    let (path, suffix) = raw.split_at(split);
    let path = path.trim_end_matches('/');
    if path.ends_with("/responses") {
        format!("{path}{suffix}")
    } else {
        format!("{path}/responses{suffix}")
    }
}

fn apply_optional_generation_fields(body: &mut serde_json::Value, llm: &LlmConfig) {
    if let Some(temperature) = llm.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }
    if let Some(effort) = llm.reasoning_effort.as_deref().filter(|s| !s.is_empty()) {
        body["reasoning"] = serde_json::json!({ "effort": effort });
    }
}

async fn post(
    llm: &LlmConfig,
    body: &serde_json::Value,
    timeout: Duration,
) -> CrwResult<serde_json::Value> {
    let url = responses_url(llm.base_url.as_deref());
    let resp = crate::llm::send_provider_post(
        shared_client()
            .post(&url)
            .timeout(timeout)
            .bearer_auth(&llm.api_key)
            .header("content-type", "application/json")
            .json(body),
    )
    .await
    .map_err(|e| {
        CrwError::ExtractionError(format!(
            "Responses API request failed: {}",
            crw_core::error::reqwest_message(e)
        ))
    })?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| {
        CrwError::ExtractionError(format!(
            "Failed to read Responses API response ({status}): {}",
            crw_core::error::reqwest_message(e)
        ))
    })?;
    if !status.is_success() {
        // The HTTP status code is enough — do not leak the body. A gateway that
        // echoes the request back in an error body would otherwise surface the
        // bearer key or the prompt to the API caller.
        return Err(CrwError::ExtractionError(format!(
            "Responses API error ({status})"
        )));
    }

    let payload: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        CrwError::ExtractionError(format!("Failed to parse Responses API response: {e}"))
    })?;
    check_response_status(&payload)?;
    Ok(payload)
}

/// A Responses payload carries its own terminal state, so an HTTP 200 is not by
/// itself a success. Only an absent status (compatibility gateways that omit
/// the field), `completed`, and `incomplete` carry usable output; `failed`,
/// `cancelled`, `queued`, `in_progress` and anything unrecognised must not be
/// mistaken for a result.
///
/// `incomplete` (the provider stopped at `max_output_tokens` or a content
/// filter) stays a success with whatever was produced, matching the Chat
/// Completions path, which likewise does not reject `finish_reason == "length"`.
/// The failure is named by its status only. `error.message` is gateway-supplied
/// and reaches the API caller verbatim through the 422 body, so echoing it would
/// reopen on this path exactly the leak [`post`] refuses to open on the non-2xx
/// one — and with no size bound at all. The status word is echoed only when it
/// is one the API actually defines, keeping even that string bounded.
fn check_response_status(payload: &serde_json::Value) -> CrwResult<()> {
    let Some(status) = payload.get("status").and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    if matches!(status, "completed" | "incomplete") {
        return Ok(());
    }
    Err(CrwError::ExtractionError(
        if matches!(status, "failed" | "cancelled" | "queued" | "in_progress") {
            format!("Responses API returned status '{status}'")
        } else {
            // Server-side only: an operator debugging a gateway that speaks a
            // dialect of the status vocabulary needs the actual word, and the
            // caller-facing string above deliberately withholds it.
            tracing::warn!(
                status = %status,
                "Responses API returned an unrecognised status"
            );
            "Responses API returned an unrecognised status".to_string()
        },
    ))
}

/// Join every `output_text` part the assistant produced.
///
/// The Chat Completions paths key on one field: an absent `message.content`
/// errors, a present-but-empty one succeeds. `output_text` is the Responses
/// analogue of that field, so presence is keyed on the PART, not on the
/// enclosing `message` item — a message carrying only a `refusal` part is
/// "content absent" exactly as a Chat Completions refusal (which nulls
/// `content`) is, and must not read as an empty answer.
///
/// `None`: no usable `output_text` part anywhere (reasoning-only, refusal-only,
/// or a malformed part whose `text` is missing or not a string).
/// `Some("")`: a real `output_text` part whose text is genuinely empty.
fn output_text(payload: &serde_json::Value) -> Option<String> {
    let mut parts = payload
        .get("output")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("message"))
        .filter_map(|item| item.get("content").and_then(serde_json::Value::as_array))
        .flatten()
        .filter(|part| part.get("type").and_then(serde_json::Value::as_str) == Some("output_text"))
        .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
        .peekable();
    parts.peek()?;
    Some(parts.collect::<Vec<_>>().join(""))
}

fn parse_usage(payload: &serde_json::Value, llm: &LlmConfig) -> Option<LlmUsage> {
    let usage = payload.get("usage")?;
    let input_tokens = usage.get("input_tokens")?.as_u64()? as u32;
    let output_tokens = usage.get("output_tokens")?.as_u64()? as u32;
    let total_tokens = usage
        .get("total_tokens")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as u32)
        .unwrap_or(input_tokens.saturating_add(output_tokens));
    let cached = usage
        .get("input_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as u32);

    Some(LlmUsage {
        input_tokens,
        output_tokens,
        total_tokens,
        estimated_cost_usd: pricing::calculate_cost(&llm.model, input_tokens, output_tokens),
        model: llm.model.clone(),
        provider: "openai-responses".to_string(),
        cache_hit_input_tokens: cached,
        cache_miss_input_tokens: cached.map(|n| input_tokens.saturating_sub(n)),
        truncated: false,
        calls: 1,
        executed_summaries: 0,
        answer_executed: false,
    })
}

/// One non-streaming text response. `instructions` carries the trusted system
/// prompt and `input` carries the user/content message.
pub(crate) async fn call_text(
    llm: &LlmConfig,
    instructions: &str,
    input: &str,
    timeout: Duration,
) -> CrwResult<LlmCallResult> {
    let mut body = serde_json::json!({
        "model": llm.model,
        "instructions": instructions,
        "input": input,
        "max_output_tokens": llm.max_tokens,
        "store": false,
    });
    apply_optional_generation_fields(&mut body, llm);

    let payload = post(llm, &body, timeout).await?;
    let content = output_text(&payload).ok_or_else(|| {
        CrwError::ExtractionError("Responses API response missing output_text".into())
    })?;
    Ok(LlmCallResult {
        content,
        usage: parse_usage(&payload, llm),
        warning: None,
    })
}

/// One forced function call used as the structured-output envelope. CRW does
/// not execute the function; its JSON arguments are the extraction result.
pub(crate) async fn call_tool(
    llm: &LlmConfig,
    input: &str,
    schema: &serde_json::Value,
    tool_name: &str,
    tool_desc: &str,
    timeout: Duration,
    force_tool: bool,
) -> CrwResult<(serde_json::Value, Option<LlmUsage>)> {
    let mut body = serde_json::json!({
        "model": llm.model,
        "input": input,
        "max_output_tokens": llm.max_tokens,
        "store": false,
        "parallel_tool_calls": false,
        "tools": [{
            "type": "function",
            "name": tool_name,
            "description": tool_desc,
            "parameters": schema,
        }],
    });
    if force_tool {
        // Same rule as the chat transports: force the tool only when there is a
        // caller schema to hold the result to. A prompt-only extraction has no
        // validation behind it, so forcing there would trade a loud failure for
        // an invented object.
        body["tool_choice"] = serde_json::json!({ "type": "function", "name": tool_name });
    }
    apply_optional_generation_fields(&mut body, llm);

    let payload = post(llm, &body, timeout).await?;
    let usage = parse_usage(&payload, llm);
    if let Some(arguments) = payload
        .get("output")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .find(|item| {
            item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
                && item.get("name").and_then(serde_json::Value::as_str) == Some(tool_name)
        })
        .and_then(|item| item.get("arguments"))
        .and_then(serde_json::Value::as_str)
    {
        let value = serde_json::from_str(arguments).map_err(|e| {
            CrwError::ExtractionError(format!(
                "Failed to parse Responses function call arguments: {e}"
            ))
        })?;
        return Ok((value, usage));
    }

    // Some compatibility gateways ignore tool_choice and return JSON text.
    // Preserve CRW's existing fallback behavior, then apply normal schema
    // validation in the caller.
    let raw_text = output_text(&payload).unwrap_or_default();
    let value = crate::structured::parse_json_response(&raw_text)?;
    Ok((value, usage))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_appends_responses_to_versioned_base() {
        assert_eq!(
            responses_url(Some("https://gateway.example/v1")),
            "https://gateway.example/v1/responses"
        );
    }

    #[test]
    fn endpoint_preserves_complete_url() {
        assert_eq!(
            responses_url(Some("https://example.test/v1/responses/")),
            "https://example.test/v1/responses"
        );
    }

    #[test]
    fn endpoint_keeps_query_and_fragment_after_the_path() {
        // Azure-style Responses endpoints carry a required api-version. The
        // suffix must survive on both the append and the idempotent branch.
        assert_eq!(
            responses_url(Some("https://gateway.example/v1?api-version=2026-01-01")),
            "https://gateway.example/v1/responses?api-version=2026-01-01"
        );
        assert_eq!(
            responses_url(Some(
                "https://gateway.example/v1/responses?api-version=2026-01-01"
            )),
            "https://gateway.example/v1/responses?api-version=2026-01-01"
        );
        assert_eq!(
            responses_url(Some("https://gateway.example/v1#frag")),
            "https://gateway.example/v1/responses#frag"
        );
    }

    #[test]
    fn status_gate_admits_only_usable_terminal_states() {
        // Absent status: compatibility gateways that omit the field.
        assert!(check_response_status(&serde_json::json!({})).is_ok());
        assert!(check_response_status(&serde_json::json!({ "status": "completed" })).is_ok());
        // Stopped at max_output_tokens: keep whatever was produced, like the
        // Chat Completions path does for finish_reason == "length".
        assert!(check_response_status(&serde_json::json!({ "status": "incomplete" })).is_ok());

        for status in ["failed", "cancelled", "queued", "in_progress"] {
            let err = check_response_status(&serde_json::json!({ "status": status }))
                .expect_err("non-terminal or failed status must not pass as a result");
            assert!(err.to_string().contains(status), "status named in error");
        }
    }

    #[test]
    fn status_gate_never_echoes_gateway_controlled_strings() {
        // `error.message` is written by the gateway and would reach the API
        // caller through the 422 body — the same leak the non-2xx branch of
        // `post` refuses to open.
        let err = check_response_status(&serde_json::json!({
            "status": "failed",
            "error": { "code": "server_error", "message": "Bearer SENTINEL-LEAK-TOKEN" }
        }))
        .expect_err("failed status must error");
        let msg = err.to_string();
        assert!(msg.contains("failed"), "status is named: {msg}");
        assert!(!msg.contains("SENTINEL-LEAK-TOKEN"), "body echoed: {msg}");

        // An unrecognised status is itself gateway-controlled, so it is not
        // echoed either — no unbounded string reaches the caller.
        let err = check_response_status(&serde_json::json!({ "status": "SENTINEL-LEAK-TOKEN" }))
            .expect_err("unknown status must error");
        assert!(!err.to_string().contains("SENTINEL-LEAK-TOKEN"));
    }

    #[test]
    fn output_text_distinguishes_absent_from_empty() {
        // A message item whose text is empty is "present but empty" -> Some("").
        assert_eq!(
            output_text(&serde_json::json!({
                "output": [{
                    "type": "message",
                    "content": [{ "type": "output_text", "text": "" }]
                }]
            })),
            Some(String::new())
        );
        // No message item at all -> nothing textual was produced.
        assert_eq!(
            output_text(&serde_json::json!({
                "output": [{ "type": "reasoning", "summary": [] }]
            })),
            None
        );
        assert_eq!(output_text(&serde_json::json!({})), None);
        // A refusal is carried as a message with no output_text part. Chat
        // Completions nulls `content` for the same case and errors, so this
        // must be "absent", never an empty answer.
        assert_eq!(
            output_text(&serde_json::json!({
                "output": [{
                    "type": "message",
                    "content": [{ "type": "refusal", "refusal": "I cannot help with that." }]
                }]
            })),
            None
        );
        // Malformed part: type says output_text but text is not a string.
        assert_eq!(
            output_text(&serde_json::json!({
                "output": [{
                    "type": "message",
                    "content": [{ "type": "output_text", "text": null }]
                }]
            })),
            None
        );
    }
}
