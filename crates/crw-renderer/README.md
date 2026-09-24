# crw-renderer

HTTP and headless-browser rendering engine for the [CRW](https://github.com/fastcrw/crw) web scraper.

[![crates.io](https://img.shields.io/crates/v/crw-renderer.svg)](https://crates.io/crates/crw-renderer)
[![docs.rs](https://docs.rs/crw-renderer/badge.svg)](https://docs.rs/crw-renderer)
[![license](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](https://github.com/fastcrw/crw/blob/main/LICENSE)

## Overview

`crw-renderer` fetches web pages via plain HTTP and optionally re-renders them through a CDP-based headless browser when SPA content is detected.

- **`FallbackRenderer`** — Composite renderer: tries HTTP first, falls back to JS rendering when the page looks like a SPA shell
- **`HttpFetcher`** — Fast reqwest-based HTTP fetcher with stealth headers, gzip/brotli decompression, and proxy support
- **SPA detection** — Heuristic analysis of the HTML response (empty body, framework markers like `__NEXT_DATA__`, `ng-app`, `nuxt`) to auto-detect pages that need JS rendering
- **CDP rendering** — Chrome DevTools Protocol support for LightPanda, Playwright, and Chrome (requires `cdp` feature)
- **Stealth mode** — User-Agent rotation from a built-in Chrome/Firefox/Safari pool and browser-like header injection

## Installation

```bash
cargo add crw-renderer
```

With CDP (headless browser) support:

```bash
cargo add crw-renderer --features cdp
```

## Feature flags

| Flag | Default | Description |
|------|---------|-------------|
| `cdp` | off | Enables CDP WebSocket rendering via `tokio-tungstenite` (LightPanda, Playwright, Chrome) |

## Usage

### Basic HTTP fetching

```rust,no_run
use crw_core::config::{RendererConfig, StealthConfig};
use crw_renderer::FallbackRenderer;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = RendererConfig::default();
    let stealth = StealthConfig::default();
    let renderer = FallbackRenderer::new(&config, "my-app/1.0", None, &stealth);

    let result = renderer.fetch("https://example.com", &HashMap::new(), None, None, None).await?;
    println!("Status: {}", result.status_code);
    println!("HTML length: {}", result.html.len());
    Ok(())
}
```

### Smart mode (auto-detect SPAs)

When a JS renderer is configured, `FallbackRenderer` automatically detects SPA shells and re-renders with a headless browser:

```rust,no_run
use crw_core::config::{RendererConfig, StealthConfig, CdpEndpoint};
use crw_renderer::FallbackRenderer;
use std::collections::HashMap;

let config = RendererConfig {
    mode: "auto".into(),
    lightpanda: Some(CdpEndpoint { ws_url: "ws://127.0.0.1:9222".into() }),
    ..Default::default()
};
let stealth = StealthConfig::default();
let renderer = FallbackRenderer::new(&config, "my-app/1.0", None, &stealth);

// Auto mode: HTTP first, JS rendering if SPA detected
let result = renderer.fetch("https://spa-app.com", &HashMap::new(), None, None, None).await?;
```

### Pinned renderer (per-request)

The 5th `fetch` argument is `requested_renderer: Option<&str>`. Pass `Some("chrome")`, `Some("lightpanda")`, or `Some("playwright")` to hard-pin a single renderer (no fallback). `None` or `Some("auto")` uses the configured chain.

> **Breaking change in 0.5:** `FallbackRenderer::fetch` (and `fetch_with_js`) gained a 5th argument. Direct consumers of this crate need to pass `None` to preserve previous behavior.

### SPA detection

Use the detector directly to check if HTML needs JS rendering:

```rust
use crw_renderer::detector::needs_js_rendering;

let spa_html = r#"<html><body><div id="root"></div><script src="/app.js"></script></body></html>"#;
assert!(needs_js_rendering(spa_html));

let static_html = r#"<html><body><h1>Hello</h1><p>This is a static page with content.</p></body></html>"#;
assert!(!needs_js_rendering(static_html));
```

### Stealth mode

Enable User-Agent rotation and browser-like headers to reduce bot detection:

```rust,no_run
use crw_core::config::{RendererConfig, StealthConfig};
use crw_renderer::FallbackRenderer;

let stealth = StealthConfig {
    enabled: true,
    inject_headers: true,
    user_agents: vec![], // uses built-in Chrome/Firefox/Safari pool
};
let renderer = FallbackRenderer::new(&RendererConfig::default(), "crw/1.0", None, &stealth);
```

### Health check

```rust,no_run
let health = renderer.check_health().await;
for (name, available) in &health {
    println!("{name}: {}", if *available { "ok" } else { "down" });
}
```

## Part of CRW

This crate is part of the [CRW](https://github.com/fastcrw/crw) workspace — a fast, lightweight, Firecrawl-compatible web scraper built in Rust.

This crate (`crw-renderer`) is the HTTP + CDP browser rendering engine. For
the full crate table and dependency graph, see
[docs.fastcrw.com/architecture/](https://docs.fastcrw.com/architecture/).

## License

AGPL-3.0 — see [LICENSE](https://github.com/fastcrw/crw/blob/main/LICENSE).
