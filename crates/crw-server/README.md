# crw-server

Firecrawl-compatible API server for the [CRW](https://github.com/fastcrw/crw) web scraper.

[![crates.io](https://img.shields.io/crates/v/crw-server.svg)](https://crates.io/crates/crw-server)
[![docs.rs](https://docs.rs/crw-server/badge.svg)](https://docs.rs/crw-server)
[![license](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](https://github.com/fastcrw/crw/blob/main/LICENSE)

## Overview

`crw-server` is the main CRW binary — an Axum-based HTTP server that provides a Firecrawl-compatible REST API and built-in MCP transport. Single binary, ~14 MB idle RAM, no Redis, no Node.js.

- **Firecrawl-compatible API** — `/v1/scrape`, `/v1/crawl`, `/v1/map` with identical request/response format
- **MCP transport** — Built-in Streamable HTTP MCP endpoint at `/mcp` for Claude Code, Cursor, Windsurf
- **Auth middleware** — Optional Bearer token auth with constant-time comparison (no timing leaks)
- **JS rendering** — Auto-detect SPAs, render via LightPanda/Playwright/Chrome (CDP)
- **LLM extraction** — JSON schema → structured data via Anthropic tool_use or OpenAI function calling
- **One-command setup** — `crw-server setup` downloads LightPanda and configures JS rendering

## Installation

```bash
cargo install crw-server
```

## Quick start

```bash
# Start the server
crw-server

# Enable JS rendering (downloads LightPanda)
crw-server setup
```

## API endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| `POST` | `/v1/scrape` | Scrape a single URL, optionally with LLM extraction |
| `POST` | `/v1/crawl` | Start async BFS crawl (returns job ID) |
| `GET` | `/v1/crawl/:id` | Check crawl status and retrieve results |
| `DELETE` | `/v1/crawl/:id` | Cancel a running crawl job |
| `POST` | `/v1/map` | Discover all URLs on a site |
| `GET` | `/health` | Health check (no auth required) |
| `POST` | `/mcp` | Streamable HTTP MCP transport |

## Usage examples

**Scrape a page:**

```bash
curl -X POST http://localhost:3000/v1/scrape \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com", "formats": ["markdown", "links"]}'
```

**Start a crawl:**

```bash
curl -X POST http://localhost:3000/v1/crawl \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com", "maxDepth": 2, "limit": 50}'
```

**LLM structured extraction:**

```bash
curl -X POST http://localhost:3000/v1/scrape \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://example.com/product",
    "formats": ["json"],
    "jsonSchema": {
      "type": "object",
      "properties": {
        "name": { "type": "string" },
        "price": { "type": "number" }
      },
      "required": ["name", "price"]
    }
  }'
```

**Discover URLs on a site:**

```bash
curl -X POST http://localhost:3000/v1/map \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com"}'
```

## Configuration

CRW uses layered TOML configuration with environment variable overrides:

```toml
[server]
host = "0.0.0.0"
port = 3000
request_timeout_secs = 120
rate_limit_rps = 10        # Max requests/second (global). 0 = unlimited.

[renderer]
mode = "auto"  # auto | lightpanda | playwright | chrome | none
# render_js_default = true   # alias: force_js = true — force JS for every request that omits `renderJs`

[crawler]
max_concurrency = 10
requests_per_second = 10.0
respect_robots_txt = true

[auth]
# api_keys = ["crw_live_key-1234"]

[extraction.llm]
provider = "anthropic"  # "anthropic" or "openai"
# api_key = "sk-..."    # or CRW_EXTRACTION__LLM__API_KEY env var
```

Override with environment variables:

```bash
CRW_SERVER__PORT=8080 CRW_CRAWLER__MAX_CONCURRENCY=20 crw-server
```

## Docker

```bash
# Pre-built image
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest

# With JS rendering sidecar
docker compose up
```

## Using as a library

```rust,no_run
use crw_server::app::create_app;
use crw_server::state::AppState;

#[tokio::main]
async fn main() {
    let config = crw_core::AppConfig::load().unwrap();
    let state = AppState::new(config).await;
    let app = create_app(state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

## Part of CRW

This crate is part of the [CRW](https://github.com/fastcrw/crw) workspace — a fast, lightweight, Firecrawl-compatible web scraper built in Rust.

This crate (`crw-server`) is the Axum HTTP API - both a library (embedded by
`crw-cli` for `crw serve`) and its own binary (the production container
entrypoint). For the full crate table and dependency graph, see
[docs.fastcrw.com/architecture/](https://docs.fastcrw.com/architecture/).

## License

AGPL-3.0 — see [LICENSE](https://github.com/fastcrw/crw/blob/main/LICENSE).
