use axum::http::{HeaderName, HeaderValue};
use axum_test::TestServer;
use crw_core::config::AppConfig;
use crw_core::mcp::{tool_output_schema, tool_result_response};
use crw_core::types::{
    ApiResponse, GroupedSearchData, ImageResult, SearchData, SearchResponseData, SearchResult,
};
use crw_server::app::create_app;
use crw_server::state::{AppState, ExtractRecord, ExtractStatus, UrlResult};
use serde_json::{Value, json};
use std::time::{Duration, Instant, SystemTime};
use uuid::Uuid;

fn test_app() -> TestServer {
    let config: AppConfig = toml::from_str("").unwrap();
    let state = AppState::new(config).expect("AppState::new failed");
    let app = create_app(state);
    TestServer::new(app)
}

/// Like `test_app` but with a SearXNG backend configured, so `crw_search` is
/// advertised in `tools/list` (the URL need not be reachable — advertisement only
/// checks that a backend is configured). The bare `test_app` has no backend, so it
/// correctly suppresses `crw_search` from `tools/list`.
fn test_app_with_search() -> TestServer {
    let config: AppConfig =
        toml::from_str("[search]\nsearxng_url = \"http://localhost:8888\"").unwrap();
    let state = AppState::new(config).expect("AppState::new failed");
    let app = create_app(state);
    TestServer::new(app)
}

/// App state with `[mcp] hide_credits = true`, the self-hosted option that
/// strips credit-billing fields from MCP tool responses.
fn test_state_hide_credits() -> AppState {
    let config: AppConfig = toml::from_str("[mcp]\nhide_credits = true").unwrap();
    AppState::new(config).expect("AppState::new failed")
}

fn pending_extract_record(created_at: Instant) -> ExtractRecord {
    ExtractRecord {
        status: ExtractStatus::Processing,
        data: None,
        per_url: vec![UrlResult {
            url: "https://example.com".into(),
            status: ExtractStatus::Processing,
            data: None,
            error: None,
            llm_usage: None,
            basis: None,
            basis_warnings: Vec::new(),
            llm_input_hash: None,
        }],
        tokens_used: 0,
        credits_used: 0,
        error: None,
        created_at,
        expires_at: SystemTime::now() + Duration::from_secs(3_600),
        claimed_index: None,
    }
}

fn mcp_request(
    method: &str,
    id: serde_json::Value,
    params: serde_json::Value,
) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    })
}

#[tokio::test]
async fn mcp_initialize_returns_capabilities() {
    let server = test_app();
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .json(&mcp_request("initialize", json!(1), json!({})))
        .await;
    resp.assert_status_ok();
    let json: serde_json::Value = resp.json();
    assert_eq!(json["jsonrpc"], "2.0");
    assert_eq!(json["id"], 1);
    let result = &json["result"];
    // T7 — protocol version is bumped to the revision that defines
    // structuredContent/outputSchema (issue #89).
    assert_eq!(result["protocolVersion"], "2025-06-18");
    assert!(result["capabilities"].is_object());
    assert!(result["serverInfo"]["name"].is_string());
    assert!(result["serverInfo"]["version"].is_string());
}

#[tokio::test]
async fn mcp_tools_list_returns_all_tools() {
    // With a search backend configured, all 9 tools are advertised.
    let server = test_app_with_search();
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .json(&mcp_request("tools/list", json!(2), json!({})))
        .await;
    resp.assert_status_ok();
    let json: serde_json::Value = resp.json();
    let tools = json["result"]["tools"].as_array().unwrap();
    assert_eq!(
        tools.len(),
        10,
        "Should have 10 tools including extract start/status/cancel and enrich"
    );

    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(tool_names.contains(&"crw_scrape"));
    assert!(tool_names.contains(&"crw_crawl"));
    assert!(tool_names.contains(&"crw_check_crawl_status"));
    assert!(tool_names.contains(&"crw_map"));
    assert!(tool_names.contains(&"crw_search"));
    assert!(tool_names.contains(&"crw_parse_file"));
    assert!(tool_names.contains(&"crw_extract"));
    assert!(tool_names.contains(&"crw_check_extract_status"));
    assert!(tool_names.contains(&"crw_cancel_extract"));
    assert!(tool_names.contains(&"crw_enrich"));

    // Every tool advertises annotations. Job-starting tools (crw_crawl,
    // crw_extract) have side effects and must NOT be marked read-only.
    let non_read_only = ["crw_crawl", "crw_extract", "crw_cancel_extract"];
    for t in tools {
        assert!(
            t["annotations"].is_object(),
            "{} must advertise annotations",
            t["name"]
        );
        assert!(
            t["title"].is_string(),
            "{} must advertise a title",
            t["name"]
        );
        let name = t["name"].as_str().unwrap();
        let read_only = t["annotations"]["readOnlyHint"].as_bool().unwrap_or(true);
        if non_read_only.contains(&name) {
            assert!(!read_only, "{name} starts a job — must not be readOnly");
        }
    }
    let crawl = tools.iter().find(|t| t["name"] == "crw_crawl").unwrap();
    assert_eq!(crawl["annotations"]["readOnlyHint"], false);
    assert_eq!(crawl["annotations"]["idempotentHint"], false);
    let scrape = tools.iter().find(|t| t["name"] == "crw_scrape").unwrap();
    assert_eq!(scrape["annotations"]["readOnlyHint"], true);
    let cancel = tools
        .iter()
        .find(|t| t["name"] == "crw_cancel_extract")
        .unwrap();
    assert_eq!(cancel["annotations"]["destructiveHint"], true);
    assert_eq!(cancel["annotations"]["idempotentHint"], true);
    for name in [
        "crw_extract",
        "crw_check_extract_status",
        "crw_cancel_extract",
    ] {
        let tool = tools.iter().find(|t| t["name"] == name).unwrap();
        assert!(tool["outputSchema"].is_object(), "{name} outputSchema");
    }
}

/// crw_search is suppressed from tools/list when no search backend is configured,
/// so a no-backend install doesn't advertise a tool that only errors.
#[tokio::test]
async fn mcp_tools_list_hides_search_without_backend() {
    let server = test_app();
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .json(&mcp_request("tools/list", json!(2), json!({})))
        .await;
    resp.assert_status_ok();
    let json: serde_json::Value = resp.json();
    let tools = json["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(tools.len(), 9, "crw_search hidden without a backend");
    assert!(!names.contains(&"crw_search"));
    assert!(names.contains(&"crw_scrape"));
}

#[tokio::test]
async fn mcp_extract_status_and_cancel_share_canonical_http_serializer() {
    let config: AppConfig = toml::from_str("").unwrap();
    let state = AppState::new(config).unwrap();
    let id = Uuid::new_v4();
    state
        .extract_jobs
        .write()
        .await
        .insert(id, pending_extract_record(Instant::now()));
    let server = TestServer::new(create_app(state));

    let status: Value = server
        .post("/mcp")
        .content_type("application/json")
        .json(&mcp_request(
            "tools/call",
            json!(201),
            json!({"name": "crw_check_extract_status", "arguments": {"id": id}}),
        ))
        .await
        .json();
    let status = &status["result"]["structuredContent"];
    assert_eq!(status["id"], id.to_string());
    assert_eq!(status["status"], "processing");
    assert_eq!(status["results"][0]["status"], "processing");
    assert!(status["expiresAt"].is_string());
    // MCP keeps the `success` envelope every other MCP tool returns; it is false
    // only for a job whose every URL failed (here the job is still processing).
    assert_eq!(status["success"], true);
    let schema = tool_output_schema("crw_check_extract_status").unwrap();
    let validator = jsonschema::validator_for(&schema).expect("extract schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(status)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "serializer/schema drift: {errors:#?}");

    let cancelled: Value = server
        .post("/mcp")
        .content_type("application/json")
        .json(&mcp_request(
            "tools/call",
            json!(202),
            json!({"name": "crw_cancel_extract", "arguments": {"id": id}}),
        ))
        .await
        .json();
    let cancelled = &cancelled["result"]["structuredContent"];
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["results"][0]["status"], "cancelled");
    // A cancelled job is not a failed one, so it still reports success: true.
    assert_eq!(cancelled["success"], true);
}

/// `[mcp] hide_credits = true` strips the SaaS credit-billing fields from MCP
/// tool results — in both the text block and `structuredContent` — while
/// `tokensUsed` (real LLM telemetry) survives. The default config must keep
/// emitting `creditsUsed` so existing consumers see no shape change.
#[tokio::test]
async fn mcp_extract_status_hides_credits_when_configured() {
    for (state, expect_credits) in [
        (test_state_hide_credits(), false),
        (
            AppState::new(toml::from_str::<AppConfig>("").unwrap()).unwrap(),
            true,
        ),
    ] {
        let id = Uuid::new_v4();
        let mut rec = pending_extract_record(Instant::now());
        rec.credits_used = 5;
        rec.tokens_used = 42;
        state.extract_jobs.write().await.insert(id, rec);
        let server = TestServer::new(create_app(state));

        let resp: Value = server
            .post("/mcp")
            .content_type("application/json")
            .json(&mcp_request(
                "tools/call",
                json!(301),
                json!({"name": "crw_check_extract_status", "arguments": {"id": id}}),
            ))
            .await
            .json();
        let status = &resp["result"]["structuredContent"];

        if expect_credits {
            assert_eq!(status["creditsUsed"], json!(5));
        } else {
            assert!(
                status.get("creditsUsed").is_none(),
                "creditsUsed must be stripped: {status}"
            );
            // The strip must cover the text block too, not just structuredContent.
            let text = resp["result"]["content"][0]["text"].as_str().unwrap();
            assert!(!text.contains("creditsUsed"), "text block: {text}");
            // A stripped body must still validate against the advertised schema
            // (creditsUsed is schema-optional since this feature).
            let schema = tool_output_schema("crw_check_extract_status").unwrap();
            let validator = jsonschema::validator_for(&schema).expect("schema compiles");
            let errors: Vec<String> = validator
                .iter_errors(status)
                .map(|error| error.to_string())
                .collect();
            assert!(
                errors.is_empty(),
                "stripped body must validate: {errors:#?}"
            );
        }
        // tokensUsed is real LLM provider consumption, never stripped.
        assert_eq!(status["tokensUsed"], json!(42));
        assert_eq!(status["id"], id.to_string());
    }
}

/// The stdio/CLI MCP proxies ship the raw `GET /v1/extract/{id}` REST body
/// verbatim as `structuredContent`, which strict clients validate against the
/// advertised `crw_check_extract_status` outputSchema. Regression lock for issue
/// #318: that REST body was missing the `success` envelope, so every poll failed
/// client-side validation. Also pins `success = (status != failed)` across all
/// five statuses. (The in-server `/mcp` path is covered above; this exercises the
/// REST endpoint the proxies actually call.)
#[tokio::test]
async fn v1_extract_status_rest_body_matches_mcp_output_schema() {
    let config: AppConfig = toml::from_str("").unwrap();
    let state = AppState::new(config).unwrap();
    let server = TestServer::new(create_app(state.clone()));
    let schema = tool_output_schema("crw_check_extract_status").unwrap();
    let validator = jsonschema::validator_for(&schema).expect("extract schema compiles");

    for status in [
        ExtractStatus::Processing,
        ExtractStatus::Cancelling,
        ExtractStatus::Completed,
        ExtractStatus::Failed,
        ExtractStatus::Cancelled,
    ] {
        let id = Uuid::new_v4();
        let mut rec = pending_extract_record(Instant::now());
        rec.status = status;
        state.extract_jobs.write().await.insert(id, rec);

        let body: Value = server.get(&format!("/v1/extract/{id}")).await.json();
        let errors: Vec<String> = validator
            .iter_errors(&body)
            .map(|e| e.to_string())
            .collect();
        assert!(
            errors.is_empty(),
            "{status:?} REST body vs schema: {errors:#?}"
        );
        assert_eq!(
            body["success"],
            Value::Bool(status != ExtractStatus::Failed),
            "success mapping wrong for {status:?}"
        );
    }
}

#[tokio::test]
async fn mcp_extract_output_schemas_match_openapi_lifecycle_components() {
    let openapi: Value =
        serde_json::from_str(include_str!("../openapi/openapi.json")).expect("valid OpenAPI");
    let components = &openapi["components"]["schemas"];

    // Both the MCP outputSchema and the native `/v1` (openapi) shape carry the
    // `success` envelope (issue #318). Its presence+requiredness is asserted on
    // every component below; the field is then stripped so the *rest* of the data
    // contract must still match field-for-field between the two surfaces.
    let has_required_success = |schema: &Value| -> bool {
        schema["properties"]["success"]["type"] == "boolean"
            && schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "success")
    };
    let required_without_success = |schema: &Value| -> Vec<String> {
        schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| *s != "success")
            .map(str::to_string)
            .collect()
    };
    let keys_without_success = |schema: &Value| -> Vec<String> {
        schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| *k != "success")
            .cloned()
            .collect()
    };

    let accepted = tool_output_schema("crw_extract").unwrap();
    let openapi_accepted = &components["ExtractAccepted"];
    assert!(
        has_required_success(&accepted),
        "MCP crw_extract missing success"
    );
    assert!(
        has_required_success(openapi_accepted),
        "openapi ExtractAccepted missing success"
    );
    assert_eq!(
        required_without_success(&accepted),
        required_without_success(openapi_accepted)
    );
    assert_eq!(
        accepted["properties"]["status"],
        openapi_accepted["properties"]["status"]
    );
    assert_eq!(
        keys_without_success(&accepted),
        keys_without_success(openapi_accepted)
    );

    let status = tool_output_schema("crw_check_extract_status").unwrap();
    assert_eq!(status, tool_output_schema("crw_cancel_extract").unwrap());
    let openapi_status = &components["ExtractStatus"];
    assert!(
        has_required_success(&status),
        "MCP crw_check_extract_status missing success"
    );
    assert!(
        has_required_success(openapi_status),
        "openapi ExtractStatus missing success"
    );
    // `creditsUsed` is the one field the two surfaces legitimately disagree on,
    // so it is compared explicitly below instead of field-for-field: REST always
    // emits it (`ExtractStatusResponse.credits_used` is a plain `u32`, no
    // `Option`, no `skip_serializing_if`), so the public OpenAPI contract must
    // keep requiring it; the MCP outputSchema must leave it optional because
    // `[mcp] hide_credits` strips it and the stripped body still has to validate
    // against `additionalProperties: false`. Everything else stays locked.
    let without_credits = |mut required: Vec<String>| -> Vec<String> {
        required.retain(|f| f != "creditsUsed");
        required
    };
    assert_eq!(
        without_credits(required_without_success(&status)),
        without_credits(required_without_success(openapi_status))
    );
    assert!(
        !required_without_success(&status)
            .iter()
            .any(|f| f == "creditsUsed"),
        "MCP schema must leave creditsUsed optional so hide_credits bodies validate"
    );
    assert!(
        required_without_success(openapi_status)
            .iter()
            .any(|f| f == "creditsUsed"),
        "REST always emits creditsUsed, so the OpenAPI contract must keep requiring it"
    );
    assert_eq!(
        status["properties"]["status"],
        openapi_status["properties"]["status"]
    );

    let result = &status["properties"]["results"]["items"];
    let openapi_result = &components["ExtractUrlResult"];
    assert_eq!(result["required"], openapi_result["required"]);
    assert_eq!(
        result["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        openapi_result["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>()
    );
    for field in [
        "url",
        "status",
        "data",
        "error",
        "llmUsage",
        "basis",
        "basisWarnings",
        "llmInputHash",
    ] {
        assert_eq!(
            result["properties"][field]["type"], openapi_result["properties"][field]["type"],
            "type drift for ExtractUrlResult.{field}"
        );
    }
    assert_eq!(
        result["properties"]["status"],
        openapi_result["properties"]["status"]
    );
}

#[tokio::test]
async fn mcp_extract_status_and_cancel_treat_expired_ids_as_not_found() {
    let mut config: AppConfig = toml::from_str("").unwrap();
    config.crawler.job_ttl_secs = 1;
    let state = AppState::new(config).unwrap();
    let ids = [Uuid::new_v4(), Uuid::new_v4()];
    let expired = pending_extract_record(Instant::now() - Duration::from_secs(2));
    {
        let mut jobs = state.extract_jobs.write().await;
        jobs.insert(ids[0], expired.clone());
        jobs.insert(ids[1], expired);
    }
    let server = TestServer::new(create_app(state.clone()));

    for (tool, id) in [
        ("crw_check_extract_status", ids[0]),
        ("crw_cancel_extract", ids[1]),
    ] {
        let response: Value = server
            .post("/mcp")
            .content_type("application/json")
            .json(&mcp_request(
                "tools/call",
                json!(301),
                json!({"name": tool, "arguments": {"id": id}}),
            ))
            .await
            .json();
        assert_eq!(response["result"]["isError"], true, "{tool}");
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("not found"),
            "{tool}"
        );
        assert!(response["result"].get("structuredContent").is_none());
    }
    assert!(state.extract_jobs.read().await.is_empty());
}

#[tokio::test]
async fn mcp_unknown_method_error_32601() {
    let server = test_app();
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .json(&mcp_request("nonexistent/method", json!(3), json!({})))
        .await;
    resp.assert_status_ok();
    let json: serde_json::Value = resp.json();
    assert_eq!(json["error"]["code"], -32601);
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("method not found")
    );
}

#[tokio::test]
async fn mcp_invalid_json_error_32700() {
    let server = test_app();
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .bytes("this is not valid json".into())
        .await;
    resp.assert_status_ok();
    let json: serde_json::Value = resp.json();
    assert_eq!(json["error"]["code"], -32700);
}

#[tokio::test]
async fn mcp_ping_returns_empty() {
    let server = test_app();
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .json(&mcp_request("ping", json!(4), json!({})))
        .await;
    resp.assert_status_ok();
    let json: serde_json::Value = resp.json();
    assert_eq!(json["result"], json!({}));
}

#[tokio::test]
async fn mcp_notification_returns_202() {
    let server = test_app();
    let body = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .json(&body)
        .await;
    resp.assert_status(axum::http::StatusCode::ACCEPTED);
}

#[tokio::test]
async fn mcp_wrong_content_type_400() {
    let server = test_app();
    let resp = server
        .post("/mcp")
        .content_type("text/plain")
        .bytes(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.into())
        .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn mcp_tools_call_unknown_tool() {
    let server = test_app();
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .json(&mcp_request(
            "tools/call",
            json!(5),
            json!({"name": "nonexistent_tool", "arguments": {}}),
        ))
        .await;
    resp.assert_status_ok();
    let json: serde_json::Value = resp.json();
    // Unknown tool name → JSON-RPC -32602 protocol error (not an isError result).
    assert_eq!(json["error"]["code"], -32602);
    assert!(json.get("result").is_none() || json["result"].is_null());
}

#[tokio::test]
async fn mcp_null_id() {
    let server = test_app();
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .json(&mcp_request("ping", json!(null), json!({})))
        .await;
    resp.assert_status_ok();
    let json: serde_json::Value = resp.json();
    assert!(json["id"].is_null());
}

#[tokio::test]
async fn mcp_integer_id() {
    let server = test_app();
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .json(&mcp_request("ping", json!(42), json!({})))
        .await;
    resp.assert_status_ok();
    let json: serde_json::Value = resp.json();
    assert_eq!(json["id"], 42);
}

#[tokio::test]
async fn mcp_crw_scrape_advertises_renderer_in_tools_list() {
    let server = test_app();
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .json(&mcp_request("tools/list", json!(99), json!({})))
        .await;
    resp.assert_status_ok();
    let json: serde_json::Value = resp.json();
    let tools = json["result"]["tools"].as_array().unwrap();
    let scrape = tools
        .iter()
        .find(|t| t["name"] == "crw_scrape")
        .expect("crw_scrape tool");
    let renderer = &scrape["inputSchema"]["properties"]["renderer"];
    assert_eq!(renderer["type"], "string");
    let enum_vals = renderer["enum"].as_array().expect("renderer.enum");
    assert_eq!(enum_vals.len(), 6);
    for v in [
        "auto",
        "lightpanda",
        "chrome",
        "playwright",
        "camoufox",
        "impersonated-http",
    ] {
        assert!(
            enum_vals.iter().any(|e| e == v),
            "renderer enum missing {v}"
        );
    }
}

#[tokio::test]
async fn mcp_crw_crawl_renderer_unavailable_returns_tool_error() {
    // mcp tools/call → crw_crawl with unavailable renderer should surface an
    // error via the MCP tool-error wrapper (isError:true), mirroring the HTTP
    // route's pre-acceptance 400.
    let server = test_app();
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .json(&mcp_request(
            "tools/call",
            json!(100),
            json!({
                "name": "crw_crawl",
                "arguments": {"url": "https://example.com", "renderer": "chrome"}
            }),
        ))
        .await;
    resp.assert_status_ok();
    let json: serde_json::Value = resp.json();
    let result = &json["result"];
    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("renderer 'chrome' not available"),
        "expected pinned-renderer error in MCP tool error, got: {text}"
    );
}

#[tokio::test]
async fn mcp_missing_method_field() {
    let server = test_app();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "params": {}
    });
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .json(&body)
        .await;
    resp.assert_status_ok();
    let json: serde_json::Value = resp.json();
    // Should be a parse error since "method" is required in JsonRpcRequest
    assert_eq!(json["error"]["code"], -32700);
}

// --- structuredContent / outputSchema (issue #89) ---

/// T8 — tools/list advertises crw_search with the corrected nested-shape
/// outputSchema (data is an object whose `results` is required), not the old
/// flat-array shape.
#[tokio::test]
async fn mcp_t8_search_advertises_nested_output_schema() {
    let server = test_app_with_search();
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .json(&mcp_request("tools/list", json!(8), json!({})))
        .await;
    resp.assert_status_ok();
    let json: serde_json::Value = resp.json();
    let tools = json["result"]["tools"].as_array().unwrap();
    let search = tools
        .iter()
        .find(|t| t["name"] == "crw_search")
        .expect("crw_search tool");
    let schema = &search["outputSchema"];
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["properties"]["data"]["type"], "object");
    let data_required = schema["properties"]["data"]["required"]
        .as_array()
        .expect("data.required");
    assert!(data_required.iter().any(|v| v == "results"));
}

/// T9 — tools/call crw_search with no SearXNG configured returns a tool error
/// (search disabled), surfaced as isError text with no structuredContent.
#[tokio::test]
async fn mcp_t9_search_disabled_is_tool_error() {
    let server = test_app();
    let resp = server
        .post("/mcp")
        .content_type("application/json")
        .json(&mcp_request(
            "tools/call",
            json!(9),
            json!({"name": "crw_search", "arguments": {"query": "anything"}}),
        ))
        .await;
    resp.assert_status_ok();
    let json: serde_json::Value = resp.json();
    let result = &json["result"];
    assert_eq!(result["isError"], true);
    assert!(result["content"][0]["text"].is_string());
    assert!(result.get("structuredContent").is_none());
}

/// T10 — the MCP-Protocol-Version header is tolerated: present-correct,
/// present-mismatched, malformed, and absent all succeed (no reject branch).
#[tokio::test]
async fn mcp_t10_protocol_version_header_tolerated() {
    let server = test_app();
    let name = HeaderName::from_static("mcp-protocol-version");
    for header in [
        Some("2025-06-18"),
        Some("2024-11-05"),
        Some("not-a-version"),
        None,
    ] {
        let mut req = server.post("/mcp").content_type("application/json");
        if let Some(v) = header {
            req = req.add_header(name.clone(), HeaderValue::from_static(v));
        }
        let resp = req.json(&mcp_request("ping", json!(10), json!({}))).await;
        resp.assert_status_ok();
        let json: serde_json::Value = resp.json();
        assert_eq!(
            json["result"],
            json!({}),
            "ping must succeed regardless of MCP-Protocol-Version header = {header:?}"
        );
    }
}

// --- T12: real-serializer gate ---

fn real_search_result(idx: u32) -> SearchResult {
    SearchResult {
        url: format!("https://example.com/{idx}"),
        title: format!("Result {idx}"),
        description: "body text".into(),
        snippet: "body text".into(),
        position: idx,
        score: Some(4.0),
        published_date: None,
        category: Some("general".into()),
        markdown: None,
        html: None,
        raw_html: None,
        links: None,
        metadata: None,
        summary: None,
        error: None,
        truncated: None,
    }
}

fn real_image_result(idx: u32) -> ImageResult {
    ImageResult {
        url: format!("https://example.com/img/{idx}"),
        title: format!("Image {idx}"),
        description: "alt text".into(),
        image_url: format!("https://example.com/img/{idx}.png"),
        position: idx,
        thumbnail_url: None,
        image_format: None,
        resolution: None,
    }
}

fn envelope(results: SearchData) -> SearchResponseData {
    SearchResponseData {
        results,
        answer: None,
        citations: Vec::new(),
        llm_usage: None,
        warnings: Vec::new(),
    }
}

/// Emit `value` through the real MCP wrapper and return the structuredContent it
/// produced (mirrors what the server sends on the wire).
fn structured_content_for(value: Value) -> Value {
    let resp = tool_result_response(json!(1), "crw_search", Ok(value));
    resp.result
        .expect("success response")
        .get("structuredContent")
        .cloned()
        .expect("crw_search emits structuredContent for an object value")
}

/// T12 — validate the REAL `SearchResponse` serializer output (untagged enum,
/// camelCase, every skip_serializing_if) against the declared outputSchema, on
/// every branch: flat populated, flat empty, grouped, grouped empty. This is
/// the gate that the original #89 schema-vs-reality drift would have failed.
#[tokio::test]
async fn mcp_t12_real_serializer_validates_against_output_schema() {
    let schema = tool_output_schema("crw_search").expect("crw_search outputSchema");
    let validator = jsonschema::validator_for(&schema).expect("schema compiles");

    let cases: Vec<(&str, SearchData)> = vec![
        (
            "A: flat populated",
            SearchData::Flat(vec![real_search_result(1), real_search_result(2)]),
        ),
        ("B: flat empty", SearchData::Flat(vec![])),
        (
            "C: grouped web+news+images",
            SearchData::Grouped(GroupedSearchData {
                web: Some(vec![real_search_result(1)]),
                news: Some(vec![real_search_result(2)]),
                images: Some(vec![real_image_result(1)]),
            }),
        ),
        (
            "D: grouped empty",
            SearchData::Grouped(GroupedSearchData::default()),
        ),
    ];

    for (label, results) in cases {
        let is_empty_grouped = matches!(&results, SearchData::Grouped(g) if g.web.is_none() && g.news.is_none() && g.images.is_none());
        let response = ApiResponse::ok(envelope(results));
        let value = serde_json::to_value(&response).expect("serialize SearchResponse");

        // Sanity on case D: an empty grouped envelope serializes results to `{}`.
        if is_empty_grouped {
            assert_eq!(
                value["data"]["results"],
                json!({}),
                "empty grouped results must serialize to an empty object"
            );
        }

        let structured = structured_content_for(value.clone());
        let errors: Vec<String> = validator
            .iter_errors(&structured)
            .map(|e| e.to_string())
            .collect();
        assert!(
            errors.is_empty(),
            "[{label}] real serializer output failed schema validation:\n{}\nvalue: {value}",
            errors.join("\n")
        );
    }
}
