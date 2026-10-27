//! Test seam for crates that drive a [`FallbackRenderer`] from the outside.
//!
//! A carried wall verdict only exists once a JS ladder has run and been
//! walled, and every real JS tier needs a browser. These two items let
//! `crw-crawl` and `crw-server` tests keep the real HTTP tier (pointed at a
//! mock origin) and stub only the browser tiers, so the whole chain from the
//! ladder tail to the route envelope runs for real.
//!
//! Compiled only with the `test-utils` feature, which only dev-dependencies
//! enable.

use std::collections::HashMap;
use std::sync::Arc;

use crw_core::Deadline;
use crw_core::error::CrwResult;
use crw_core::types::FetchResult;

use crate::FallbackRenderer;
use crate::traits::PageFetcher;

/// A browser tier that answers every URL with the same page.
pub struct StaticTier {
    pub name: &'static str,
    pub status_code: u16,
    pub html: String,
    /// Report the page as a partial snapshot taken when the budget ran out.
    pub truncated: bool,
}

#[async_trait::async_trait]
impl PageFetcher for StaticTier {
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
            html: self.html.clone(),
            content_type: Some("text/html".to_string()),
            raw_bytes: None,
            rendered_with: Some(self.name.to_string()),
            elapsed_ms: 0,
            warning: None,
            render_decision: None,
            credit_cost: 0,
            warnings: Vec::new(),
            truncated: self.truncated,
            deadline_exceeded: false,
            captured_responses: Vec::new(),
            screenshot: None,
            block: None,
        })
    }

    fn name(&self) -> &str {
        self.name
    }

    fn supports_js(&self) -> bool {
        true
    }

    async fn is_available(&self) -> bool {
        true
    }
}

impl FallbackRenderer {
    /// Replace the JS ladder with `tiers`, keeping the real HTTP tier.
    pub fn with_js_tiers(mut self, tiers: Vec<Arc<dyn PageFetcher>>) -> Self {
        self.js_renderers = tiers;
        self
    }
}
