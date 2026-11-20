//! Offline integration tests for the Firecrawl `/v2/*` surface (issue #62).
//!
//! These avoid the network: they exercise request parsing, error envelopes,
//! unknown-job 404s, auth parity, and — by constructing the router at all —
//! confirm the `/v2/crawl/active` vs `/v2/crawl/{id}` overlap doesn't panic.
//! End-to-end scrape/crawl/map content is covered by the conformance harness.

use axum::http::{HeaderValue, StatusCode};
use axum_test::TestServer;
use crw_core::config::AppConfig;
use crw_server::app::create_app;
use crw_server::state::AppState;
use serde_json::{Value, json};

fn test_app() -> TestServer {
    let config: AppConfig = toml::from_str("").unwrap();
    let state = AppState::new(config).expect("AppState::new failed");
    TestServer::new(create_app(state))
}

fn test_app_with_auth(keys: &[&str]) -> TestServer {
    let toml_str = format!("[auth]\napi_keys = {keys:?}");
    let config: AppConfig = toml::from_str(&toml_str).unwrap();
    let state = AppState::new(config).expect("AppState::new failed");
    TestServer::new(create_app(state))
}

#[tokio::test]
async fn v2_scrape_invalid_url_400() {
    let s = test_app();
    let r = s
        .post("/v2/scrape")
        .json(&json!({"url": "not-a-url", "formats": ["markdown"]}))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
    let body: Value = r.json();
    assert_eq!(body["success"], false);
}

#[tokio::test]
async fn v2_scrape_object_formats_deserialize() {
    // Object-form formats (the headline v1→v2 delta) must DESERIALIZE — reaching
    // URL validation (400) rather than a body-decode 422 proves the v2 format
    // objects parsed successfully.
    let s = test_app();
    let r = s
        .post("/v2/scrape")
        .json(&json!({
            "url": "not-a-url",
            "formats": [{"type": "json", "schema": {"type": "object"}}, {"type": "summary"}]
        }))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn v2_scrape_tolerates_unknown_sdk_fields() {
    // The SDK sends fields we don't model (origin, integration, mobile, …). We
    // must NOT reject them — serde ignores unknowns (no deny_unknown_fields).
    let s = test_app();
    let r = s
        .post("/v2/scrape")
        .json(&json!({
            "url": "not-a-url",
            "origin": "api",
            "integration": "_sdk",
            "mobile": true,
            "storeInCache": true,
            "maxAge": 3600
        }))
        .await;
    // 400 from URL parse, NOT 422 from an unknown-field rejection.
    r.assert_status(StatusCode::BAD_REQUEST);
}

// maxAge and storeInCache used to be unknown-and-ignored. Typing them must not
// turn an SDK that sends the wrong shape into a 400 on a request that worked.
#[tokio::test]
async fn v2_scrape_tolerates_a_badly_typed_max_age() {
    let s = test_app();
    let r = s
        .post("/v2/scrape")
        .json(&json!({
            "url": "not-a-url",
            "maxAge": "3600000",
            "storeInCache": "false"
        }))
        .await;
    // 400 from URL parse, NOT 422 from a type rejection.
    r.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn v2_map_invalid_url_400() {
    let s = test_app();
    let r = s.post("/v2/map").json(&json!({"url": "not-a-url"})).await;
    r.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn v2_extract_requires_urls_400() {
    let s = test_app();
    let r = s
        .post("/v2/extract")
        .json(&json!({"prompt": "get the title"}))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn v2_extract_rejects_system_prompt() {
    // systemPrompt is a distinct Firecrawl feature with no slot in the engine
    // yet. It must be rejected loudly, not accepted and silently ignored.
    let s = test_app();
    let r = s
        .post("/v2/extract")
        .json(&json!({
            "urls": ["http://example.com"],
            "prompt": "get the title",
            "systemPrompt": "you are a careful extractor",
        }))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
    assert!(r.text().contains("systemPrompt"), "body: {}", r.text());
}

#[tokio::test]
async fn v2_extract_blank_system_prompt_is_tolerated() {
    // An empty systemPrompt is not a real instruction — do not 400 on it, or a
    // client that always sends the key gets rejected for nothing.
    let s = test_app();
    let r = s
        .post("/v2/extract")
        .json(&json!({
            "urls": ["http://example.com"],
            "prompt": "get the title",
            "systemPrompt": "   ",
        }))
        .await;
    // Not a 400 for the systemPrompt reason (it will start a job or fail on SSRF,
    // either way not our rejection).
    assert_ne!(
        r.status_code(),
        StatusCode::BAD_REQUEST,
        "blank systemPrompt must not be rejected: {}",
        r.text()
    );
}

#[tokio::test]
async fn v2_batch_requires_urls_400() {
    let s = test_app();
    let r = s.post("/v2/batch/scrape").json(&json!({"urls": []})).await;
    r.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn v2_crawl_status_unknown_404() {
    let s = test_app();
    let r = s
        .get("/v2/crawl/00000000-0000-0000-0000-000000000000")
        .await;
    r.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn v2_batch_status_unknown_404() {
    let s = test_app();
    let r = s
        .get("/v2/batch/scrape/00000000-0000-0000-0000-000000000000")
        .await;
    r.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn v2_extract_status_unknown_404() {
    let s = test_app();
    let r = s
        .get("/v2/extract/00000000-0000-0000-0000-000000000000")
        .await;
    r.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn v2_scrape_job_stub_404() {
    let s = test_app();
    let r = s.get("/v2/scrape/some-job-id").await;
    r.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn v2_crawl_active_routes_ok() {
    // Confirms the static `/v2/crawl/active` route coexists with `/v2/crawl/{id}`
    // (router built without panic) and returns the active-jobs envelope.
    let s = test_app();
    let r = s.get("/v2/crawl/active").await;
    r.assert_status_ok();
    let body: Value = r.json();
    assert_eq!(body["success"], true);
    assert!(body["crawls"].is_array());
}

#[tokio::test]
async fn v2_scrape_requires_auth_when_keys_set() {
    let s = test_app_with_auth(&["secret-key"]);
    let r = s
        .post("/v2/scrape")
        .json(&json!({"url": "https://example.com"}))
        .await;
    r.assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn v2_scrape_auth_ok_reaches_handler() {
    let s = test_app_with_auth(&["secret-key"]);
    let r = s
        .post("/v2/scrape")
        .add_header(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-key"),
        )
        .json(&json!({"url": "not-a-url"}))
        .await;
    // Passed auth, hit the handler, which 400s on the bad URL.
    assert_ne!(r.status_code(), StatusCode::UNAUTHORIZED);
}

// --- Firecrawl-compat namespace (`/firecrawl/*`) ---
// The frozen Firecrawl drop-in surface re-mounts the same v1/v2 handlers under
// a `/firecrawl` prefix, leaving root `/v1` (native) and `/v2` (deprecated
// alias) untouched. These tests prove the prefix routes to the same handlers.

#[tokio::test]
async fn firecrawl_v2_scrape_routes_to_handler() {
    let s = test_app();
    let r = s
        .post("/firecrawl/v2/scrape")
        .json(&json!({"url": "not-a-url", "formats": ["markdown"]}))
        .await;
    // Reached the v2 handler (400 on bad URL), not a 404 from a missing route.
    r.assert_status(StatusCode::BAD_REQUEST);
    let body: Value = r.json();
    assert_eq!(body["success"], false);
}

#[tokio::test]
async fn firecrawl_v1_scrape_routes_to_handler() {
    let s = test_app();
    let r = s
        .post("/firecrawl/v1/scrape")
        .json(&json!({"url": "not-a-url"}))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn firecrawl_prefix_unknown_route_404() {
    let s = test_app();
    let r = s.post("/firecrawl/v2/nope").json(&json!({})).await;
    r.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn v2_batch_url_cap_400() {
    // Sanity cap: max_batch_urls rejects oversized lists BEFORE any
    // per-URL validation work (the check is O(1) on the list length).
    let config: AppConfig = toml::from_str("[crawler]\nmax_batch_urls = 5").unwrap();
    let state = AppState::new(config).expect("AppState::new failed");
    let s = TestServer::new(create_app(state));
    let urls: Vec<String> = (0..6).map(|i| format!("https://example.com/{i}")).collect();
    let r = s
        .post("/v2/batch/scrape")
        .json(&json!({ "urls": urls }))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
    let body: Value = r.json();
    assert_eq!(body["success"], false);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("maximum of 5"),
        "error should name the cap: {body}"
    );
}

#[tokio::test]
async fn v2_batch_cancel_flips_status_to_cancelled() {
    // Cancel must move the job to a terminal "cancelled" state — previously it
    // only aborted the task, leaving polls at "scraping" until TTL eviction
    // (SDK waiters hung). Start the job at the state layer with a TEST-NET-1
    // blackhole IP so it deterministically stays InProgress until we cancel.
    let config: AppConfig = toml::from_str("").unwrap();
    let state = AppState::new(config).expect("AppState::new failed");
    let s = TestServer::new(create_app(state.clone()));

    let template = crw_core::types::ScrapeRequest::default();
    let id = state
        .start_batch_job(vec!["http://192.0.2.1/".into()], template, None)
        .await;

    // Cancel via the public route.
    let r = s.delete(&format!("/v2/batch/scrape/{id}")).await;
    r.assert_status_ok();
    let body: Value = r.json();
    assert_eq!(body["status"], "cancelled");

    // Status poll now reports the terminal state (not "scraping").
    let g = s.get(&format!("/v2/batch/scrape/{id}")).await;
    g.assert_status_ok();
    let gb: Value = g.json();
    assert_eq!(gb["status"], "cancelled");

    // Re-cancel is rejected as already finished.
    let r2 = s.delete(&format!("/v2/batch/scrape/{id}")).await;
    r2.assert_status(StatusCode::BAD_REQUEST);
}

// --- browser actions on the Firecrawl surface vs the frozen root alias ---
// Both paths share one handler. A public IP literal passes URL validation
// without DNS, and every request below is rejected before any fetch.

#[tokio::test]
async fn firecrawl_v2_scrape_runs_actions_validation() {
    let s = test_app();
    let r = s
        .post("/firecrawl/v2/scrape")
        .json(&json!({"url": "http://93.184.215.14/", "actions": [{"type": "hover"}]}))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
    let body: Value = r.json();
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("actions[0]")),
        "got: {body}"
    );
}

#[tokio::test]
async fn root_v2_scrape_still_ignores_actions() {
    // The same invalid list, plus a contradiction the scrape itself rejects. If
    // the alias read `actions`, its error would come first; it must not.
    let s = test_app();
    let r = s
        .post("/v2/scrape")
        .json(&json!({
            "url": "http://93.184.215.14/",
            "formats": ["screenshot"],
            "renderJs": false,
            "actions": [{"type": "hover"}]
        }))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
    let body: Value = r.json();
    let err = body["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("screenshot") && !err.contains("actions"),
        "got: {body}"
    );
}

#[tokio::test]
async fn v1_scrape_rejects_actions_with_render_js_false() {
    let s = test_app();
    let r = s
        .post("/v1/scrape")
        .json(&json!({
            "url": "http://93.184.215.14/",
            "renderJs": false,
            "actions": [{"type": "wait", "milliseconds": 100}]
        }))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
    let body: Value = r.json();
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("run in a browser")),
        "got: {body}"
    );
}

#[tokio::test]
async fn v1_scrape_names_the_supported_types_for_a_firecrawl_output_action() {
    let s = test_app();
    let r = s
        .post("/v1/scrape")
        .json(&json!({"url": "http://93.184.215.14/", "actions": [{"type": "screenshot"}]}))
        .await;
    r.assert_status(StatusCode::BAD_REQUEST);
    let body: Value = r.json();
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("not supported") && e.contains("formats")),
        "got: {body}"
    );
}
