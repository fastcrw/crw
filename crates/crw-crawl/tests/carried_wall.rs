//! A wall the renderer ladder could not clear, end to end through crw-crawl.
//!
//! The renderer used to raise that wall as an error, which every caller here
//! turned into "no page". It now carries the verdict on an `Ok` result, so each
//! caller has to read it. These tests keep the HTTP tier real against a mock
//! origin and stub only the browser tier, because nothing else reaches the
//! ladder tail without a browser.

use std::sync::Arc;

use crw_core::Deadline;
use crw_core::config::{ExtractionConfig, LlmConfig, RendererConfig, RendererMode, StealthConfig};
use crw_core::types::{
    ChangeTrackingMode, ChangeTrackingOptions, ChangeTrackingSnapshot, CrawlRequest, CrawlState,
    CrawlStatus, OutputFormat, ScrapeData, ScrapeRequest,
};
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
            // `set_body_string` would reset the type to text/plain.
            ResponseTemplate::new(403).set_body_raw(WALL, "text/html"),
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

fn discover_opts<'a>(
    base_url: &'a str,
    renderer: &'a Arc<FallbackRenderer>,
    use_sitemap: bool,
    crawl_fallback: bool,
) -> crw_crawl::crawl::DiscoverOptions<'a> {
    crw_crawl::crawl::DiscoverOptions {
        base_url,
        max_depth: 2,
        use_sitemap,
        crawl_fallback,
        renderer,
        max_concurrency: 2,
        requests_per_second: 100.0,
        user_agent: "crw-test",
        proxy: None,
        deadline_ms_per_page: 15_000,
        per_host_max_concurrent: 2,
        url_filter: None,
        max_urls: 100,
        overall_deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
        respect_robots: false,
    }
}

/// Map's BFS used to get an error for a page like this and drop it. Reading
/// the carried wall as a page would list the wall's own links as the site's.
#[tokio::test]
async fn map_lists_no_links_from_a_carried_wall() {
    let server = walled_origin().await;
    let renderer = walled_renderer(WALL);
    let base = server.uri();

    let result = crw_crawl::crawl::discover_urls(discover_opts(&base, &renderer, false, true))
        .await
        .expect("discovery completes");

    assert!(
        !result.urls.iter().any(|u| u.contains("behind-the-wall")),
        "a link on the wall was listed: {:?}",
        result.urls
    );
}

/// The sitemap escalation renders a challenged sitemap and reads `<url><loc>`
/// out of whatever comes back. A walled render is not the sitemap, even when
/// the markup it holds would parse as one.
#[tokio::test]
async fn sitemap_escalation_never_reads_a_carried_wall_as_xml() {
    let server = MockServer::start().await;
    // A challenge marker (so the walk escalates) on a body that is itself a
    // generic wall (so the renderer keeps the ladder's verdict rather than
    // substituting this body).
    Mock::given(method("GET"))
        .and(path("/sitemap.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<html><head><title>Just a moment...</title></head><body>\
                 <h1>Security Check</h1>\
                 <p>Checking your browser before accessing the site</p></body></html>",
            "text/html",
        ))
        .mount(&server)
        .await;
    let base = server.uri();
    let walled_render = format!(
        "<html><body><h1>Security Check</h1>\
         <p>Checking your browser before accessing the site</p>\
         <urlset><url><loc>{base}/from-the-wall</loc></url></urlset></body></html>"
    );
    let renderer = walled_renderer(&walled_render);

    let result = crw_crawl::crawl::discover_urls(discover_opts(&base, &renderer, true, false))
        .await
        .expect("discovery completes");

    assert!(
        !result.urls.iter().any(|u| u.contains("from-the-wall")),
        "an entry from the walled render was listed: {:?}",
        result.urls
    );
}

async fn scrape(
    renderer: &Arc<FallbackRenderer>,
    req: &ScrapeRequest,
    llm: Option<&LlmConfig>,
) -> ScrapeData {
    crw_crawl::single::scrape_url(
        req,
        renderer,
        llm,
        &ExtractionConfig::default(),
        "crw-test",
        false,
        None,
        Deadline::from_request_ms(30_000),
    )
    .await
    .expect("a wall is a verdict on the page, not a scrape error")
}

fn change_tracking_request(url: String, previous: &str) -> ScrapeRequest {
    ScrapeRequest {
        url,
        formats: vec![OutputFormat::Markdown, OutputFormat::ChangeTracking],
        change_tracking: Some(ChangeTrackingOptions {
            modes: vec![ChangeTrackingMode::GitDiff],
            previous: Some(ChangeTrackingSnapshot {
                markdown: Some(previous.to_string()),
                content_hash: crw_diff::snapshot::hash_markdown(previous),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Change tracking on a wall no tier could clear is a diff of the wall text
/// that no surface ever returns, and in json mode or with the judge it is a
/// real model call on a request nobody is charged for. That wall used to stop
/// before this phase as an error; it must still stop here.
#[tokio::test]
async fn change_tracking_never_runs_on_a_wall() {
    let server = walled_origin().await;
    // No route mounted: any model call is recorded, and fails the scrape.
    let llm_server = MockServer::start().await;
    let llm = LlmConfig {
        provider: "openai".into(),
        api_key: "test-key".into(),
        model: "gpt-4o-mini".into(),
        base_url: Some(format!("{}/v1", llm_server.uri())),
        ..Default::default()
    };
    let previous = "# Pro plan costs 49 USD";
    let req = ScrapeRequest {
        url: format!("{}/", server.uri()),
        formats: vec![OutputFormat::Markdown, OutputFormat::ChangeTracking],
        change_tracking: Some(ChangeTrackingOptions {
            modes: vec![ChangeTrackingMode::GitDiff, ChangeTrackingMode::Json],
            schema: Some(serde_json::json!({
                "type": "object",
                "properties": { "price": { "type": "string" } }
            })),
            previous: Some(ChangeTrackingSnapshot {
                markdown: Some(previous.to_string()),
                content_hash: crw_diff::snapshot::hash_markdown(previous),
                ..Default::default()
            }),
            ..Default::default()
        }),
        goal: Some("Alert me when pricing changes".into()),
        judge_enabled: Some(true),
        ..Default::default()
    };

    let data = scrape(&walled_renderer(WALL), &req, Some(&llm)).await;

    assert!(data.block.is_some(), "the wall verdict is stamped");
    assert!(data.unclearable_wall);
    assert!(data.change_tracking.is_none(), "a wall got a diff");
    assert!(
        llm_server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "a model was called for a wall"
    );
}

/// The same real HTTP tier with no browser tier at all.
fn http_only_renderer() -> Arc<FallbackRenderer> {
    allow_loopback();
    let mut cfg = RendererConfig {
        mode: RendererMode::None,
        ..Default::default()
    };
    cfg.impersonated.enabled = false;
    Arc::new(
        FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default())
            .expect("renderer builds"),
    )
}

fn markdown_request(url: String) -> ScrapeRequest {
    ScrapeRequest {
        url,
        formats: vec![OutputFormat::Markdown],
        ..Default::default()
    }
}

async fn serve_once_then(first: ResponseTemplate, then: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(first)
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(then)
        .mount(&server)
        .await;
    server
}

fn html(status: u16, body: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_raw(body.to_string(), "text/html")
}

/// A wall wordy enough that its markdown clears the substantial-content guard,
/// so `classify_block` alone answers "not a block". Still short enough, and
/// phrased enough, to be a generic wall for the renderer.
const WORDY_WALL: &str = "<html><body><h1>Verify you are human</h1>\
    <p>Please complete the security check to continue to the site. This keeps \
    the service safe for everyone, and we appreciate your patience while your \
    browser is verified.</p></body></html>";

/// The carried verdict has to win where `classify_block` stays silent, or a
/// wall with a paragraph of copy on it ships as the page.
#[tokio::test]
async fn carried_verdict_beats_the_substantial_markdown_guard() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(html(403, WORDY_WALL))
        .mount(&server)
        .await;
    let req = markdown_request(format!("{}/", server.uri()));

    // Without a ladder this page is not a block at all: the markdown guard
    // waves it through. That is what makes the carried verdict load-bearing.
    let plain = scrape(&http_only_renderer(), &req, None).await;
    assert!(plain.block.is_none(), "control: {:?}", plain.block);

    let walled = scrape(&walled_renderer(WORDY_WALL), &req, None).await;
    let block = walled.block.as_ref().expect("the carried verdict applies");
    assert_eq!(block.vendor, "generic_block");
    assert!(walled.unclearable_wall);
}

/// `unclearable_wall` means the ladder condemned the page, nothing else. A
/// verdict `classify_block` reaches on its own must not set it, or `/v2` would
/// start refunding 4xx pages it bills today.
#[tokio::test]
async fn a_verdict_classify_block_reaches_alone_does_not_set_the_flag() {
    let server = walled_origin().await;
    let req = markdown_request(format!("{}/", server.uri()));

    let data = scrape(&http_only_renderer(), &req, None).await;

    assert!(data.block.is_some(), "the 403 wall is still classified");
    assert!(!data.unclearable_wall);
}

/// `single.rs`'s own escalation (a 2xx whose markdown came out thin) can bring
/// back a walled render. When that render is accepted as the page, its verdict
/// comes with it.
#[tokio::test]
async fn an_accepted_escalation_carries_the_verdict() {
    // No script, so the renderer does not escalate the thin page itself and
    // the second-chance escalation in `scrape_url` is the one that runs. Its
    // fetch then meets the wall.
    let server = serve_once_then(
        html(200, "<html><body><p>Tiny.</p></body></html>"),
        html(403, WORDY_WALL),
    )
    .await;
    let req = markdown_request(format!("{}/", server.uri()));

    let data = scrape(&walled_renderer(WORDY_WALL), &req, None).await;

    let seen = server.received_requests().await.unwrap_or_default().len();
    assert!(
        seen >= 2,
        "the escalation never re-fetched: {seen} request(s)"
    );
    assert!(data.unclearable_wall, "block: {:?}", data.block);
    assert_eq!(
        data.block.as_ref().map(|b| b.vendor.as_str()),
        Some("generic_block")
    );
}

/// ...and when it is NOT accepted, the verdict stays behind with the render
/// that was thrown away. Carrying it would condemn the page we kept.
#[tokio::test]
async fn an_escalation_that_is_not_accepted_leaves_its_verdict_behind() {
    // `WALL`'s markdown is under the acceptance threshold, so the escalated
    // render loses and the first body stays.
    let server = serve_once_then(
        html(200, "<html><body><p>Tiny.</p></body></html>"),
        html(403, WALL),
    )
    .await;
    let req = markdown_request(format!("{}/", server.uri()));

    let data = scrape(&walled_renderer(WALL), &req, None).await;

    let seen = server.received_requests().await.unwrap_or_default().len();
    assert!(
        seen >= 2,
        "the escalation never re-fetched: {seen} request(s)"
    );
    assert!(!data.unclearable_wall, "block: {:?}", data.block);
    assert!(
        data.markdown
            .as_deref()
            .is_some_and(|m| m.contains("Tiny.")),
        "the first body is the one kept: {:?}",
        data.markdown
    );
}

/// The other side of the change-tracking gate. With no browser tier, this 403
/// is a page `classify_block` labelled and nothing more: `/v2` still returns it
/// as a billed `success:true` document, and it has always had its diff there.
#[tokio::test]
async fn change_tracking_still_runs_on_a_page_only_classify_block_labelled() {
    let server = walled_origin().await;
    let req = change_tracking_request(format!("{}/", server.uri()), "# Earlier page");

    let data = scrape(&http_only_renderer(), &req, None).await;

    assert!(data.block.is_some(), "the 403 is still labelled");
    assert!(!data.unclearable_wall);
    assert!(data.change_tracking.is_some(), "the diff must still run");
}
