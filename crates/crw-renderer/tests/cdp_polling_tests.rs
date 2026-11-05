//! Integration tests for the CDP renderer's content-stability polling.
//!
//! These tests spin up a local axum server that serves a page whose real
//! content is injected by `setTimeout` *after* `DEFAULT_JS_WAIT_MS` has
//! elapsed. Without polling, the renderer would return the loading
//! placeholder; with polling it must wait for the injected content to
//! appear.
//!
//! Gated behind `#[ignore]` because they require a running Chrome (or other
//! CDP browser) reachable via `CRW_CDP_WS_URL`. Run with:
//!
//! ```sh
//! # Start headless Chrome once:
//! chrome --headless=new --remote-debugging-port=9223 &
//! # Then:
//! CRW_CDP_WS_URL=ws://127.0.0.1:9223 \
//!   cargo test -p crw-renderer --features cdp --test cdp_polling_tests -- --ignored
//! ```

#![cfg(feature = "cdp")]

use std::collections::HashMap;
use std::time::Duration;

use axum::Router;
use axum::response::Html;
use axum::routing::get;
use crw_core::Deadline;
use crw_renderer::cdp::CdpRenderer;
use crw_renderer::traits::PageFetcher;
use tokio::net::TcpListener;

fn tdl() -> Deadline {
    Deadline::now_plus(Duration::from_secs(60))
}

/// Page that displays a loading placeholder for 2.5 s, then swaps in real
/// content. 2500 ms is chosen to be *after* the default 2 s JS wait so that
/// a non-polling renderer would miss the real content entirely.
const DELAYED_CONTENT_HTML: &str = r#"<!doctype html>
<html lang="en">
<head><title>Delayed SPA</title></head>
<body>
  <div id="root">
    <p>Loading...</p>
    <p>Please wait while we prepare your experience.</p>
  </div>
  <script>
    setTimeout(function () {
      document.getElementById('root').innerHTML =
        '<article>' +
        '<h1>Welcome To My Creative Space</h1>' +
        '<p>This is a real portfolio page with substantial content. ' +
        'The author specialises in full-stack development, covering both ' +
        'backend infrastructure and client-side interfaces. Projects span ' +
        'distributed systems, mobile apps, and developer tooling.</p>' +
        '<p>Additional paragraphs cover topics like observability, typed ' +
        'APIs, and the craft of writing software that remains maintainable ' +
        'over long horizons.</p>' +
        '</article>';
    }, 2500);
  </script>
</body>
</html>"#;

async fn spawn_delayed_site() -> String {
    let app = Router::new().route("/", get(|| async { Html(DELAYED_CONTENT_HTML) }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn cdp_ws_url() -> Option<String> {
    std::env::var("CRW_CDP_WS_URL").ok()
}

#[tokio::test]
#[ignore = "requires CRW_CDP_WS_URL pointing to a live Chrome/CDP endpoint"]
async fn polls_until_delayed_content_appears() {
    let ws_url = cdp_ws_url().expect("CRW_CDP_WS_URL must be set for this test");
    let base_url = spawn_delayed_site().await;

    let renderer = CdpRenderer::new("chrome", &ws_url, 30_000, 2);

    // Auto mode (wait_for_ms = None) triggers the stability poll when the
    // initial snapshot still looks like a loading placeholder.
    let result = renderer
        .fetch(&base_url, &HashMap::new(), None, tdl())
        .await
        .expect("CDP fetch failed");

    assert!(
        result.html.contains("Welcome To My Creative Space"),
        "stability poll did not wait for delayed content; got:\n{}",
        &result.html[..result.html.len().min(1000)]
    );
    assert!(
        !result.html.contains("Please wait while we prepare"),
        "placeholder text still present — polling returned too early"
    );
}

#[tokio::test]
#[ignore = "requires CRW_CDP_WS_URL pointing to a live Chrome/CDP endpoint"]
async fn explicit_wait_for_opts_out_of_polling() {
    // When the caller passes an explicit wait_for_ms, the stability poll is
    // skipped. We pass 500 ms (well under the 2500 ms content injection),
    // so the returned HTML should still contain the placeholder.
    let ws_url = cdp_ws_url().expect("CRW_CDP_WS_URL must be set for this test");
    let base_url = spawn_delayed_site().await;

    let renderer = CdpRenderer::new("chrome", &ws_url, 30_000, 2);

    let result = renderer
        .fetch(&base_url, &HashMap::new(), Some(500), tdl())
        .await
        .expect("CDP fetch failed");

    assert!(
        result.html.contains("Please wait while we prepare"),
        "explicit wait_for should have returned placeholder (polling opt-out); got:\n{}",
        &result.html[..result.html.len().min(500)]
    );
}

/// ~1000 chars of plain prose and no SPA container (`main`, `article`, `#root`,
/// ...): the shape example.com has today, which spins the readiness poll until
/// the nav budget fires.
fn selectorless_page(extra_script: &str) -> String {
    let text = "<p>This page uses none of the usual SPA containers: no main, no \
                article, no root or app mount. It is plain markup with a script, \
                the shape example.com has today.</p>"
        .repeat(6);
    format!(
        "<!doctype html><html><head><title>Plain</title></head><body>{text}\
         <script>{extra_script}</script></body></html>"
    )
}

async fn spawn_budget_site() -> String {
    let app = Router::new()
        .route(
            "/settled",
            get(|| async { Html(selectorless_page("document.title = 'Plain';")) }),
        )
        .route(
            "/hung",
            get(|| async { Html(selectorless_page("fetch('/never');")) }),
        )
        .route(
            "/never",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                "late"
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// The budget fires on a page that finished loading long before: the snapshot
/// is the whole page, so it must not carry the partial-render warning.
#[tokio::test]
#[ignore = "requires CRW_CDP_WS_URL (live Chrome/CDP) and CRW_ALLOW_LOOPBACK_FOR_TESTS=1"]
async fn budget_hit_on_a_settled_page_is_not_labelled_partial() {
    let ws_url = cdp_ws_url().expect("CRW_CDP_WS_URL must be set for this test");
    let base_url = spawn_budget_site().await;
    let renderer = CdpRenderer::new("lightpanda", &ws_url, 2_500, 2);

    let result = renderer
        .fetch(&format!("{base_url}/settled"), &HashMap::new(), None, tdl())
        .await
        .expect("CDP fetch failed");

    assert!(result.warning.is_none(), "warning: {:?}", result.warning);
    assert!(
        result.warnings.is_empty(),
        "warnings: {:?}",
        result.warnings
    );
    // Decisions still see the budget hit; only the label changed.
    assert!(result.truncated, "the budget should still have fired");
    assert!(result.budget_settled);
}

/// A request still in flight when the budget fires: the page is genuinely
/// partial and keeps its label.
#[tokio::test]
#[ignore = "requires CRW_CDP_WS_URL (live Chrome/CDP) and CRW_ALLOW_LOOPBACK_FOR_TESTS=1"]
async fn budget_hit_with_a_request_in_flight_stays_partial() {
    let ws_url = cdp_ws_url().expect("CRW_CDP_WS_URL must be set for this test");
    let base_url = spawn_budget_site().await;
    let renderer = CdpRenderer::new("lightpanda", &ws_url, 2_500, 2);

    let result = renderer
        .fetch(&format!("{base_url}/hung"), &HashMap::new(), None, tdl())
        .await
        .expect("CDP fetch failed");

    assert!(result.truncated);
    assert!(!result.budget_settled);
    assert_eq!(
        result.warnings,
        vec!["lightpanda_budget_truncated".to_string()]
    );
}
