//! Crawl subcommand — BFS crawl a website starting from a URL.
//!
//! This is a simplified CLI version that outputs results as JSON lines.
//! For full async crawl jobs with status polling, use `crw serve`.

use crate::commands::scrape::Format;
use crate::teardown::CmdError;
use clap::Args;
use crw_core::config::{RendererConfig, RendererMode, StealthConfig};
use crw_core::types::{CrawlRequest, CrawlState, CrawlStatus, OutputFormat};
use crw_crawl::crawl::{CrawlOptions, run_crawl};
use crw_renderer::FallbackRenderer;
use std::sync::Arc;
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Args)]
pub struct CrawlArgs {
    /// Starting URL for the crawl
    pub url: String,

    /// Maximum crawl depth (0 = only starting URL)
    #[arg(short, long, default_value = "2")]
    pub depth: u32,

    /// Maximum number of pages to crawl
    #[arg(short, long, default_value = "10")]
    pub limit: u32,

    /// Output format for each page
    #[arg(short, long, value_enum, default_value = "markdown")]
    pub format: Format,

    /// Enable JavaScript rendering
    #[arg(long)]
    pub js: bool,

    /// Disable main content extraction (return full page content)
    #[arg(long)]
    pub raw: bool,

    /// HTTP, HTTPS, or SOCKS5 proxy URL
    #[arg(long, value_name = "URL")]
    pub proxy: Option<String>,

    /// Enable stealth mode
    #[arg(long)]
    pub stealth: bool,

    /// Requests per second rate limit
    #[arg(long, default_value = "2.0")]
    pub rate_limit: f64,

    /// Maximum concurrent requests
    #[arg(long, default_value = "5")]
    pub concurrency: usize,

    /// Per-page timeout in milliseconds
    #[arg(long, default_value = "30000")]
    pub timeout: u64,
}

pub async fn run(mut args: CrawlArgs) -> Result<(), CmdError> {
    // Auto-prepend https:// if no scheme is provided
    if !args.url.contains("://") {
        args.url = format!("https://{}", args.url);
    }

    // Build renderer config
    let mut renderer_config = RendererConfig::default();
    let _browser_guards = if args.js {
        if let Ok(ws_url) = std::env::var("CRW_CDP_URL") {
            renderer_config.lightpanda = Some(crw_core::config::CdpEndpoint { ws_url });
            Vec::new()
        } else {
            let browsers = crw_renderer::browser::spawn_all_headless().await;
            if browsers.is_empty() {
                eprintln!(
                    "warning: --js requested but no browser found. \
                     Install LightPanda or Chrome for JS rendering. \
                     Falling back to HTTP."
                );
            }
            let mut guards = Vec::new();
            for (guard, ws_url, kind) in browsers {
                match kind {
                    crw_renderer::browser::RendererKind::LightPanda => {
                        renderer_config.lightpanda = Some(crw_core::config::CdpEndpoint { ws_url });
                    }
                    crw_renderer::browser::RendererKind::Chrome => {
                        renderer_config.chrome = Some(crw_core::config::CdpEndpoint { ws_url });
                    }
                }
                guards.push(guard);
            }
            guards
        }
    } else {
        renderer_config.mode = RendererMode::None;
        Vec::new()
    };

    let stealth_config = StealthConfig {
        enabled: args.stealth,
        inject_headers: args.stealth,
        ..Default::default()
    };

    // Attach a rotator built from --proxy so the proxy is resolved into
    // REQUEST_PROXY for BOTH the HTTP and JS/CDP tiers (the JS tier reads only
    // REQUEST_PROXY; passing --proxy to new() alone would leave JS direct).
    let renderer = {
        let build = || -> crw_core::CrwResult<FallbackRenderer> {
            let rotator = crw_core::ProxyRotator::build(
                &[],
                args.proxy.as_deref(),
                crw_core::ProxyRotation::default(),
            )
            .map_err(crw_core::CrwError::ConfigError)?
            .map(Arc::new);
            FallbackRenderer::new(&renderer_config, "crw/0.7.0", None, &stealth_config)?
                .with_proxy_rotator(rotator)
        };
        match build() {
            Ok(r) => Arc::new(r),
            Err(e) => {
                eprintln!("error: failed to build renderer: {e}");
                return Err(CmdError::code_only(1));
            }
        }
    };

    let output_format = match args.format {
        Format::Markdown => OutputFormat::Markdown,
        Format::Json => OutputFormat::Json,
        Format::Html => OutputFormat::Html,
        Format::Rawhtml => OutputFormat::RawHtml,
        Format::Text => OutputFormat::PlainText,
        Format::Links => OutputFormat::Links,
        Format::Images => OutputFormat::Images,
    };

    let crawl_req = CrawlRequest {
        url: args.url.clone(),
        max_depth: Some(args.depth),
        max_pages: Some(args.limit),
        formats: vec![output_format],
        only_main_content: args.raw.then_some(false),
        json_schema: None,
        render_js: if args.js { Some(true) } else { None },
        wait_for: None,
        renderer: None,
        country: None,
        proxy_list: Vec::new(),
        proxy_rotation: None,
        // No CLI flag for per-request headers, same as `crw scrape`.
        headers: std::collections::HashMap::new(),
    };

    let id = Uuid::new_v4();
    let (state_tx, mut state_rx) = watch::channel(CrawlState {
        id,
        success: false,
        status: CrawlStatus::InProgress,
        total: 0,
        completed: 0,
        blocked: 0,
        data: vec![],
        error: None,
    });

    // Spawn the crawl task
    // One config load for both extraction knobs the crawl has to match the
    // single-scrape path on.
    let crawl_extraction_cfg = crw_core::config::AppConfig::load()
        .unwrap_or_default()
        .extraction;
    let crawl_opts = CrawlOptions {
        id,
        req: crawl_req,
        renderer: renderer.clone(),
        max_concurrency: args.concurrency,
        respect_robots: true,
        requests_per_second: args.rate_limit,
        user_agent: "crw/0.7.0",
        state_tx,
        llm_config: None,
        proxy: args.proxy,
        jitter_factor: 0.2,
        deadline_ms_per_page: args.timeout,
        per_host_max_concurrent: 1,
        http_retry_threshold_bytes: crawl_extraction_cfg.http_retry_threshold_bytes,
        normalize_tables: crawl_extraction_cfg.normalize_tables,
        only_main_content_default: crawl_extraction_cfg.only_main_content,
    };

    let crawl_handle = tokio::spawn(async move {
        // A crawl is `Batch` traffic (multi-page). Scoped inside the spawned task
        // (task-locals don't cross `tokio::spawn`) so its fetches/extracts use the
        // batch lanes rather than the interactive reserve.
        crw_core::REQUEST_CLASS
            .scope(crw_core::ScrapeClass::Batch, async {
                run_crawl(crawl_opts).await;
            })
            .await;
    });

    // Stream results as they come in
    let mut last_completed = 0;
    let mut output_count = 0;

    loop {
        state_rx.changed().await.ok();
        let state = state_rx.borrow().clone();

        // Output new results
        for data in state.data.iter().skip(last_completed) {
            let content = match args.format {
                Format::Markdown => data.markdown.clone().unwrap_or_default(),
                Format::Json => serde_json::to_string(&data).unwrap_or_default(),
                Format::Html => data.html.clone().unwrap_or_default(),
                Format::Rawhtml => data.raw_html.clone().unwrap_or_default(),
                Format::Text => data.plain_text.clone().unwrap_or_default(),
                Format::Links => data.links.clone().unwrap_or_default().join("\n"),
                Format::Images => data
                    .images
                    .clone()
                    .unwrap_or_default()
                    .iter()
                    .map(|i| i.url.clone())
                    .collect::<Vec<_>>()
                    .join("\n"),
            };

            if !content.is_empty() {
                output_count += 1;
                eprintln!(
                    "--- Page {} ({}) ---",
                    output_count, data.metadata.source_url
                );
                println!("{content}");
                println!();
            }
        }
        last_completed = state.data.len();

        // Check if done
        match state.status {
            CrawlStatus::Completed => {
                // `completed` counts every URL the crawl finished with, and
                // that now includes the ones it could not read. Report the
                // pages the caller actually got, and name the rest rather than
                // folding them into the same number.
                let scraped = state.completed.saturating_sub(state.blocked);
                if state.blocked > 0 {
                    eprintln!(
                        "Crawl completed: {scraped} pages ({} unreadable)",
                        state.blocked
                    );
                } else {
                    eprintln!("Crawl completed: {scraped} pages");
                }
                break;
            }
            CrawlStatus::Failed => {
                if let Some(err) = state.error {
                    eprintln!("error: crawl failed: {err}");
                } else {
                    eprintln!("error: crawl failed");
                }
                return Err(CmdError::code_only(1));
            }
            CrawlStatus::Cancelled => {
                eprintln!("Crawl cancelled after {} pages", state.completed);
                break;
            }
            CrawlStatus::InProgress => {}
        }
    }

    crawl_handle.await.ok();
    Ok(())
}
