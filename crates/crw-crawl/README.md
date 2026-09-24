# crw-crawl

Async BFS web crawler with rate limiting and robots.txt support for the [CRW](https://github.com/fastcrw/crw) web scraper.

[![crates.io](https://img.shields.io/crates/v/crw-crawl.svg)](https://crates.io/crates/crw-crawl)
[![docs.rs](https://docs.rs/crw-crawl/badge.svg)](https://docs.rs/crw-crawl)
[![license](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](https://github.com/fastcrw/crw/blob/main/LICENSE)

## Overview

`crw-crawl` provides the crawling and scraping engine used by CRW:

- **BFS crawler** — Breadth-first crawl with configurable depth (max 10), page limit (max 1000), and concurrency
- **Single-page scrape** — `scrape_url()` fetches, extracts, and optionally runs LLM structured extraction in one call
- **robots.txt** — Full parser with `Allow`/`Disallow`, wildcard patterns (`*`, `$`), and RFC 9309 specificity rules
- **Sitemap parser** — Discovers URLs from `sitemap.xml` and sitemap index files
- **Rate limiting** — Global per-domain rate limiter with configurable RPS and jitter to avoid traffic fingerprinting
- **SSRF protection** — Validates every URL before fetching (private IPs, cloud metadata, non-HTTP schemes)
- **Warning detection** — Identifies anti-bot pages, 4xx responses, and interstitial content

## Installation

```bash
cargo add crw-crawl
```

## Usage

### Single-page scrape

The simplest entry point — fetch a URL, extract content, and optionally run LLM extraction:

```rust,no_run
use crw_crawl::single::scrape_url;
use crw_core::types::{ScrapeRequest, OutputFormat};
use crw_renderer::FallbackRenderer;
use crw_core::config::{RendererConfig, StealthConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = RendererConfig::default();
    let stealth = StealthConfig::default();
    let renderer = Arc::new(FallbackRenderer::new(&config, "my-crawler/1.0", None, &stealth));

    let request = ScrapeRequest {
        url: "https://example.com".into(),
        formats: vec![OutputFormat::Markdown, OutputFormat::Links],
        ..Default::default()
    };

    let result = scrape_url(&request, &renderer, None, "my-crawler/1.0", false).await?;
    println!("Title: {:?}", result.metadata.title);
    println!("Markdown: {}", result.markdown.unwrap_or_default());
    Ok(())
}
```

### BFS crawl

Run a full breadth-first crawl with concurrent workers:

```rust,no_run
use crw_crawl::crawl::{CrawlOptions, run_crawl};
use crw_core::types::{CrawlRequest, CrawlState, CrawlStatus};
use crw_renderer::FallbackRenderer;
use crw_core::config::{RendererConfig, StealthConfig};
use std::sync::Arc;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = RendererConfig::default();
    let stealth = StealthConfig::default();
    let renderer = Arc::new(FallbackRenderer::new(&config, "my-crawler/1.0", None, &stealth));

    let (state_tx, mut state_rx) = tokio::sync::watch::channel(CrawlState {
        status: CrawlStatus::Scraping,
        completed: 0,
        total: 0,
        data: vec![],
    });

    let options = CrawlOptions {
        id: Uuid::new_v4(),
        req: CrawlRequest {
            url: "https://example.com".into(),
            max_depth: Some(2),
            limit: Some(10),
            ..Default::default()
        },
        renderer,
        max_concurrency: 5,
        respect_robots: true,
        requests_per_second: 2.0,
        user_agent: "my-crawler/1.0",
        state_tx,
        llm_config: None,
        proxy: None,
        jitter_factor: 0.2,
    };

    run_crawl(options).await;
    let final_state = state_rx.borrow().clone();
    println!("Crawled {} pages", final_state.completed);
    Ok(())
}
```

### robots.txt

Parse and check robots.txt rules:

```rust
use crw_crawl::robots::RobotsTxt;

let robots_txt = r#"
User-agent: *
Disallow: /admin/
Allow: /admin/public/
Sitemap: https://example.com/sitemap.xml
"#;

let robots = RobotsTxt::parse(robots_txt, "my-crawler");
assert!(!robots.is_allowed("/admin/secret"));
assert!(robots.is_allowed("/admin/public/page"));
assert!(robots.is_allowed("/about"));
assert_eq!(robots.sitemaps(), &["https://example.com/sitemap.xml"]);
```

### Sitemap parsing

Discover URLs from sitemap XML:

```rust
use crw_crawl::sitemap::parse_sitemap;

let xml = r#"<?xml version="1.0"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url><loc>https://example.com/page1</loc></url>
  <url><loc>https://example.com/page2</loc></url>
</urlset>"#;

let urls = parse_sitemap(xml);
assert_eq!(urls.len(), 2);
```

## Configuration

When used via CRW server, crawl behavior is configured in `config.local.toml`:

```toml
[crawler]
max_concurrency = 10        # concurrent workers per crawl job
requests_per_second = 10.0  # per-domain rate limit
respect_robots_txt = true   # honor robots.txt rules
jitter_factor = 0.2         # ±20% randomized delay between requests
```

## Safety limits

| Limit | Value | Purpose |
|-------|-------|---------|
| Max crawl depth | 10 | Prevents infinite recursion |
| Max pages per crawl | 1,000 | Bounds memory and time |
| Max discovered URLs | 5,000 | Prevents memory exhaustion |
| Rate limiter TTL | 1 hour | Cleans up stale domain limiters |

## Part of CRW

This crate is part of the [CRW](https://github.com/fastcrw/crw) workspace — a fast, lightweight, Firecrawl-compatible web scraper built in Rust.

This crate (`crw-crawl`) is the async BFS crawler with robots.txt & sitemap
support. For the full crate table and dependency graph, see
[docs.fastcrw.com/architecture/](https://docs.fastcrw.com/architecture/).

## License

AGPL-3.0 — see [LICENSE](https://github.com/fastcrw/crw/blob/main/LICENSE).
