//! Map subcommand — discover URLs on a website via sitemap and crawling.

use crate::teardown::CmdError;
use clap::{Args, ValueEnum};
use crw_core::config::{RendererConfig, RendererMode, StealthConfig};
use crw_crawl::crawl::{DiscoverOptions, discover_urls};
use crw_renderer::FallbackRenderer;

/// Discovery budget for the `map` subcommand, which has no request timeout of
/// its own. Matches the HTTP route's default.
const CLI_MAP_TIMEOUT_SECS: u64 = 120;
use std::sync::Arc;

#[derive(Clone, ValueEnum)]
pub enum MapFormat {
    /// One URL per line
    Text,
    /// JSON array
    Json,
}

#[derive(Args)]
pub struct MapArgs {
    /// Base URL to discover links from
    pub url: String,

    /// Maximum depth for link discovery
    #[arg(short, long, default_value = "2")]
    pub depth: u32,

    /// Output format
    #[arg(short, long, value_enum, default_value = "text")]
    pub format: MapFormat,

    /// Skip sitemap discovery, only use HTML crawling
    #[arg(long)]
    pub no_sitemap: bool,

    /// Skip HTML crawl fallback, only use sitemap
    #[arg(long)]
    pub sitemap_only: bool,

    /// Max URLs to discover (up to 5,000,000). Raise it to dump large sitemaps.
    #[arg(long, default_value_t = crw_crawl::crawl::DEFAULT_MAX_DISCOVERED_URLS)]
    pub limit: usize,

    /// Enable JavaScript rendering
    #[arg(long)]
    pub js: bool,

    /// HTTP, HTTPS, or SOCKS5 proxy URL
    #[arg(long, value_name = "URL")]
    pub proxy: Option<String>,

    /// Enable stealth mode
    #[arg(long)]
    pub stealth: bool,

    /// Requests per second rate limit
    #[arg(long, default_value = "5.0")]
    pub rate_limit: f64,

    /// Maximum concurrent requests
    #[arg(long, default_value = "10")]
    pub concurrency: usize,

    /// Per-page timeout in milliseconds
    #[arg(long, default_value = "15000")]
    pub timeout: u64,

    /// Map the whole site even when the URL has a path (by default only URLs
    /// under that path are returned)
    #[arg(long)]
    pub no_filter_by_path: bool,
}

pub async fn run(mut args: MapArgs) -> Result<(), CmdError> {
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

    // Attach a rotator from --proxy so discovery page fetches resolve the proxy
    // into REQUEST_PROXY uniformly (HTTP + any JS), matching the crawl/server path.
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

    let opts = DiscoverOptions {
        base_url: &args.url,
        max_depth: args.depth,
        use_sitemap: !args.no_sitemap,
        crawl_fallback: !args.sitemap_only,
        renderer: &renderer,
        max_concurrency: args.concurrency,
        requests_per_second: args.rate_limit,
        user_agent: "crw/0.7.0",
        proxy: args.proxy,
        deadline_ms_per_page: args.timeout,
        per_host_max_concurrent: 2,
        url_filter: None,
        max_urls: args.limit,
        // The CLI has no outer timeout, so discovery would otherwise be
        // unbounded. Same default budget as the HTTP route, stated explicitly.
        overall_deadline: crw_crawl::crawl::discovery_deadline(std::time::Duration::from_secs(
            CLI_MAP_TIMEOUT_SECS,
        )),
        respect_robots: true,
        filter_by_path: !args.no_filter_by_path,
    };

    let result = match discover_urls(opts).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: map failed: {e}");
            return Err(CmdError::code_only(1));
        }
    };

    match args.format {
        MapFormat::Text => {
            for url in &result.urls {
                println!("{url}");
            }
            eprintln!();
            eprintln!("Discovered {} URLs", result.urls.len());
            if result.dropped_action_count > 0 {
                eprintln!("  Dropped {} action URLs", result.dropped_action_count);
            }
            if result.stripped_tracking_count > 0 {
                eprintln!(
                    "  Stripped tracking from {} URLs",
                    result.stripped_tracking_count
                );
            }
            // stderr, not stdout: stdout stays a clean pipeable list of pages.
            for sm in &result.sitemaps {
                eprintln!("  Sitemap: {sm}");
            }
        }
        MapFormat::Json => {
            let output = serde_json::json!({
                "success": true,
                "links": result.urls,
                "droppedActionCount": result.dropped_action_count,
                "strippedTrackingCount": result.stripped_tracking_count,
                "sitemaps": result.sitemaps,
            });
            match serde_json::to_string_pretty(&output) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("error: failed to serialize JSON: {e}");
                    return Err(CmdError::code_only(1));
                }
            }
        }
    }
    Ok(())
}
