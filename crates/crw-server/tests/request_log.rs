//! The per-request log line, driven through the real `create_app` stack.
//!
//! Own test binary on purpose: it installs a thread-local tracing subscriber
//! that captures everything, which must not race with other suites' output.

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum_test::TestServer;
use crw_core::config::AppConfig;
use crw_server::app::create_app;
use crw_server::middleware::is_probe_route;
use crw_server::state::AppState;
use serde_json::json;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    /// Only the request-log lines, so state-construction chatter is ignored.
    fn request_lines(&self) -> Vec<String> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .filter(|l| l.contains("crw_server::middleware"))
            .map(str::to_owned)
            .collect()
    }
}

#[test]
fn probe_predicate_covers_liveness_readiness_and_metrics_only() {
    for probe in [
        "/health",
        "/ready",
        "/metrics",
        "/metrics/renderer-breakers",
        "/metrics/renderer-preferences",
    ] {
        assert!(is_probe_route(probe), "{probe} should be skipped");
    }
    for real in [
        "/v1/scrape",
        "/v1/search",
        "/v1/crawl/{id}",
        "/admin/breakers/reset",
        "/healthz-not-a-route",
        "/v1/metrics",
    ] {
        assert!(!is_probe_route(real), "{real} must be logged");
    }
}

#[tokio::test]
async fn one_line_per_request_with_route_status_and_reason_and_no_probes() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    // Empty config: no search backend, so search answers a real 503 envelope.
    let config: AppConfig = toml::from_str("").unwrap();
    let server = TestServer::new(create_app(AppState::new(config).unwrap()));

    // Probes: answered, never logged.
    server.get("/health").await.assert_status_ok();
    server.get("/metrics").await;
    server.get("/ready").await;
    assert!(
        captured.request_lines().is_empty(),
        "probe logged: {:?}",
        captured.request_lines()
    );

    // 5xx on the native surface: one WARN line naming route, status and code.
    let res = server.post("/v1/search").json(&json!({"query": "q"})).await;
    assert_eq!(res.status_code().as_u16(), 503);
    // 5xx on the Firecrawl-shaped surface (`V2Error`).
    let res = server.post("/v2/search").json(&json!({"query": "q"})).await;
    assert_eq!(res.status_code().as_u16(), 503);
    // 4xx: INFO, with the code too. The query string and the raw id carry
    // what a customer token would look like and must not reach the log.
    let res = server
        .get("/v1/crawl/6f1c0a52-0000-4000-8000-00000000beef?token=cust-s3cret")
        .await;
    assert_eq!(res.status_code().as_u16(), 404);
    // A path no route matches: logged, but not as its raw path.
    server.get("/wp-login.php?token=cust-s3cret").await;

    let lines = captured.request_lines();
    println!("{}", lines.join("\n"));
    assert_eq!(lines.len(), 4, "one line per non-probe request: {lines:#?}");

    let v1 = &lines[0];
    assert!(v1.contains(" WARN "), "5xx must be WARN: {v1}");
    assert!(v1.contains("request failed"), "{v1}");
    for field in [
        "method=POST",
        "route=\"/v1/search\"",
        "status=503",
        "error_code=\"search_disabled\"",
        "latency_ms=",
    ] {
        assert!(v1.contains(field), "missing {field}: {v1}");
    }

    let v2 = &lines[1];
    assert!(v2.contains(" WARN "), "{v2}");
    assert!(v2.contains("route=\"/v2/search\""), "{v2}");
    assert!(v2.contains("error_code=\"search_disabled\""), "{v2}");

    let crawl = &lines[2];
    assert!(crawl.contains(" INFO "), "4xx is INFO: {crawl}");
    assert!(crawl.contains("route=\"/v1/crawl/{id}\""), "{crawl}");
    assert!(crawl.contains("status=404"), "{crawl}");
    assert!(crawl.contains("error_code=\"not_found\""), "{crawl}");

    let unmatched = &lines[3];
    assert!(unmatched.contains("route=\"<unmatched>\""), "{unmatched}");
    assert!(unmatched.contains("status=404"), "{unmatched}");
    assert!(!unmatched.contains("error_code"), "{unmatched}");

    for line in &lines {
        assert!(!line.contains("ERROR"), "nothing at ERROR: {line}");
        assert!(!line.contains("cust-s3cret"), "query string leaked: {line}");
        assert!(!line.contains("beef"), "raw path id leaked: {line}");
        assert!(
            !line.contains("wp-login"),
            "raw unmatched path leaked: {line}"
        );
    }
}
