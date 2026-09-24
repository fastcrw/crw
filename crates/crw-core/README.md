# crw-core

Core types, configuration, and error handling for the [CRW](https://github.com/fastcrw/crw) web scraper.

[![crates.io](https://img.shields.io/crates/v/crw-core.svg)](https://crates.io/crates/crw-core)
[![docs.rs](https://docs.rs/crw-core/badge.svg)](https://docs.rs/crw-core)
[![license](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](https://github.com/fastcrw/crw/blob/main/LICENSE)

## Overview

`crw-core` provides the foundational building blocks shared across all CRW crates:

- **Configuration** — Layered TOML config with environment variable overrides (`AppConfig`)
- **Error handling** — Unified error type (`CrwError`) and result alias (`CrwResult`)
- **Shared types** — `ScrapeRequest`, `ScrapeData`, `FetchResult`, `OutputFormat`, `ChunkStrategy`, and more
- **SSRF protection** — URL validation that blocks private IPs, cloud metadata endpoints, loopback, and non-HTTP schemes
- **MCP types** — JSON-RPC request/response types for MCP protocol support

## Installation

```bash
cargo add crw-core
```

## Usage

### Configuration

CRW uses layered configuration: built-in defaults → `config.local.toml` → environment variables.

```rust
use crw_core::AppConfig;

let config = AppConfig::load().unwrap();
println!("Server port: {}", config.server.port);
println!("Renderer mode: {}", config.renderer.mode);
println!("Max concurrency: {}", config.crawler.max_concurrency);
```

Override any setting with environment variables using the `CRW_` prefix:

```bash
CRW_SERVER__PORT=8080 CRW_CRAWLER__MAX_CONCURRENCY=20 ./my-app
```

### Error handling

All CRW crates return `CrwResult<T>`, which uses the unified `CrwError` enum:

```rust
use crw_core::{CrwError, CrwResult};

fn fetch_page(url: &str) -> CrwResult<String> {
    if url.is_empty() {
        return Err(CrwError::InvalidRequest("URL cannot be empty".into()));
    }
    // ...
    Ok("page content".into())
}
```

Error variants: `HttpError`, `UrlParseError`, `InvalidRequest`, `RendererError`, `ExtractionError`, `CrawlError`, `Timeout`, `ConfigError`, `NotFound`, `RateLimited`, `Internal`.

Each variant maps to a machine-readable `error_code` string via `CrwError::error_code()` (e.g. `"invalid_url"`, `"rate_limited"`, `"not_found"`).

### SSRF protection

Validate URLs before fetching to prevent server-side request forgery:

```rust
use crw_core::url_safety::validate_safe_url;

let url = url::Url::parse("https://example.com").unwrap();
assert!(validate_safe_url(&url).is_ok());

let private = url::Url::parse("http://169.254.169.254/metadata").unwrap();
assert!(validate_safe_url(&private).is_err()); // blocks AWS metadata
```

Use `safe_redirect_policy()` with reqwest to block SSRF via redirects:

```rust
use crw_core::url_safety::safe_redirect_policy;

let client = reqwest::Client::builder()
    .redirect(safe_redirect_policy())
    .build()
    .unwrap();
```

### Shared types

```rust
use crw_core::types::{OutputFormat, ScrapeRequest};

let request = ScrapeRequest {
    url: "https://example.com".into(),
    formats: Some(vec![OutputFormat::Markdown, OutputFormat::Links]),
    ..Default::default()
};
```

## Part of CRW

This crate is part of the [CRW](https://github.com/fastcrw/crw) workspace — a fast, lightweight, Firecrawl-compatible web scraper built in Rust.

This crate (`crw-core`) provides core types, config, and error handling. For
the full crate table and dependency graph, see
[docs.fastcrw.com/architecture/](https://docs.fastcrw.com/architecture/).

## License

AGPL-3.0 — see [LICENSE](https://github.com/fastcrw/crw/blob/main/LICENSE).
