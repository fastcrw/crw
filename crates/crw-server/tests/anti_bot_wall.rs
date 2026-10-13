//! A wall no renderer tier could clear, through the real router.
//!
//! The renderer used to raise this wall as `CrwError::HttpError`, which `/v1`
//! answered with a 502 and `/v2` with a 500 `SCRAPE_ALL_ENGINES_FAILED`. The
//! SaaS turned both into a 502, the one status the Firecrawl SDKs retry. It is
//! a target refusing us, so it now has the contract every other wall already
//! has: HTTP 200, `success: false`, the body cleared, and nothing billed.
//!
//! The origin is a mock serving the shape of `mock.fastcrw.com/wall/captcha`
//! (403, "Verify you are human") through the real HTTP tier. Only the browser
//! tier is stubbed, because nothing else reaches the ladder tail.

use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::TestServer;
use crw_core::config::{AppConfig, RendererConfig, RendererMode, StealthConfig};
use crw_renderer::FallbackRenderer;
use crw_renderer::test_utils::StaticTier;
use crw_renderer::traits::PageFetcher;
use crw_server::app::create_app;
use crw_server::state::AppState;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CAPTCHA_WALL: &str = "<!doctype html><html><head><title>captcha</title></head><body>\
    <h1>Verify you are human</h1><div class=\"g-recaptcha\" data-sitekey=\"mock\"></div>\
    </body></html>";

/// wiremock binds to loopback, which the SSRF guard rejects by default.
fn allow_loopback() {
    // SAFETY: set before any request runs; every test in this file sets the
    // same value, so concurrent tests cannot observe a different one.
    unsafe {
        std::env::set_var("CRW_ALLOW_LOOPBACK_FOR_TESTS", "1");
    }
}

/// The real router over a real HTTP tier, with or without a browser tier that
/// always lands on the wall.
fn app(with_browser: bool) -> TestServer {
    allow_loopback();
    let config: AppConfig = toml::from_str("").unwrap();
    let mut state = AppState::new(config).expect("AppState::new failed");
    let mut cfg = RendererConfig {
        mode: RendererMode::None,
        ..Default::default()
    };
    cfg.impersonated.enabled = false;
    let mut renderer = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default())
        .expect("renderer builds");
    if with_browser {
        let browser = Arc::new(StaticTier {
            name: "chrome",
            status_code: 403,
            html: CAPTCHA_WALL.to_string(),
        }) as Arc<dyn PageFetcher>;
        renderer = renderer.with_js_tiers(vec![browser]);
    }
    state.renderer = Arc::new(renderer);
    TestServer::new(create_app(state))
}

async fn walled_origin() -> (MockServer, String) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/wall/captcha"))
        .respond_with(ResponseTemplate::new(403).set_body_raw(CAPTCHA_WALL, "text/html"))
        .mount(&server)
        .await;
    let url = format!("{}/wall/captcha", server.uri());
    (server, url)
}

#[tokio::test]
async fn v1_answers_a_wall_no_tier_could_clear_with_200_anti_bot() {
    let (_origin, url) = walled_origin().await;
    let s = app(true);

    let r = s
        .post("/v1/scrape")
        .json(&json!({"url": url, "formats": ["markdown"]}))
        .await;

    r.assert_status(StatusCode::OK);
    let body: Value = r.json();
    assert_eq!(body["success"], false, "{body}");
    assert_eq!(body["errorCode"], "anti_bot", "{body}");
    let error = body["error"].as_str().unwrap_or_default();
    assert!(error.starts_with("Blocked by anti-bot ("), "{body}");
    assert!(body["data"]["block"]["vendor"].is_string(), "{body}");
    assert!(
        body["data"].get("markdown").is_none(),
        "wall text leaked: {body}"
    );
    assert_eq!(body["data"]["metadata"]["statusCode"], 403, "{body}");
}

/// Root `/v2` and `/firecrawl/v2` share one router. Both must answer with the
/// existing blocked envelope: the same three keys, no `code`, no `errorCode`.
#[tokio::test]
async fn v2_answers_a_wall_no_tier_could_clear_with_the_blocked_envelope() {
    let (_origin, url) = walled_origin().await;
    let s = app(true);

    for route in ["/v2/scrape", "/firecrawl/v2/scrape"] {
        let r = s
            .post(route)
            .json(&json!({"url": url, "formats": ["markdown"]}))
            .await;

        r.assert_status(StatusCode::OK);
        let body: Value = r.json();
        let mut keys: Vec<&str> = body
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["data", "error", "success"], "{route}: {body}");
        assert_eq!(body["success"], false, "{route}: {body}");
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            error.starts_with("Blocked by anti-bot ("),
            "{route}: {body}"
        );
        assert!(
            body["data"].get("markdown").is_none(),
            "{route}: wall text leaked: {body}"
        );
        assert_eq!(body["data"]["metadata"]["statusCode"], 403, "{route}");
        // Nothing is billed for it: the SaaS refunds on `success: false`, and
        // the document advertises no credit either.
        assert_eq!(body["data"]["metadata"]["creditsUsed"], 0, "{route}");
    }
}

/// The billing guard's other half. With no browser tier nothing condemned the
/// page, so `/v2` keeps today's answer for a 403: a `success: true` document,
/// billed as before. Only a wall the ladder gave up on changes.
#[tokio::test]
async fn v2_keeps_a_4xx_page_no_ladder_condemned_as_a_document() {
    let (_origin, url) = walled_origin().await;
    let s = app(false);

    let r = s
        .post("/v2/scrape")
        .json(&json!({"url": url, "formats": ["markdown"]}))
        .await;

    r.assert_status(StatusCode::OK);
    let body: Value = r.json();
    assert_eq!(body["success"], true, "{body}");
    assert_eq!(body["data"]["metadata"]["statusCode"], 403, "{body}");
}
