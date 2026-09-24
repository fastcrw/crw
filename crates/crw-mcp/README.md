# crw-mcp

MCP (Model Context Protocol) server for the [CRW](https://github.com/fastcrw/crw) web scraper.

[![crates.io](https://img.shields.io/crates/v/crw-mcp.svg)](https://crates.io/crates/crw-mcp)
[![license](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](https://github.com/fastcrw/crw/blob/main/LICENSE)

## Overview

`crw-mcp` is a self-contained MCP server that gives any MCP-compatible AI client (Claude Code, Claude Desktop, Cursor, Windsurf, Cline, Continue.dev, OpenAI Codex CLI) 9 web scraping tools. No external server needed — just add and go.

**Two modes:**

| Mode | When | Setup |
|------|------|-------|
| **Embedded** (default) | `CRW_LOCAL` truthy, or no API URL resolved | Self-contained, zero setup |
| **Proxy** | `CRW_LOCAL` unset or falsy, and an API URL resolved | Forwards to remote CRW server |

**9 MCP tools:**

| Tool | Description |
|------|-------------|
| `crw_scrape` | Scrape a single URL → markdown, HTML, links |
| `crw_crawl` | Start an async BFS crawl (returns job ID) |
| `crw_check_crawl_status` | Poll crawl job status and retrieve results |
| `crw_map` | Discover all URLs on a website |
| `crw_extract` | Extract structured JSON from URLs via a prompt and/or JSON schema (async job, returns job ID) |
| `crw_check_extract_status` | Poll extract job status and retrieve results |
| `crw_cancel_extract` | Idempotently cancel an extract job and retrieve canonical status |
| `crw_search` | Search the web (needs a configured search backend; always available in proxy mode, available in embedded mode only when a search backend is configured) |
| `crw_parse_file` | Parse a local PDF (base64) to markdown |

> **Output bounding:** By default, content fields are truncated to ~15 000 chars and `crw_map` returns at most 100 URLs to keep agent context small. Pass `maxLength: 0` (scrape / check_status / parse_file) or `limit: 0` (map) to opt out.

## Installation

```bash
# npm — prebuilt binary (Linux builds are static musl, so they run on any
# distro/glibc; macOS and Windows included)
npm install -g crw-mcp

# or build from source (Rust)
cargo install crw-mcp
```

## Quick Start (Embedded Mode)

No server to run. Just add `crw-mcp` to your AI client:

```bash
# Claude Code
claude mcp add crw -- crw-mcp

# With custom config via env vars.
# NOTE: the server name (crw) MUST come BEFORE the -e flags — `-e` is variadic
# and will otherwise swallow the name ("Invalid environment variable: crw").
claude mcp add crw \
  -e CRW_CRAWLER__MAX_CONCURRENCY=5 \
  -e CRW_RENDERER__LIGHTPANDA__WS_URL=ws://127.0.0.1:9222 \
  -- crw-mcp
```

## Proxy Mode (Remote Server)

Connect to [fastcrw.com](https://fastcrw.com) or any remote CRW instance:

```bash
# Cloud server (name `crw` comes BEFORE -e, command after `--`)
claude mcp add crw \
  -e CRW_API_URL=https://api.fastcrw.com \
  -e CRW_API_KEY=crw_live_xxx \
  -- crw-mcp

# Local crw-server on custom port
claude mcp add crw \
  -e CRW_API_URL=http://localhost:4000 \
  -- crw-mcp
```

Or use the HTTP transport directly (no `crw-mcp` binary needed):

```bash
claude mcp add --transport http crw http://localhost:3000/mcp
```

## CLI Options

| Flag | Env Var | Description |
|------|---------|-------------|
| `--api-url` | `CRW_API_URL` | Remote server URL (enables proxy mode, unless `CRW_LOCAL` is truthy) |
| n/a | `CRW_LOCAL` | Truthy pins embedded mode and ignores any configured API URL |
| `--api-key` | `CRW_API_KEY` | Bearer token for remote server auth |
| `--config` | `CRW_CONFIG` | Config file path (embedded mode only) |
| — | `RUST_LOG` | Log level (default: `crw_mcp=info`, logs go to stderr) |

### Embedded mode configuration

In embedded mode, `crw-mcp` loads configuration the same way as `crw-server`: `config.default.toml` → `config.local.toml` → environment variable overrides. Env vars use `CRW_` prefix with `__` as separator:

```bash
CRW_CRAWLER__MAX_CONCURRENCY=5
CRW_RENDERER__LIGHTPANDA__WS_URL=ws://127.0.0.1:9222
CRW_CRAWLER__USER_AGENT="MyBot/1.0"
```

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `embedded` | on | Self-contained scraping engine (pulls in `crw-server`) |

Build a slim proxy-only binary without the embedded engine:

```bash
cargo build --profile release-small --no-default-features -p crw-mcp
```

This yields a ~4.2 MB binary (vs ~17 MB for the default embedded build) because the `embedded` feature gates the headless-browser engine (`crw-renderer`) and `crw-server`.

## Setup by Client

### Claude Code

```bash
claude mcp add crw -- crw-mcp
```

### Claude Desktop

Edit your config file:

| OS | Path |
|---|---|
| macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |
| Linux | `~/.config/Claude/claude_desktop_config.json` |

```json
{
  "mcpServers": {
    "crw": {
      "command": "crw-mcp"
    }
  }
}
```

### Cursor

Edit `~/.cursor/mcp.json` (global) or `.cursor/mcp.json` (project):

```json
{
  "mcpServers": {
    "crw": {
      "command": "crw-mcp"
    }
  }
}
```

### Windsurf

Edit `~/.codeium/windsurf/mcp_config.json`:

```json
{
  "mcpServers": {
    "crw": {
      "command": "crw-mcp"
    }
  }
}
```

### Cline (VS Code)

```json
{
  "mcpServers": {
    "crw": {
      "command": "crw-mcp",
      "alwaysAllow": ["crw_scrape", "crw_map"],
      "disabled": false
    }
  }
}
```

### Continue.dev (VS Code / JetBrains)

Edit `~/.continue/config.yaml`:

```yaml
mcpServers:
  - name: crw
    command: crw-mcp
```

### OpenAI Codex CLI

Edit `~/.codex/config.toml`:

```toml
[mcp_servers.crw]
command = "crw-mcp"
```

### Any MCP client

```json
{
  "mcpServers": {
    "crw": {
      "command": "crw-mcp"
    }
  }
}
```

> **Tip:** For clients that support HTTP transport, you can still use `http://localhost:3000/mcp` directly with a running `crw-server` — no stdio binary needed.

## With Proxy Mode Authentication

```json
{
  "mcpServers": {
    "crw": {
      "command": "crw-mcp",
      "env": {
        "CRW_API_URL": "https://api.fastcrw.com",
        "CRW_API_KEY": "crw_live_YOUR_KEY"
      }
    }
  }
}
```

## Part of CRW

This crate is part of the [CRW](https://github.com/fastcrw/crw) workspace — a fast, lightweight, Firecrawl-compatible web scraper built in Rust.

This crate (`crw-mcp`) is the standalone MCP server binary. For the full
crate table and dependency graph, see
[docs.fastcrw.com/architecture/](https://docs.fastcrw.com/architecture/).

## License

AGPL-3.0 — see [LICENSE](https://github.com/fastcrw/crw/blob/main/LICENSE).
