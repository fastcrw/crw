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
            truncated: false,
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

/// Serve `body` with `status` at `path` and return its URL.
async fn serve(origin: &MockServer, at: &str, status: u16, body: &str) -> String {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(status).set_body_raw(body.to_string(), "text/html"))
        .mount(origin)
        .await;
    format!("{}{at}", origin.uri())
}

/// The fields that decide what a scrape answer is and what it costs.
async fn answer(s: &TestServer, route: &str, url: &str) -> Value {
    let r = s
        .post(route)
        .json(&json!({"url": url, "formats": ["markdown"]}))
        .await;
    r.assert_status(StatusCode::OK);
    let body: Value = r.json();
    json!({
        "success": body["success"],
        "error": body["error"],
        "errorCode": body["errorCode"],
        "code": body["code"],
        "block": body["data"]["block"],
        "markdown": body["data"]["markdown"],
        "statusCode": body["data"]["metadata"]["statusCode"],
        "creditsUsed": body["data"]["metadata"]["creditsUsed"],
    })
}

/// The shape of hyatt's `.pdf` 403 (Akamai): a deny page whose wording matches
/// no wall phrase list, so only its status says what it is.
const AKAMAI_UNEXPECTED: &str = "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
    <title>Hyatt</title></head><body><h1>Your browser did something unexpected.</h1>\
    <p>Please try again. If the problem persists, contact us and share the reference ID \
    below.</p><p>Reference ID: 0.8f3e1702.1760400000.1a2b3c4d</p></body></html>";

/// A 4xx on a PDF path never reaches the browser ladder: the auto arm returns
/// the HTTP verdict, since no browser renders a PDF. A hard-block status used
/// to send it up the ladder anyway, where it ended as a refunded error, so it
/// stays refunded on every surface. Keyed on the status, so it holds for bodies
/// that are not walls at all.
///
/// - `/v1` keeps main's own answer, the one an engine with no browser tier
///   gives: `http_error` with the body, already refunded.
/// - `/v2` answers the blocked envelope, refunded, with an error that says what
///   happened and claims no anti-bot wall.
#[tokio::test]
async fn a_pdf_with_a_hard_block_status_is_refunded_without_claiming_a_wall() {
    let s3_expired = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <Error><Code>AccessDenied</Code><Message>Request has expired</Message>\
        <Expires>2026-10-01T00:00:00Z</Expires></Error>";
    let cases = [
        (
            "akamai",
            "/files/menu.pdf",
            403,
            AKAMAI_UNEXPECTED,
            "text/html",
        ),
        (
            "s3 expired",
            "/bucket/statement.pdf",
            403,
            s3_expired,
            "application/xml",
        ),
        (
            "json 401",
            "/api/docs/report.pdf",
            401,
            "{\"error\":\"unauthorized\",\"message\":\"Sign in to download this file\"}",
            "application/json",
        ),
    ];
    for (name, at, status, body, content_type) in cases {
        let origin = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(ResponseTemplate::new(status).set_body_raw(body, content_type))
            .mount(&origin)
            .await;
        let url = format!(
            "{}{at}?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Signature=0f1e2d",
            origin.uri()
        );

        let main_v1 = answer(&app(false), "/v1/scrape", &url).await;
        let s = app(true);
        let v1 = answer(&s, "/v1/scrape", &url).await;
        for key in ["success", "error", "errorCode", "markdown", "statusCode"] {
            assert_eq!(
                v1[key], main_v1[key],
                "{name}: /v1 {key} moved off main's answer"
            );
        }
        assert_eq!(v1["success"], false, "{name}: {v1}");
        assert_eq!(v1["errorCode"], "http_error", "{name}: {v1}");

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
            assert_eq!(keys, ["data", "error", "success"], "{name} {route}: {body}");
            assert_eq!(body["success"], false, "{name} {route}: {body}");
            assert!(body["data"].get("markdown").is_none(), "{name} {route}");
            assert_eq!(body["data"]["metadata"]["creditsUsed"], 0, "{name} {route}");
            let error = body["error"].as_str().unwrap_or_default();
            assert_eq!(
                error,
                format!("HTTP {status} on a PDF URL; no browser tier renders a PDF"),
                "{name} {route}"
            );
        }
    }
}

/// A body that is the generic wall the ladder tail condemns is answered as that
/// wall under any 4xx/5xx status, as it was when the ladder still ran for it.
/// 404 is outside the hard-block set, so only the body says what this is.
#[tokio::test]
async fn a_pdf_wall_under_a_404_is_answered_like_a_wall_no_tier_could_clear() {
    let origin = MockServer::start().await;
    let url = serve(&origin, "/files/menu.pdf", 404, CAPTCHA_WALL).await;
    let s = app(true);

    let v1 = answer(&s, "/v1/scrape", &url).await;
    assert_eq!(v1["success"], false, "{v1}");
    assert_eq!(v1["errorCode"], "anti_bot", "{v1}");
    assert!(v1["markdown"].is_null(), "wall text leaked: {v1}");

    let v2 = answer(&s, "/v2/scrape", &url).await;
    assert_eq!(v2["success"], false, "{v2}");
    assert!(v2["markdown"].is_null(), "wall text leaked: {v2}");
    assert_eq!(v2["creditsUsed"], 0, "{v2}");
}

/// The other side: a PDF path answered with a status outside the hard-block set
/// (a site's own 404 page, with a script so the gate really skips a ladder) is
/// answered exactly as before, the same as an engine with no browser tier
/// answers it. On `/v2` that is a billed 4xx document.
#[tokio::test]
async fn a_genuine_pdf_404_is_answered_as_before() {
    let origin = MockServer::start().await;
    let not_found = "<!doctype html><html><head><title>Not Found</title>\
        <script src=\"/assets/app.js\"></script></head><body><h1>Not Found</h1>\
        <p>The document you asked for is not on this server.</p></body></html>";
    let url = serve(&origin, "/files/missing.pdf", 404, not_found).await;

    let mut answers = Vec::new();
    for with_browser in [true, false] {
        let s = app(with_browser);
        answers.push([
            answer(&s, "/v1/scrape", &url).await,
            answer(&s, "/v2/scrape", &url).await,
        ]);
    }

    assert_eq!(
        answers[0], answers[1],
        "a browser tier changed the answer for a genuine .pdf 404"
    );
    assert_eq!(answers[0][1]["success"], true, "{}", answers[0][1]);
    assert_eq!(answers[0][1]["statusCode"], 404);
}

/// Poll a batch job until it finishes and return its final status body.
async fn finished_batch(s: &TestServer, status_path: &str) -> Value {
    for _ in 0..300 {
        let body: Value = s.get(status_path).await.json();
        if body["status"] == "completed" {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("batch {status_path} did not finish");
}

/// Batch takes the same document as `/v1/scrape`: a PDF the origin refused with
/// a hard-block status keeps the page that explains the refusal (here an
/// expired presigned S3 link) and is counted `blocked`, so it is not billed.
/// With a browser tier present, the answer for this page must be exactly what
/// an engine with none gives, on both batch surfaces.
#[tokio::test]
async fn batch_keeps_a_pdf_the_origin_refused_as_main_does() {
    let origin = MockServer::start().await;
    let expired = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <Error><Code>AccessDenied</Code><Message>Request has expired</Message></Error>";
    Mock::given(method("GET"))
        .and(path("/bucket/statement.pdf"))
        .respond_with(ResponseTemplate::new(403).set_body_raw(expired, "application/xml"))
        .mount(&origin)
        .await;
    let url = format!(
        "{}/bucket/statement.pdf?X-Amz-Signature=0f1e2d",
        origin.uri()
    );

    let mut per_engine = Vec::new();
    for with_browser in [true, false] {
        let s = app(with_browser);
        let mut seen = Vec::new();
        for prefix in ["/v1", "/v2"] {
            let started: Value = s
                .post(&format!("{prefix}/batch/scrape"))
                .json(&json!({"urls": [url], "formats": ["markdown"]}))
                .await
                .json();
            let id = started["id"].as_str().expect("a batch id").to_string();
            let done = finished_batch(&s, &format!("{prefix}/batch/scrape/{id}")).await;
            let doc = &done["data"][0];
            seen.push(json!({
                "blocked": done["blocked"],
                "markdown": doc["markdown"],
                "block": doc["block"],
                "statusCode": doc["metadata"]["statusCode"],
            }));
        }
        per_engine.push(seen);
    }

    assert_eq!(
        per_engine[0], per_engine[1],
        "a browser tier changed the batch answer for a refused .pdf"
    );
    let v1 = &per_engine[0][0];
    assert!(
        v1["markdown"]
            .as_str()
            .is_some_and(|m| m.contains("Request has expired")),
        "the origin's explanation was cleared: {v1}"
    );
    assert_eq!(v1["block"]["vendor"], "http_error", "{v1}");
    assert_eq!(v1["blocked"], 1, "a refused page is never billed: {v1}");
    assert_eq!(v1["statusCode"], 403);
}

/// A browser tier that ran out of budget on a wall: an empty partial snapshot
/// served with `status`. A rate-limiting origin answers 429 this way, and the
/// request used to end as a timeout the client kept retrying.
fn app_with_truncated_browser(status: u16) -> TestServer {
    allow_loopback();
    let config: AppConfig = toml::from_str("").unwrap();
    let mut state = AppState::new(config).expect("AppState::new failed");
    let mut cfg = RendererConfig {
        mode: RendererMode::None,
        ..Default::default()
    };
    cfg.impersonated.enabled = false;
    let browser = Arc::new(StaticTier {
        name: "chrome",
        status_code: status,
        html: "<html><head></head><body></body></html>".to_string(),
        truncated: true,
    }) as Arc<dyn PageFetcher>;
    let renderer = FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default())
        .expect("renderer builds")
        .with_js_tiers(vec![browser]);
    state.renderer = Arc::new(renderer);
    TestServer::new(create_app(state))
}

/// The raw answer to a scrape that must go through the browser tier.
async fn forced_js(s: &TestServer, route: &str, url: &str) -> Value {
    s.post(route)
        .json(&json!({"url": url, "formats": ["markdown"], "renderJs": true}))
        .await
        .json()
}

/// An empty partial render on a 429 is the wall, not a timeout, on every
/// surface, and it stays unbilled.
#[tokio::test]
async fn an_empty_truncated_render_on_a_rate_limit_answers_the_wall() {
    let origin = MockServer::start().await;
    let url = serve(&origin, "/rate-limited", 429, "").await;
    let s = app_with_truncated_browser(429);

    let v1 = forced_js(&s, "/v1/scrape", &url).await;
    assert_eq!(v1["data"]["metadata"]["renderedWith"], "chrome", "{v1}");
    assert_eq!(v1["success"], false, "{v1}");
    assert_eq!(v1["errorCode"], "anti_bot", "{v1}");
    assert_eq!(v1["data"]["block"]["vendor"], "rate_limited", "{v1}");

    for route in ["/v2/scrape", "/firecrawl/v2/scrape"] {
        let v2 = forced_js(&s, route, &url).await;
        assert_eq!(v2["success"], false, "{route}: {v2}");
        assert_eq!(v2["data"]["metadata"]["creditsUsed"], 0, "{route}: {v2}");
    }
}

/// Without a wall the same empty partial render is still a timeout: the caller
/// should raise its budget, not give up on the site.
#[tokio::test]
async fn an_empty_truncated_render_without_a_wall_is_still_a_timeout() {
    let origin = MockServer::start().await;
    let url = serve(&origin, "/slow", 200, "<html><body></body></html>").await;
    let s = app_with_truncated_browser(200);

    let body = forced_js(&s, "/v1/scrape", &url).await;
    assert_eq!(body["success"], false, "{body}");
    assert_eq!(body["errorCode"], "timeout", "{body}");

    let r = s
        .post("/v2/scrape")
        .json(&json!({"url": url, "formats": ["markdown"], "renderJs": true}))
        .await;
    assert_eq!(r.status_code(), StatusCode::REQUEST_TIMEOUT, "{}", r.text());
}
