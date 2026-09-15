//! Integration tests for the two behaviours `/v1/crawl` gained together:
//! caller-supplied request headers reaching every page, and a URL the crawl
//! could not read coming back marked instead of silently vanishing.
//!
//! Both are exercised through `run_crawl` against a mock origin, because the
//! unit tests around them cannot fail if the wiring is removed: reverting the
//! fetch call to an empty header map, or deleting the failure branches, leaves
//! every serde-level test green.

use std::sync::Arc;

use crw_core::config::{RendererConfig, RendererMode, StealthConfig};
use crw_core::types::{CrawlRequest, CrawlState, CrawlStatus, OutputFormat};
use crw_crawl::crawl::{CrawlOptions, run_crawl};
use crw_renderer::FallbackRenderer;
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// wiremock binds to loopback, which the SSRF guard rejects by default.
fn allow_loopback() {
    // SAFETY: set before any crawl runs; tests in this file share one process.
    unsafe {
        std::env::set_var("CRW_ALLOW_LOOPBACK_FOR_TESTS", "1");
    }
}

/// An HTTP-only renderer: these tests exercise crawl bookkeeping, not JS.
async fn renderer() -> Arc<FallbackRenderer> {
    allow_loopback();
    let cfg = RendererConfig {
        mode: RendererMode::None,
        ..Default::default()
    };
    Arc::new(
        FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default())
            .expect("renderer builds in http-only mode"),
    )
}

fn request(url: String) -> CrawlRequest {
    CrawlRequest {
        url,
        max_depth: Some(0),
        max_pages: Some(1),
        formats: vec![OutputFormat::Markdown],
        only_main_content: false,
        json_schema: None,
        render_js: Some(false),
        wait_for: None,
        renderer: None,
        country: None,
        proxy_list: Vec::new(),
        proxy_rotation: None,
        headers: std::collections::HashMap::new(),
    }
}

/// Drive one crawl to completion and hand back the terminal state.
async fn run(req: CrawlRequest) -> CrawlState {
    run_with_robots(req, false).await
}

/// As `run`, but lets a test turn robots.txt enforcement on.
async fn run_with_robots(req: CrawlRequest, respect_robots: bool) -> CrawlState {
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
        req,
        renderer: renderer().await,
        max_concurrency: 1,
        respect_robots,
        requests_per_second: 100.0,
        user_agent: "crw-test-default-ua",
        state_tx,
        llm_config: None,
        proxy: None,
        jitter_factor: 0.0,
        deadline_ms_per_page: 15_000,
        per_host_max_concurrent: 1,
        normalize_tables: false,
        http_retry_threshold_bytes: 0,
    })
    .await;
    state_rx.borrow().clone()
}

/// The crawl used to hand the renderer an empty header map, so a documented
/// `headers` field did nothing on this path. The mock only answers when both
/// the custom header and the overridden User-Agent arrive.
#[tokio::test]
async fn caller_headers_reach_every_page_of_the_crawl() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .and(header("X-Crw-Test", "probe"))
        .and(header("User-Agent", "crw-header-probe/1.0"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><h1>Header page</h1></body></html>")
                .insert_header("content-type", "text/html"),
        )
        .mount(&server)
        .await;

    let mut req = request(format!("{}/", server.uri()));
    req.headers.insert("X-Crw-Test".into(), "probe".into());
    req.headers
        .insert("User-Agent".into(), "crw-header-probe/1.0".into());

    let state = run(req).await;

    // A missing header would leave the mock unmatched, so the page would come
    // back as a failure instead of content.
    assert_eq!(state.blocked, 0, "headers did not reach the origin");
    assert_eq!(state.data.len(), 1);
    assert!(
        state.data[0]
            .markdown
            .as_deref()
            .unwrap_or_default()
            .contains("Header page")
    );
}

/// Without the headers the same mock does not match, which is what makes the
/// assertion above meaningful rather than vacuous.
#[tokio::test]
async fn the_header_probe_fails_when_the_headers_are_absent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .and(header("X-Crw-Test", "probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body>ok</body></html>"))
        .mount(&server)
        .await;

    let state = run(request(format!("{}/", server.uri()))).await;
    assert!(
        state.data.first().and_then(|d| d.markdown.as_deref()) != Some("ok"),
        "the mock matched without the header, so the header test proves nothing"
    );
}

/// A CDN answering for a dead origin used to be dropped on the floor: the
/// caller got `completed: 0`, an empty array, and no way to learn which URL
/// failed. It now comes back marked, and marked is what keeps it unbilled,
/// since the caller charges `completed - blocked`.
#[tokio::test]
async fn a_cdn_origin_error_comes_back_marked_and_unbilled() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(523))
        .mount(&server)
        .await;

    let url = format!("{}/", server.uri());
    let state = run(request(url.clone())).await;

    assert_eq!(state.status, CrawlStatus::Completed);
    assert_eq!(state.completed, 1);
    assert_eq!(state.blocked, 1);
    assert_eq!(
        state.completed - state.blocked,
        0,
        "a page nobody could read must not be billable"
    );

    let doc = state.data.first().expect("the failed URL must be reported");
    assert_eq!(doc.metadata.source_url, url);
    assert_eq!(doc.metadata.status_code, 523);
    assert!(doc.markdown.is_none(), "there is no page to return");
    let block = doc.block.as_ref().expect("failure must be marked");
    assert_eq!(block.vendor, crw_core::types::HTTP_ERROR_VENDOR);
    assert_eq!(block.reason, "CDN could not reach origin");
}

/// A page that answers normally is untouched by any of the above: no block, and
/// it stays billable. Guards against the failure branches over-triggering.
#[tokio::test]
async fn a_healthy_page_is_neither_marked_nor_counted_blocked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    "<html><body><h1>Real page</h1><p>Body text here.</p></body></html>",
                )
                .insert_header("content-type", "text/html"),
        )
        .mount(&server)
        .await;

    let state = run(request(format!("{}/", server.uri()))).await;

    assert_eq!(state.completed, 1);
    assert_eq!(state.blocked, 0);
    assert!(state.data[0].block.is_none());
    assert!(
        state.data[0]
            .markdown
            .as_deref()
            .unwrap_or_default()
            .contains("Real page")
    );
}

/// The crawl used to enqueue its own dedup key, which was the whole URL
/// lowercased. On a case-sensitive origin that turned a discovered
/// `/docs/Guide` into a request for `/docs/guide`, so the page came back as a
/// 404 failure and `source_url` named a URL that was never requested.
///
/// The mock answers `/docs/Guide` and nothing else, so a lowercased request
/// falls through to the catch-all 404 and the assertions below fail.
#[tokio::test]
async fn discovered_links_are_fetched_with_the_case_the_page_wrote() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"<html><body><a href="/docs/Guide">G</a></body></html>"#)
                .insert_header("content-type", "text/html"),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/docs/Guide"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><h1>Mixed case page</h1></body></html>")
                .insert_header("content-type", "text/html"),
        )
        .mount(&server)
        .await;

    // Catch-all: anything else (notably `/docs/guide`) is a 404, which is what
    // the old behaviour produced.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let mut req = request(format!("{}/", server.uri()));
    req.max_depth = Some(1);
    req.max_pages = Some(2);

    let state = run(req).await;

    assert_eq!(state.blocked, 0, "the discovered link did not resolve");
    assert_eq!(state.data.len(), 2, "seed plus the discovered page");
    let urls: Vec<&str> = state
        .data
        .iter()
        .map(|d| d.metadata.source_url.as_str())
        .collect();
    assert!(
        urls.iter().any(|u| u.ends_with("/docs/Guide")),
        "source_url must name the URL that was actually requested, got {urls:?}"
    );
    assert!(
        state.data.iter().any(|d| d
            .markdown
            .as_deref()
            .unwrap_or_default()
            .contains("Mixed case page")),
        "the mixed-case page's content is missing, got {urls:?}"
    );
}

/// `run_crawl` matched robots rules against `parsed.path()`, dropping the
/// query. Rules keyed on a query string, the shape Hacker News uses for
/// `/hide?`, `/vote?` and `/reply?`, therefore matched nothing and the crawl
/// fetched exactly what the site forbade. `discover_urls` already used the
/// query-aware check; the crawl, which is the surface that fetches at volume,
/// did not.
///
/// The mock counts hits on the disallowed path, so a crawl that ignores the
/// rule is caught by the count rather than by an absence.
#[tokio::test]
async fn crawl_honours_a_robots_rule_keyed_on_the_query_string() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("User-agent: crw\nDisallow: /Hide?Token=\n"),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"<html><body><a href="/Hide?Token=AbC">h</a></body></html>"#)
                .insert_header("content-type", "text/html"),
        )
        .mount(&server)
        .await;

    // Answers happily if asked. The assertion is that it is never asked.
    Mock::given(method("GET"))
        .and(path("/Hide"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><h1>Forbidden page</h1></body></html>")
                .insert_header("content-type", "text/html"),
        )
        .expect(0)
        .mount(&server)
        .await;

    let mut req = request(format!("{}/", server.uri()));
    req.max_depth = Some(1);
    req.max_pages = Some(5);

    let state = run_with_robots(req, true).await;

    assert!(
        !state.data.iter().any(|d| d
            .markdown
            .as_deref()
            .unwrap_or_default()
            .contains("Forbidden page")),
        "a page disallowed by a query-keyed robots rule was crawled"
    );
    // wiremock verifies `.expect(0)` on drop; assert here too so the failure
    // names the rule rather than surfacing as a panic in teardown.
    assert_eq!(
        state.data.len(),
        1,
        "only the seed should have been fetched"
    );
}

#[tokio::test]
async fn a_robots_txt_we_cannot_read_does_not_fail_the_crawl() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body><h1>Seed page</h1></body></html>")
                .insert_header("content-type", "text/html"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let state = run_with_robots(request(format!("{}/", server.uri())), true).await;

    assert_eq!(state.status, CrawlStatus::Completed);
    assert!(state.success);
    assert_eq!(state.data.len(), 1, "the seed must still be crawled");
    assert!(
        state.error.is_none(),
        "a 503 on robots.txt is logged, not surfaced as a job error: {:?}",
        state.error
    );
}
