//! `[extraction].only_main_content` is the default for a scrape that does not
//! send `onlyMainContent`; an explicit value on the request still wins.
//!
//! Requests are built from JSON, the way every HTTP surface builds them, so the
//! "field omitted" case is exactly what a client that omits it produces.

use std::sync::Arc;

use crw_core::config::{ExtractionConfig, RendererConfig, StealthConfig};
use crw_core::types::{ScrapeData, ScrapeRequest};
use crw_crawl::single::scrape_url;
use crw_renderer::FallbackRenderer;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const UA: &str = "crw-test/0.0";
const NAV: &str = "Navmarker";
const FOOTER: &str = "Footermarker";

fn body() -> String {
    let para = "Lume Roasters sources single origin coffee from smallholder farms. ";
    format!(
        "<html><head><title>Lume</title></head><body>\
         <nav><a href=\"/p\">{NAV}</a></nav>\
         <main><article><h1>Our coffee</h1><p>{}</p></article></main>\
         <footer><p>{FOOTER}</p></footer></body></html>",
        para.repeat(24)
    )
}

/// One mock server and path per test: the page cache is process-global.
async fn page_url(name: &str) -> (MockServer, String) {
    let server = MockServer::start().await;
    let route = format!("/omc-{name}");
    Mock::given(method("GET"))
        .and(path(route.clone()))
        .respond_with(
            // `set_body_string` would force `text/plain`, and a plain-text page
            // never reaches the HTML cleaner.
            ResponseTemplate::new(200).set_body_raw(body(), "text/html; charset=utf-8"),
        )
        .mount(&server)
        .await;
    let url = format!("{}{}", server.uri(), route);
    (server, url)
}

async fn scrape(json: serde_json::Value, config_only_main: bool) -> ScrapeData {
    let req: ScrapeRequest = serde_json::from_value(json).expect("request");
    let cfg = RendererConfig::default();
    let renderer = Arc::new(
        FallbackRenderer::new(&cfg, UA, None, &StealthConfig::default()).expect("renderer"),
    );
    let extraction = ExtractionConfig {
        only_main_content: config_only_main,
        ..ExtractionConfig::default()
    };
    scrape_url(
        &req,
        &renderer,
        None,
        &extraction,
        UA,
        false,
        Some(false),
        crw_core::deadline::Deadline::from_request_ms(60_000),
    )
    .await
    .expect("scrape")
}

/// `true` when nav AND footer survived, `false` when both were stripped.
/// Anything in between fails: the page is built so the two modes differ on both.
fn has_chrome(data: &ScrapeData) -> bool {
    let md = data.markdown.as_deref().unwrap_or_default();
    assert!(md.contains("smallholder farms"), "article missing: {md}");
    let (nav, footer) = (md.contains(NAV), md.contains(FOOTER));
    assert_eq!(nav, footer, "nav={nav} footer={footer}: {md}");
    nav
}

#[tokio::test]
async fn omitted_field_follows_config_false() {
    let (_s, url) = page_url("cfg-false").await;
    let data = scrape(serde_json::json!({ "url": url }), false).await;
    assert!(has_chrome(&data), "config false must keep nav/footer");
}

#[tokio::test]
async fn omitted_field_follows_config_true() {
    let (_s, url) = page_url("cfg-true").await;
    let data = scrape(serde_json::json!({ "url": url }), true).await;
    assert!(!has_chrome(&data), "config true must strip nav/footer");
}

#[tokio::test]
async fn explicit_true_beats_config_false() {
    let (_s, url) = page_url("req-true").await;
    let data = scrape(
        serde_json::json!({ "url": url, "onlyMainContent": true }),
        false,
    )
    .await;
    assert!(!has_chrome(&data), "request true must strip nav/footer");
}

#[tokio::test]
async fn explicit_false_beats_config_true() {
    let (_s, url) = page_url("req-false").await;
    let data = scrape(
        serde_json::json!({ "url": url, "onlyMainContent": false }),
        true,
    )
    .await;
    assert!(has_chrome(&data), "request false must keep nav/footer");
}
