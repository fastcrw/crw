# crw-cli

Standalone CLI tool for scraping URLs to markdown, JSON, HTML, or plain text — no server needed.

[![crates.io](https://img.shields.io/crates/v/crw-cli.svg)](https://crates.io/crates/crw-cli)
[![license](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](https://github.com/fastcrw/crw/blob/main/LICENSE)

## Overview

`crw-cli` is a single-binary web scraper that fetches any URL and outputs clean content to stdout. Part of the [CRW](https://github.com/fastcrw/crw) project — same extraction engine as the server, but with zero setup.

- **6 output formats** — markdown, JSON, HTML, raw HTML, plain text, links
- **Main content extraction** — automatically strips nav, footer, ads, scripts
- **CSS selector & XPath** — extract specific elements before conversion
- **Stealth mode** — User-Agent rotation and browser-like headers
- **JS rendering** — optional CDP-based rendering for SPAs (via `--js` flag)
- **Proxy support** — per-request HTTP, HTTPS, or SOCKS5 proxy
- **File output** — write directly to a file with `-o`

## Installation

```bash
cargo install crw-cli
```

This installs the `crw` binary.

## Usage

### Basic scraping

```bash
# Scrape a page to markdown (default)
crw https://example.com

# Output as JSON (includes all metadata)
crw https://example.com --format json

# Output as plain text
crw https://example.com --format text

# Output as HTML (cleaned)
crw https://example.com --format html

# Output raw HTML (no cleaning)
crw https://example.com --format rawhtml

# Extract all links
crw https://example.com --format links
```

### CSS selector extraction

Extract only specific elements:

```bash
# Extract just the article content
crw https://blog.example.com --css 'article.post'

# Extract the main heading
crw https://example.com --css 'h1'
```

### XPath extraction

```bash
# Extract all paragraph text
crw https://example.com --xpath '//p'

# Extract a specific element by ID
crw https://example.com --xpath '//*[@id="content"]'
```

### Save to file

```bash
crw https://example.com -o page.md
crw https://example.com --format json -o page.json
```

### Full page content (no main content extraction)

By default, `crw` strips boilerplate (nav, footer, ads). Use `--raw` to get everything:

```bash
crw https://example.com --raw
```

### Stealth mode

Rotate User-Agent and inject browser-like headers to reduce bot detection:

```bash
crw https://protected-site.com --stealth
```

### Proxy

Route requests through a proxy:

```bash
crw https://example.com --proxy http://user:pass@proxy:8080
```

### JavaScript rendering

For SPAs that require JavaScript, use `--js`. The CLI auto-detects a locally
installed LightPanda or Chrome; you can also point at an existing CDP WebSocket
with `CRW_CDP_URL`:

```bash
# Auto-detect (spawns LightPanda/Chrome if installed)
crw https://spa-app.com --js

# Or point at a running CDP endpoint
CRW_CDP_URL=ws://127.0.0.1:9222 crw https://spa-app.com --js
```

Note: `CRW_CDP_URL` is only honored by `crw` (the CLI). In server/MCP mode use
`[renderer.lightpanda.ws_url]` / `[renderer.chrome.ws_url]` (or the matching
`CRW_RENDERER__*__WS_URL` env vars).

## All options

```
Usage: crw [OPTIONS] <URL>

Arguments:
  <URL>  URL to scrape (http or https)

Options:
  -f, --format <FORMAT>      Output format [default: markdown]
                              [values: markdown, json, html, rawhtml, text, links]
  -o, --output <FILE>        Write output to file instead of stdout
      --raw                  Disable main content extraction (full page)
      --js                   Force JS rendering (auto-detects LightPanda/Chrome, or CRW_CDP_URL)
      --css <SELECTOR>       Extract only elements matching this CSS selector
      --xpath <EXPR>         Extract only elements matching this XPath expression
      --proxy <URL>          HTTP, HTTPS, or SOCKS5 proxy URL (e.g. socks5://user:pass@host:1080)
      --stealth              Enable stealth mode (UA rotation + browser headers)
  -h, --help                 Print help
```

## Part of CRW

This crate is part of the [CRW](https://github.com/fastcrw/crw) workspace — a fast, lightweight, Firecrawl-compatible web scraper built in Rust.

This crate (`crw-cli`) builds the standalone `crw` binary. For the full
crate table and dependency graph, see
[docs.fastcrw.com/architecture/](https://docs.fastcrw.com/architecture/).

## License

AGPL-3.0 — see [LICENSE](https://github.com/fastcrw/crw/blob/main/LICENSE).
