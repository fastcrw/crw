//! A wall the renderer ladder could not clear, end to end through crw-crawl.
//!
//! The renderer used to raise that wall as an error, which every caller here
//! turned into "no page". It now carries the verdict on an `Ok` result, so each
//! caller has to read it. These tests keep the HTTP tier real against a mock
//! origin and stub only the browser tier, because nothing else reaches the
//! ladder tail without a browser.

use std::sync::Arc;

use crw_core::config::{RendererConfig, RendererMode, StealthConfig};
use crw_core::types::{CrawlRequest, CrawlState, CrawlStatus, OutputFormat};
use crw_crawl::crawl::{CrawlOptions, run_crawl};
use crw_renderer::FallbackRenderer;
use crw_renderer::test_utils::StaticTier;
use crw_renderer::traits::PageFetcher;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A generic phrase-list wall (`detector::looks_like_generic_bot_wall`) with a
/// link on it, so a test can tell whether anything read the wall as a page.
const WALL: &str = "<html><head><title>Security Check</title></head><body>\
    <h1>Security Check</h1><p>Checking your browser before accessing the site</p>\
    <a href=\"/behind-the-wall\">continue</a></body></html>";

/// wiremock binds to loopback, which the SSRF guard rejects by default.
fn allow_loopback() {
    // SAFETY: set before any fetch runs; every test in this file sets the same
    // value, so concurrent tests cannot observe a different one.
    unsafe {
        std::env::set_var("CRW_ALLOW_LOOPBACK_FOR_TESTS", "1");
    }
}

/// A real HTTP tier and one browser tier that always lands on `html`.
fn walled_renderer(html: &str) -> Arc<FallbackRenderer> {
    allow_loopback();
    let mut cfg = RendererConfig {
        mode: RendererMode::None,
        ..Default::default()
    };
    // One plain HTTP tier, so the origin sees a predictable number of requests.
    cfg.impersonated.enabled = false;
    let browser = Arc::new(StaticTier {
        name: "chrome",
        status_code: 403,
        html: html.to_string(),
    }) as Arc<dyn PageFetcher>;
    Arc::new(
        FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default())
            .expect("renderer builds")
            .with_js_tiers(vec![browser]),
    )
}

/// An origin that serves `WALL` at `/` with a 403, the shape of
/// `mock.fastcrw.com/wall/captcha`. The wall's own link must never be fetched.
async fn walled_origin() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("content-type", "text/html")
                .set_body_string(WALL),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/behind-the-wall"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>never</html>"))
        .expect(0)
        .mount(&server)
        .await;
    server
}

async fn crawl(renderer: Arc<FallbackRenderer>, url: String) -> CrawlState {
    let id = Uuid::new_v4();
    let (state_tx, state_rx) = tokio::sync::watch::channel(CrawlState {
        id,
        success: false,
        status: CrawlStatus::InProgress,
        total: 0,
        completed: 0,
        blocked: 0,
        data: Vec::new(),
        error: None,
    });
    run_crawl(CrawlOptions {
        id,
        req: CrawlRequest {
            url,
            max_depth: Some(1),
            max_pages: Some(5),
            formats: vec![OutputFormat::Markdown],
            only_main_content: Some(false),
            json_schema: None,
            // Auto, the default: the renderer escalates a walled HTTP body.
            render_js: None,
            wait_for: None,
            renderer: None,
            country: None,
            proxy_list: Vec::new(),
            proxy_rotation: None,
            headers: std::collections::HashMap::new(),
        },
        renderer,
        max_concurrency: 1,
        respect_robots: false,
        requests_per_second: 100.0,
        user_agent: "crw-test",
        state_tx,
        llm_config: None,
        proxy: None,
        jitter_factor: 0.0,
        deadline_ms_per_page: 15_000,
        per_host_max_concurrent: 1,
        normalize_tables: false,
        only_main_content_default: true,
        http_retry_threshold_bytes: 0,
    })
    .await;
    state_rx.borrow().clone()
}

/// Before the verdict was carried, the renderer's error sent this page down
/// the crawl's failed-page arm: no body, no links followed, counted in
/// `blocked` and so never billed. It must still end exactly there.
#[tokio::test]
async fn crawl_records_a_carried_wall_as_a_failed_page() {
    let server = walled_origin().await;
    let state = crawl(walled_renderer(WALL), format!("{}/", server.uri())).await;

    assert_eq!(state.status, CrawlStatus::Completed);
    assert_eq!(state.data.len(), 1, "no link on the wall may be followed");
    assert_eq!(state.completed, 1);
    assert_eq!(state.blocked, 1, "a wall is never a billable page");
    let page = &state.data[0];
    let block = page
        .block
        .as_ref()
        .expect("the wall verdict is on the page");
    assert_eq!(block.vendor, "generic_block");
    assert!(
        block.reason.contains("no renderer tier could clear"),
        "got {}",
        block.reason
    );
    assert_eq!(page.metadata.status_code, 403);
    assert!(page.markdown.is_none(), "the wall text is not content");
    // `/behind-the-wall` carries `.expect(0)`, verified when `server` drops.
}
