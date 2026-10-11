//! Wiremock-backed tests for the page cache on `crw_crawl::single::scrape_url`.
//!
//! The assertion that matters in every one of these is the number of requests
//! the ORIGIN received. A unit test can prove the cache stores and loads; only
//! counting real fetches proves the fetch was actually skipped, and that a
//! bypass really did go back to the network.
//!
//! The cache is process-global and these tests run in parallel in one process,
//! so every test must work on a URL no other test can produce. A mock server
//! alone is not enough: wiremock pools and reuses its servers, so a dropped
//! server's port comes back and two tests can end up with the same URL, the
//! same cache key, and each other's entries. Each test therefore mounts its own
//! PATH as well.

use std::sync::Arc;

use crw_core::config::{ExtractionConfig, RendererConfig, StealthConfig};
use crw_core::types::{
    ChangeTrackingMode, ChangeTrackingOptions, ChangeTrackingSnapshot, OutputFormat, ScrapeData,
    ScrapeRequest,
};
use crw_crawl::single::scrape_url;
use crw_renderer::FallbackRenderer;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const UA: &str = "crw-test/0.0";

/// Long enough to sit in the top budget bucket, so a bucket mismatch never
/// explains a miss in these tests.
const DEADLINE_MS: u64 = 60_000;

fn http_renderer() -> Arc<FallbackRenderer> {
    let cfg = RendererConfig::default();
    let stealth = StealthConfig::default();
    Arc::new(FallbackRenderer::new(&cfg, UA, None, &stealth).expect("renderer"))
}

/// A page with enough text that markdown extraction is never byte-thin, which
/// would otherwise keep it out of the cache as a suspected shell.
fn body() -> String {
    let para = "Lume Roasters sources single origin coffee from smallholder farms. ";
    format!(
        "<html><head><title>Lume</title></head><body><h1>Our coffee</h1><p>{}</p></body></html>",
        para.repeat(24)
    )
}

async fn page_server(name: &str) -> (MockServer, String) {
    let server = MockServer::start().await;
    let route = format!("/{name}");
    Mock::given(method("GET"))
        .and(path(route.clone()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html; charset=utf-8")
                .set_body_string(body()),
        )
        .mount(&server)
        .await;
    let url = format!("{}{}", server.uri(), route);
    (server, url)
}

async fn run(req: &ScrapeRequest) -> ScrapeData {
    scrape_url(
        req,
        &http_renderer(),
        None,
        &ExtractionConfig::default(),
        UA,
        false,
        Some(false),
        crw_core::deadline::Deadline::from_request_ms(DEADLINE_MS),
    )
    .await
    .expect("scrape")
}

async fn fetches(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .map(|r| r.len())
        .unwrap_or(0)
}

fn req(url: &str) -> ScrapeRequest {
    ScrapeRequest {
        url: url.to_string(),
        formats: vec![OutputFormat::Markdown],
        ..Default::default()
    }
}

/// The headline claim: the same page twice costs one fetch.
#[tokio::test]
async fn a_repeat_scrape_does_not_reach_the_origin() {
    let (server, url) = page_server("repeat").await;

    let first = run(&req(&url)).await;
    assert!(!first.cached, "the first scrape cannot be a hit");
    assert_eq!(fetches(&server).await, 1);

    let second = run(&req(&url)).await;
    assert!(second.cached, "the second scrape should have been a hit");
    assert_eq!(fetches(&server).await, 1, "the origin was fetched twice");
    assert_eq!(second.markdown, first.markdown);
}

/// The reason the feature exists. Extraction options are not part of the key,
/// so a second pass asking for different output still shares one fetch. This is
/// the multi-pass extraction shape, minus the LLM leg.
#[tokio::test]
async fn different_output_options_share_one_fetch() {
    let (server, url) = page_server("outputs").await;

    run(&req(&url)).await;
    assert_eq!(fetches(&server).await, 1);

    let second = run(&ScrapeRequest {
        formats: vec![OutputFormat::Markdown, OutputFormat::Html],
        only_main_content: Some(false),
        include_tags: vec!["p".into()],
        ..req(&url)
    })
    .await;

    assert!(second.cached, "a different output shape refetched the page");
    assert_eq!(fetches(&server).await, 1);
    assert!(second.html.is_some(), "the new format was still produced");
}

/// The documented escape hatch has to actually reach the network.
#[tokio::test]
async fn max_age_zero_always_fetches() {
    let (server, url) = page_server("maxage").await;

    run(&req(&url)).await;
    let second = run(&ScrapeRequest {
        max_age: Some(0),
        ..req(&url)
    })
    .await;

    assert!(!second.cached);
    assert_eq!(fetches(&server).await, 2);
}

/// Change tracking diffs a LIVE page against the caller's snapshot. Served from
/// cache it would report "unchanged" for a page that did change, which is a
/// silent wrong answer rather than a visible error.
#[tokio::test]
async fn change_tracking_never_reads_the_cache() {
    let (server, url) = page_server("changetracking").await;

    run(&req(&url)).await;
    assert_eq!(fetches(&server).await, 1);

    let tracked = run(&ScrapeRequest {
        formats: vec![OutputFormat::Markdown, OutputFormat::ChangeTracking],
        change_tracking: Some(ChangeTrackingOptions {
            modes: vec![ChangeTrackingMode::GitDiff],
            previous: Some(ChangeTrackingSnapshot {
                markdown: Some("something else entirely".to_string()),
                content_hash: crw_diff::snapshot::hash_markdown("something else entirely"),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..req(&url)
    })
    .await;

    assert!(!tracked.cached);
    assert_eq!(
        fetches(&server).await,
        2,
        "change tracking was answered from cache"
    );
}

/// Country changes the exit the page is fetched through, so it changes the
/// page. It belongs in the key.
#[tokio::test]
async fn a_country_change_is_a_different_fetch() {
    let (server, url) = page_server("country").await;

    run(&req(&url)).await;
    let elsewhere = run(&ScrapeRequest {
        country: Some("de".into()),
        ..req(&url)
    })
    .await;

    assert!(!elsewhere.cached, "a different country reused the fetch");
    assert_eq!(fetches(&server).await, 2);
}

/// Caller headers are where a cookie or a token lives. A page fetched with one
/// is personal to that caller and must not sit in a process-global map, nor be
/// readable by anyone else, however the key is shaped.
#[tokio::test]
async fn a_request_with_caller_headers_is_never_cached() {
    let (server, url) = page_server("headers").await;

    let with_headers = ScrapeRequest {
        headers: [("Cookie".to_string(), "session=alice".to_string())]
            .into_iter()
            .collect(),
        ..req(&url)
    };

    let first = run(&with_headers).await;
    assert!(!first.cached);
    let second = run(&with_headers).await;
    assert!(!second.cached, "an authenticated page was replayed");
    assert_eq!(fetches(&server).await, 2);

    // And it left nothing behind for a caller who sends no headers.
    let anonymous = run(&req(&url)).await;
    assert!(!anonymous.cached, "a header-bearing fetch seeded the cache");
    assert_eq!(fetches(&server).await, 3);
}

/// Whether a fetch could escalate to a browser is part of the key, because an
/// escalated fetch holds a rendered DOM rather than the HTTP source.
///
/// This renderer has no browser tier, so neither request could escalate and
/// sharing the entry is the correct answer here: there is no DOM either of them
/// could have got. The key-level separation is asserted in the `page_cache`
/// unit tests, which can vary that input directly.
#[tokio::test]
async fn without_a_browser_tier_every_reader_shares_one_fetch() {
    let (server, url) = page_server("notier").await;

    run(&req(&url)).await;
    let raw = run(&ScrapeRequest {
        formats: vec![OutputFormat::RawHtml],
        ..req(&url)
    })
    .await;

    assert!(raw.cached);
    assert_eq!(fetches(&server).await, 1);
    assert!(raw.raw_html.is_some(), "the new format was still produced");
}

/// The hosted API promises a cache is never shared between customers. The
/// engine keeps that promise by never letting an entry cross a tenant scope,
/// which its front end sets per account.
#[tokio::test]
async fn one_tenant_never_reads_another_tenants_page() {
    let (server, url) = page_server("tenants").await;

    let alice = ScrapeRequest {
        cache_scope: Some("alice".into()),
        ..req(&url)
    };
    let bob = ScrapeRequest {
        cache_scope: Some("bob".into()),
        ..req(&url)
    };

    let first = run(&alice).await;
    assert!(!first.cached);
    assert_eq!(fetches(&server).await, 1);

    let crossed = run(&bob).await;
    assert!(
        !crossed.cached,
        "one tenant was served another tenant's page"
    );
    assert_eq!(fetches(&server).await, 2);

    // And each still gets their own repeat for free.
    assert!(run(&alice).await.cached);
    assert!(run(&bob).await.cached);
    assert_eq!(fetches(&server).await, 2);
}

/// `storeInCache: false` is Firecrawl's write switch: this request may read,
/// but must leave nothing behind.
#[tokio::test]
async fn store_in_cache_false_writes_nothing() {
    let (server, url) = page_server("nostore").await;

    let first = run(&ScrapeRequest {
        store_in_cache: Some(false),
        ..req(&url)
    })
    .await;
    assert!(!first.cached);
    assert_eq!(fetches(&server).await, 1);

    let second = run(&req(&url)).await;
    assert!(!second.cached, "a no-store request still seeded the cache");
    assert_eq!(fetches(&server).await, 2);
}

/// A failed fetch is never stored, so a retry always gets a real attempt. This
/// is published behaviour, not an implementation detail.
#[tokio::test]
async fn a_failing_page_is_never_cached() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/failing"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream is down"))
        .mount(&server)
        .await;
    let url = format!("{}/failing", server.uri());

    let _ = scrape_url(
        &req(&url),
        &http_renderer(),
        None,
        &ExtractionConfig::default(),
        UA,
        false,
        Some(false),
        crw_core::deadline::Deadline::from_request_ms(DEADLINE_MS),
    )
    .await;
    let before = fetches(&server).await;

    let _ = scrape_url(
        &req(&url),
        &http_renderer(),
        None,
        &ExtractionConfig::default(),
        UA,
        false,
        Some(false),
        crw_core::deadline::Deadline::from_request_ms(DEADLINE_MS),
    )
    .await;

    assert!(
        fetches(&server).await > before,
        "a failed fetch was replayed from cache"
    );
}
