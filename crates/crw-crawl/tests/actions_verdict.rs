//! What a scrape with browser `actions` answers, end to end through
//! `crw_crawl::single::scrape_url`: the real HTTP tier against a mock origin
//! and a stub browser tier that reports what the actions did.
//!
//! The guarantee these pin: a body the actions never ran on is never a
//! success (issue #658 was exactly that, silently), a failed step is
//! `action_failed`, skipped optional steps are warnings, and a wall keeps its
//! own verdict.

use std::collections::HashMap;
use std::sync::Arc;

use crw_core::Deadline;
use crw_core::actions::ActionsOutcome;
use crw_core::config::{ExtractionConfig, RendererConfig, RendererMode, StealthConfig};
use crw_core::error::{CrwError, CrwResult};
use crw_core::types::{FetchResult, ScrapeData, ScrapeRequest};
use crw_renderer::FallbackRenderer;
use crw_renderer::traits::PageFetcher;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PAGE: &str = "<html><head><title>Products</title></head><body><main>\
    <h1>Products</h1><p>Showing 36 of 85 results. Every product on this page has a long \
    description so the body is well past the thin-content floor and reads as a real page \
    rather than a placeholder or a shell.</p><ul><li>Product one</li><li>Product two</li>\
    <li>Product three</li></ul></main></body></html>";

const WALL: &str = "<html><head><title>Security Check</title></head><body>\
    <h1>Security Check</h1><p>Checking your browser before accessing the site</p></body></html>";

/// A browser tier that answers with `html` and the given actions outcome.
struct ActionsTier {
    status_code: u16,
    html: &'static str,
    outcome: Option<ActionsOutcome>,
}

#[async_trait::async_trait]
impl PageFetcher for ActionsTier {
    async fn fetch(
        &self,
        url: &str,
        _headers: &HashMap<String, String>,
        _wait_for_ms: Option<u64>,
        _deadline: Deadline,
    ) -> CrwResult<FetchResult> {
        Ok(FetchResult {
            url: url.to_string(),
            final_url: None,
            status_code: self.status_code,
            html: self.html.to_string(),
            content_type: Some("text/html".to_string()),
            raw_bytes: None,
            rendered_with: Some("chrome".to_string()),
            elapsed_ms: 0,
            warning: None,
            render_decision: None,
            credit_cost: 0,
            warnings: Vec::new(),
            truncated: false,
            budget_settled: false,
            deadline_exceeded: false,
            captured_responses: Vec::new(),
            screenshot: None,
            block: None,
            residential_bytes: None,
            actions: self.outcome.clone(),
        })
    }

    fn name(&self) -> &str {
        "chrome"
    }

    fn supports_js(&self) -> bool {
        true
    }

    async fn is_available(&self) -> bool {
        true
    }
}

fn renderer(tier: ActionsTier) -> Arc<FallbackRenderer> {
    // SAFETY: set before any fetch runs; every test here sets the same value.
    unsafe {
        std::env::set_var("CRW_ALLOW_LOOPBACK_FOR_TESTS", "1");
    }
    let mut cfg = RendererConfig {
        mode: RendererMode::None,
        ..Default::default()
    };
    cfg.impersonated.enabled = false;
    Arc::new(
        FallbackRenderer::new(&cfg, "crw-test", None, &StealthConfig::default())
            .expect("renderer builds")
            .with_js_tiers(vec![Arc::new(tier) as Arc<dyn PageFetcher>]),
    )
}

async fn origin() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(PAGE.as_bytes().to_vec(), "text/html"),
        )
        .mount(&server)
        .await;
    server
}

async fn scrape(tier: ActionsTier) -> CrwResult<ScrapeData> {
    let server = origin().await;
    let req = ScrapeRequest {
        url: format!("{}/", server.uri()),
        actions: Some(serde_json::json!([
            { "type": "click", "selector": "button.more", "optional": true }
        ])),
        ..Default::default()
    };
    crw_crawl::single::scrape_url(
        &req,
        &renderer(tier),
        None,
        &ExtractionConfig::default(),
        "crw-test",
        false,
        None,
        Deadline::from_request_ms(30_000),
    )
    .await
}

#[tokio::test]
async fn a_body_the_actions_never_ran_on_is_action_failed() {
    let err = scrape(ActionsTier {
        status_code: 200,
        html: PAGE,
        outcome: None,
    })
    .await
    .expect_err("a body with no actions outcome must not be a success");
    assert!(
        matches!(&err, CrwError::ActionFailed(m) if m.contains("did not run")),
        "got {err:?}"
    );
}

#[tokio::test]
async fn a_failed_step_is_action_failed_with_its_message() {
    let err = scrape(ActionsTier {
        status_code: 200,
        html: PAGE,
        outcome: Some(ActionsOutcome {
            failed: Some("action 1 (click \"button.more\"): no element matched".into()),
            skipped: Vec::new(),
        }),
    })
    .await
    .expect_err("a failed strict step fails the scrape");
    assert!(
        matches!(&err, CrwError::ActionFailed(m) if m.contains("button.more")),
        "got {err:?}"
    );
    assert_eq!(err.error_code(), "action_failed");
}

#[tokio::test]
async fn skipped_optional_steps_become_warnings() {
    let data = scrape(ActionsTier {
        status_code: 200,
        html: PAGE,
        outcome: Some(ActionsOutcome {
            failed: None,
            skipped: vec!["action 1 (click \"button.more\") skipped: no element matched".into()],
        }),
    })
    .await
    .expect("actions that ran are a success");
    assert!(
        data.warnings.iter().any(|w| w.contains("skipped")),
        "got {:?}",
        data.warnings
    );
    assert!(
        data.markdown
            .as_deref()
            .is_some_and(|m| m.contains("Product one"))
    );
}

#[tokio::test]
async fn a_wall_keeps_its_own_verdict_over_action_failed() {
    let data = scrape(ActionsTier {
        status_code: 403,
        html: WALL,
        outcome: None,
    })
    .await
    .expect("a wall is a verdict on the page, not a scrape error");
    assert!(data.block.is_some(), "the wall verdict must be stamped");
}
