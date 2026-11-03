//! BFS concurrency for `/map` discovery — in its OWN test binary on purpose.
//!
//! The per-host limiter is process-global and keyed by eTLD+1. Every wiremock
//! server in a test binary binds to 127.0.0.1, so sibling tests in the same
//! process contend for the SAME host permits and serialize each other, which
//! looks exactly like the serial-BFS bug this test exists to catch. Cargo gives
//! each `tests/*.rs` its own process, so isolating this one keeps it honest.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crw_core::config::{RendererConfig, RendererMode, StealthConfig};
use crw_crawl::crawl::{DiscoverOptions, discover_urls};
use crw_renderer::FallbackRenderer;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// wiremock binds to loopback, which the SSRF guard rejects by default.
fn allow_loopback() {
    // SAFETY: set before any discovery runs.
    unsafe {
        std::env::set_var("CRW_ALLOW_LOOPBACK_FOR_TESTS", "1");
    }
}

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

fn opts<'a>(
    base_url: &'a str,
    renderer: &'a Arc<FallbackRenderer>,
    overall_deadline: Instant,
    respect_robots: bool,
) -> DiscoverOptions<'a> {
    DiscoverOptions {
        base_url,
        max_depth: 2,
        use_sitemap: true,
        crawl_fallback: true,
        renderer,
        max_concurrency: 5,
        requests_per_second: 100.0,
        user_agent: "crw-test",
        proxy: None,
        deadline_ms_per_page: 5_000,
        per_host_max_concurrent: 4,
        url_filter: None,
        max_urls: 100,
        overall_deadline,
        respect_robots,
        filter_by_path: true,
    }
}

/// The BFS used to acquire a semaphore permit and then await the fetch inline in
/// the same loop body, so nothing was ever spawned and real concurrency was 1.
/// Pages here respond slowly; if fetches were serial the level could not finish
/// in anywhere near the asserted window.
#[tokio::test]
async fn bfs_fetches_a_level_concurrently() {
    let server = MockServer::start().await;
    const PAGE_DELAY: Duration = Duration::from_millis(600);
    const CHILDREN: usize = 4;

    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sitemap.xml"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<a href="/p1">1</a><a href="/p2">2</a><a href="/p3">3</a><a href="/p4">4</a>"#,
        ))
        .mount(&server)
        .await;
    for i in 1..=CHILDREN {
        Mock::given(method("GET"))
            .and(path(format!("/p{i}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(PAGE_DELAY)
                    .set_body_string("<html>leaf</html>"),
            )
            .mount(&server)
            .await;
    }

    let r = renderer().await;
    let started = Instant::now();
    discover_urls(opts(
        &server.uri(),
        &r,
        Instant::now() + Duration::from_secs(30),
        true,
    ))
    .await
    .expect("discovery succeeds");
    let elapsed = started.elapsed();

    // Serial would be >= CHILDREN * PAGE_DELAY (2.4s) for the child level alone.
    // Concurrent overlaps them. The bound is loose on purpose: the real ceiling is
    // the per-host limiter, not `max_concurrency`, so this asserts "not serial"
    // rather than a specific speedup.
    let serial_floor = PAGE_DELAY * CHILDREN as u32;
    assert!(
        elapsed < serial_floor,
        "BFS level looks serial: took {elapsed:?}, serial floor is {serial_floor:?}"
    );
}
