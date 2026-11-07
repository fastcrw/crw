//! Offline tests for `POST /v1/enrich`: request validation, the capabilities
//! advertisement, and one full row against a mock page and a mock LLM.

use axum::http::StatusCode;
use axum_test::TestServer;
use crw_core::config::AppConfig;
use crw_server::app::create_app;
use crw_server::state::AppState;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn app(config: AppConfig) -> TestServer {
    TestServer::new(create_app(AppState::new(config).expect("AppState::new")))
}

fn no_llm() -> TestServer {
    app(toml::from_str("").unwrap())
}

fn llm_config(base_url: &str) -> AppConfig {
    let mut config: AppConfig = toml::from_str(
        r#"
[extraction.llm]
api_key = "test-key"
provider = "anthropic"
model = "claude-test"
"#,
    )
    .unwrap();
    config.extraction.llm.as_mut().unwrap().base_url = Some(base_url.to_string());
    config
}

fn ceo_column() -> Value {
    json!([{ "name": "ceo", "description": "Current CEO", "type": "text" }])
}

#[tokio::test]
async fn rejects_more_rows_than_one_call_takes() {
    let r = no_llm()
        .post("/v1/enrich")
        .json(&json!({ "rows": [{}, {}, {}, {}], "columns": ceo_column() }))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
    assert!(r.text().contains("too many rows"));
}

#[tokio::test]
async fn rejects_an_unknown_column_type() {
    let r = no_llm()
        .post("/v1/enrich")
        .json(&json!({ "rows": [{}], "columns": [{ "name": "x", "type": "date" }] }))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(r.json::<Value>()["errorCode"], json!("invalid_request"));
}

/// A column set whose evidence cannot fit the model's output cap is refused
/// before any page is fetched.
#[tokio::test]
async fn refuses_columns_that_cannot_fit_the_output_cap_upfront() {
    let mut config = llm_config("http://127.0.0.1:9");
    config.extraction.llm.as_mut().unwrap().max_tokens = 900;
    let r = app(config)
        .post("/v1/enrich")
        .json(&json!({
            "rows": [{ "company": "Stripe", "website": "stripe.com" }],
            "columns": [{ "name": "ceo" }, { "name": "hq" }, { "name": "founded" }]
        }))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
    assert!(r.text().contains("basis_schema_too_large"), "{}", r.text());
}

#[tokio::test]
async fn capabilities_report_enrich_supported_with_a_server_llm() {
    let body: Value = app(llm_config("http://127.0.0.1:9"))
        .get("/v1/capabilities")
        .await
        .json();
    assert_eq!(body["enrich"]["supported"], json!(true));
}

#[tokio::test]
async fn mcp_crw_enrich_runs_the_same_validation() {
    let r: Value = no_llm()
        .post("/mcp")
        .content_type("application/json")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "crw_enrich", "arguments": {
                "rows": [{}, {}, {}, {}], "columns": [{ "name": "ceo" }]
            }}
        }))
        .await
        .json();
    assert_eq!(r["result"]["isError"], json!(true), "{r}");
    assert!(r.to_string().contains("too many rows"), "{r}");
}

#[tokio::test]
async fn rejects_a_request_without_any_llm() {
    let r = no_llm()
        .post("/v1/enrich")
        .json(&json!({ "rows": [{ "company": "Stripe" }], "columns": ceo_column() }))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
    assert!(r.text().contains("requires an LLM"));
}

#[tokio::test]
async fn rejects_base_url() {
    let r = no_llm()
        .post("/v1/enrich")
        .json(&json!({ "rows": [{}], "columns": ceo_column(), "baseUrl": "http://x" }))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn capabilities_report_enrich_limits() {
    let body: Value = no_llm().get("/v1/capabilities").await.json();
    assert_eq!(body["enrich"]["supported"], json!(false));
    assert_eq!(body["enrich"]["maxRows"], json!(3));
    assert_eq!(body["enrich"]["maxColumns"], json!(10));
    assert_eq!(body["enrich"]["search"], json!(false));
}

/// The row's own website fills the cell; the citation carries the fetched url
/// and a verbatim excerpt; a column the page does not state comes back
/// `notFound`; the row id is echoed and the page is reported as billable.
#[tokio::test]
async fn fills_a_row_from_its_own_website_with_verified_evidence() {
    unsafe {
        std::env::set_var("CRW_ALLOW_LOOPBACK_FOR_TESTS", "1");
    }
    let mock = MockServer::start().await;
    let page = format!("{}/about", mock.uri());
    Mock::given(method("GET"))
        .and(path("/about"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html")
                .set_body_string(
                    "<html><body><article><h1>About Stripe</h1>\
                     <p>Patrick Collison is the CEO of Stripe.</p></article></body></html>",
                ),
        )
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{
                "type": "tool_use",
                "id": "t1",
                "name": "extract_data",
                "input": {
                    "ceo": "Patrick Collison",
                    "hq": null,
                    "basis": {
                        "ceo": {
                            "value": "Patrick Collison",
                            "sourceUrl": page,
                            "excerpt": "Patrick Collison is the CEO of Stripe.",
                            "confidence": "high"
                        },
                        "hq": { "value": null, "sourceUrl": page, "excerpt": null }
                    }
                }
            }],
            "usage": { "input_tokens": 40, "output_tokens": 20 }
        })))
        .mount(&mock)
        .await;

    let r = app(llm_config(&mock.uri()))
        .post("/v1/enrich")
        .json(&json!({
            "rows": [{ "id": "r1", "company": "Stripe", "website": page, "ceo": null, "hq": "" }],
            "columns": [
                { "name": "ceo", "description": "Current CEO", "type": "text" },
                { "name": "hq", "description": "Headquarters city", "type": "text" }
            ]
        }))
        .await;
    r.assert_status_ok();
    let body: Value = r.json();
    assert_eq!(body["success"], json!(true));
    assert_eq!(body["status"], json!("completed"));
    let row = &body["data"]["rows"][0];
    assert_eq!(row["rowId"], json!("r1"));
    assert_eq!(row["status"], json!("completed"), "{row}");
    let ceo = &row["cells"]["ceo"];
    assert_eq!(ceo["status"], json!("supported"), "{row}");
    assert_eq!(ceo["value"], json!("Patrick Collison"));
    assert_eq!(ceo["citations"][0]["url"], json!(page));
    assert!(
        ceo["citations"][0]["excerpt"]
            .as_str()
            .unwrap()
            .contains("Patrick Collison")
    );
    assert_eq!(row["cells"]["hq"]["status"], json!("notFound"), "{row}");
    assert_eq!(row["cells"]["hq"]["value"], Value::Null);
    assert_eq!(row["usage"]["pages"], json!([page]));
    assert_eq!(row["usage"]["searches"], json!(0));
    assert!(row["usage"]["llmUsage"].is_object());
}
