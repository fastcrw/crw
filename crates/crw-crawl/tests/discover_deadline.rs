//! `/map` discovery must return what it found when an in-flight page outlives
//! its own budget, instead of waiting on it past the BFS deadline. In its own
//! test binary for the same reason as `discover_concurrency.rs`: the per-host
//! limiter is process-global and every loopback server shares one host key.
//!
//! The http tier floors its body read at 500ms, so a page whose headers arrive
//! just inside the deadline and whose body then stalls overran it by ~450ms
//! with the old unbounded `in_flight.next().await`. In production the same wait
//! sat behind a recovery tier's fresh budget and outlived the route timeout,
//! which turned a map with links into a 504 with none.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crw_core::config::{RendererConfig, RendererMode, StealthConfig};
use crw_crawl::crawl::{DiscoverOptions, discover_urls};
use crw_renderer::FallbackRenderer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve(listener: TcpListener, headers_at: Instant) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 2048];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let req = String::from_utf8_lossy(&buf);
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            match path.as_str() {
                "/" => {
                    let body = r#"<html><body><a href="/slow">slow</a></body></html>"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                }
                "/slow" => {
                    // Headers land just inside the deadline, then the body stalls.
                    tokio::time::sleep_until(tokio::time::Instant::from_std(headers_at)).await;
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 100000\r\n\r\n<html><body>",
                        )
                        .await;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                _ => {
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                }
            }
        });
    }
}

#[tokio::test]
async fn bfs_returns_at_its_deadline_when_a_page_overruns_its_budget() {
    // SAFETY: set before any discovery runs in this binary.
    unsafe {
        std::env::set_var("CRW_ALLOW_LOOPBACK_FOR_TESTS", "1");
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let deadline = Instant::now() + Duration::from_secs(2);
    tokio::spawn(serve(listener, deadline - Duration::from_millis(50)));

    let cfg = RendererConfig {
        mode: RendererMode::None,
        ..Default::default()
    };
    let r =
        Arc::new(FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default()).unwrap());
    let result = discover_urls(DiscoverOptions {
        base_url: &base,
        max_depth: 2,
        use_sitemap: true,
        crawl_fallback: true,
        renderer: &r,
        max_concurrency: 5,
        requests_per_second: 100.0,
        user_agent: "crw-test",
        proxy: None,
        deadline_ms_per_page: 30_000,
        per_host_max_concurrent: 4,
        url_filter: None,
        max_urls: 100,
        overall_deadline: deadline,
        respect_robots: true,
    })
    .await
    .expect("partial results, not an error");
    let overshoot = Instant::now().saturating_duration_since(deadline);
    eprintln!(
        "overshoot past BFS deadline: {overshoot:?}; urls={:?}",
        result.urls
    );
    assert!(
        result.urls.iter().any(|u| u.ends_with("/slow")),
        "the link found before the stall must be returned: {:?}",
        result.urls
    );
    assert!(
        overshoot < Duration::from_millis(300),
        "discovery waited {overshoot:?} past its deadline for a page that overran its own budget"
    );
}
