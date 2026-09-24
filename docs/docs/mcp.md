# MCP Server for AI Agents

CRW includes a built-in MCP (Model Context Protocol) server that gives any MCP-compatible AI assistant — Claude Code, Claude Desktop, Cursor, Windsurf, Cline, Continue.dev, OpenAI Codex CLI — 9 web scraping tools (8 on a self-host with no search backend configured, since `crw_search` is hidden in that case). Turn any AI coding agent into a web scraper with a single command.

> Also available on the [MCP Registry](https://registry.modelcontextprotocol.io/?q=crw)

## Two Modes

`crw-mcp` supports two modes:

| Mode | When | Tools | Description |
|------|------|-------|-------------|
| **Embedded** (default) | `CRW_LOCAL` truthy, or no API URL resolved | scrape, crawl, check_crawl_status, map, extract, check_extract_status, cancel_extract, parse_file + **search** (when a search backend is configured) | Self-contained. No server needed. The scraping engine runs inside the MCP process. `crw_search` is advertised only when a search backend is configured (e.g. the Docker compose sidecar). |
| **Proxy / Server** | `CRW_LOCAL` unset or falsy, and an API URL resolved | scrape, crawl, check_crawl_status, map, extract, check_extract_status, cancel_extract, parse_file, search | Forwards tool calls to a remote CRW server: the [fastcrw.com](https://fastcrw.com) cloud **or your own self-hosted server**. `crw_search` is always advertised in proxy mode; it works whenever the server has a search backend configured (the Docker stack enables it by default). |

An API URL resolves from `--api-url`, `CRW_API_URL`, `CRW_CLIENT__API_URL`, or
`client.api_url` in `~/.config/crw/config.toml`, which is where `crw setup`
writes it. A truthy `CRW_LOCAL` overrides all four and pins embedded mode, so a
run you asked to keep local is never billed or sent to a remote server. Truthy
means anything except `0`, `false`, `no` or empty, matching the SDKs. This
applies to the MCP entry point; other subcommands such as `crw search` still
read `client.api_url` on their own.

## Where to use what

- Use this page for the MCP model, tool list, and transport choices.
- Use [MCP Client Setup](#mcp-clients) for Claude Code, Codex, Cursor, Windsurf, Cline, Continue, and similar host-specific config snippets.

## When MCP Helps

MCP is useful when the agent host already expects tools to be registered through a standard interface. That reduces one layer of custom glue code between the agent and your scraping service.

Typical fits:

- agentic research workflows,
- internal copilots that need current website content,
- multi-tool assistants that combine search, scrape, and synthesis,
- and developer environments such as Claude or Cursor where MCP is already the preferred integration path.

## Quick Start (Embedded Mode)

No server to start, no setup. Install and add `crw-mcp`:

```bash
# One-line install (auto-detects OS & arch):
curl -fsSL https://fastcrw.com/install | sh

# npm (zero install):
npx crw-mcp

# Python (SDK — use npx or cargo for MCP binary):
pip install crw

# Cargo:
cargo install crw-mcp

# Docker:
docker run -i ghcr.io/fastcrw/crw crw-mcp
```

Add to your MCP client:

```bash
# Claude Code:
claude mcp add crw -- npx -y crw-mcp

# OpenAI Codex CLI:
codex mcp add crw -- npx crw-mcp
```

That's it. The agent starts `crw-mcp`, which contains the full scraping engine. When the agent disconnects, the process dies.

If you want host-by-host config files instead of one-liners, jump to [MCP Client Setup](#mcp-clients).

:::note
500 credits to start, up to 500 more from onboarding tasks, no card.
<a href="https://fastcrw.com/register?ref=docs-mcp-inline" target="_blank" rel="noopener">Get your API key</a>
:::

### With CDP rendering (LightPanda/Chrome)

If you have a CDP-compatible browser, pass it via env vars:

```bash
claude mcp add crw \
  -e CRW_RENDERER__LIGHTPANDA__WS_URL=ws://127.0.0.1:9222 \
  -- npx -y crw-mcp
```

Without a CDP browser, `crw-mcp` uses its HTTP-only renderer (no JavaScript rendering).

### Embedded mode configuration

In embedded mode, `crw-mcp` loads config the same way as `crw-server`: `config.default.toml` → `config.local.toml` → env var overrides. Env vars use `CRW_` prefix with `__` separator:

```bash
CRW_CRAWLER__MAX_CONCURRENCY=5
CRW_RENDERER__LIGHTPANDA__WS_URL=ws://127.0.0.1:9222
CRW_CRAWLER__USER_AGENT="MyBot/1.0"
```

## Proxy Mode (Remote Server)

Connect to [fastcrw.com](https://fastcrw.com) or any remote CRW instance:

```bash
# Cloud server
claude mcp add crw \
  -e CRW_API_URL=https://api.fastcrw.com \
  -e CRW_API_KEY=YOUR_API_KEY \
  -- npx -y crw-mcp

# Local crw-server on custom port
claude mcp add crw \
  -e CRW_API_URL=http://localhost:4000 \
  -- npx -y crw-mcp
```

The same `CRW_API_URL` + `CRW_API_KEY` env block also works in Codex, Claude Desktop, Cursor, Windsurf, Cline, and Continue. See [MCP Client Setup](#mcp-clients) for ready-to-paste config files.

## Three Transport Options

| Transport | Setup | Requires |
|-----------|-------|----------|
| **Stdio embedded** (recommended) | `claude mcp add crw -- npx -y crw-mcp` | Nothing |
| **Stdio proxy** | `CRW_API_URL=... npx crw-mcp` | Remote CRW server |
| **HTTP** | `claude mcp add --transport http crw http://localhost:3000/mcp` | `crw-server` running |

### HTTP Transport

The `crw-server` has a built-in `/mcp` endpoint. No extra binary needed:

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
| `--hide-credits` | `CRW_MCP__HIDE_CREDITS` | Strip `creditCost`/`creditsUsed` from tool responses |
| — | `RUST_LOG` | Log level (default: `crw_mcp=info`, logs go to stderr) |

> **Hiding credit fields (self-hosted):** credits exist for the managed SaaS
> billing layer. On a self-hosted deployment they are unused bookkeeping that
> still costs context tokens on every tool response, so `[mcp] hide_credits =
> true` strips them from every tool result. It covers all three surfaces: the
> `/mcp` endpoint and embedded mode strip server-side, and proxy mode strips
> client-side after each call, since a remote's REST body is not shaped by your
> local config. `--hide-credits` is the one-off override. Values you extracted
> yourself are never touched, only the engine's own bookkeeping, and
> `tokensUsed` LLM telemetry is kept.

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `embedded` | on | Self-contained scraping engine (pulls in `crw-server`) |

Build a slim proxy-only binary:

```bash
cargo build --profile release-small --no-default-features -p crw-mcp
```

This yields a ~4.2 MB binary (vs ~17 MB for the default embedded build) because the `embedded` feature gates the headless-browser engine (`crw-renderer`) and `crw-server`.

## Available Tools

| Tool | Description | HTTP Endpoint | Availability |
|------|-------------|---------------|-------------|
| `crw_scrape` | Scrape a URL → markdown, HTML, links | `POST /v1/scrape` | All modes |
| `crw_crawl` | Start async crawl → returns job ID | `POST /v1/crawl` | All modes |
| `crw_check_crawl_status` | Poll crawl status and get results | `GET /v1/crawl/:id` | All modes |
| `crw_map` | Discover all URLs on a site | `POST /v1/map` | All modes |
| `crw_extract` | Extract structured JSON from URLs → async job ID | `POST /v1/extract` | All modes |
| `crw_check_extract_status` | Poll extract status and get results | `GET /v1/extract/:id` | All modes |
| `crw_cancel_extract` | Idempotently cancel extract and return canonical status | `DELETE /v1/extract/:id` | All modes |
| `crw_search` | Search the web → titles, URLs, descriptions | `POST /v1/search` | Always in proxy mode; embedded only when a search backend is configured |
| `crw_parse_file` | Parse a local PDF (base64) → markdown | `POST /firecrawl/v2/parse` (multipart) | All modes |

> **Output bounding:** Tool results are bounded by default to keep agent context small. Content fields (markdown/html/etc.) are truncated to ~15 000 chars — including page content inlined into `crw_search` results via `scrapeOptions` — and `crw_map` returns at most 100 URLs plus at most 100 sitemaps, bounded independently. Truncated responses include `truncated: true`, plus `totalDiscovered` when the link list was cut and `totalSitemaps` when the sitemap list was. Pass `maxLength: 0` (scrape / check_status / parse_file) or `limit: 0` (map) to opt out. `crw_search` does not *advertise* `maxLength`, so an agent will not discover it, but bounding is applied per-tool from the call's own arguments — a hand-written client may still pass `maxLength: 0` on a search to opt out, or a smaller value to tighten it.

## Browser Automation (`crw-browse`)

`crw-browse` is a **separate MCP server** (since v0.4.0) that drives a real Chrome-family browser over CDP for stateful interaction. Use it when the agent needs to navigate multi-step flows, click, or read the DOM — cases where a one-shot scrape is not enough.

| Tool | Description |
|------|-------------|
| `goto` | Navigate the session browser to an `http(s)` URL and wait for load. Creates a session on first call. |
| `tree` | Snapshot the current page as an indented accessibility tree (`[nodeId] role: name`). Requires a prior `goto`. |

See [crw-browse](/docs/crw-browse) for the full list of 14 interaction tools, including click, fill, type_text, evaluate, screenshot, and more. Session state is automatically swept after an idle TTL.

Install and launch:

```bash
# From crates.io
cargo install crw-browse
crw-browse

# Or grab a prebuilt binary from the v0.4.0 GitHub release
# https://github.com/fastcrw/crw/releases/tag/v0.4.0
```

Wire it into your client the same way you do `crw-mcp` — it uses stdio transport and is a self-hosted binary (no cloud account required).

## Best first setups

### Claude Code

```bash
# Local embedded
claude mcp add crw -- npx -y crw-mcp

# fastcrw.com cloud
claude mcp add crw \
  -e CRW_API_URL=https://api.fastcrw.com \
  -e CRW_API_KEY=YOUR_API_KEY \
  -- npx -y crw-mcp
```

### OpenAI Codex CLI

```bash
# Local embedded
codex mcp add crw -- npx crw-mcp
```

For cloud mode and file-based configs, continue in [MCP Client Setup](#mcp-clients).

### crw_scrape

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `url` | string | **yes** | The URL to scrape |
| `formats` | string[] | no | `markdown`, `html`, `links` |
| `onlyMainContent` | boolean | no | Strip nav/footer (default: true) |
| `includeTags` | string[] | no | CSS selectors to keep |
| `excludeTags` | string[] | no | CSS selectors to remove |
| `renderJs` | boolean | no | Enable JS rendering via a CDP browser |
| `waitFor` | integer | no | ms to wait after page load |
| `renderer` | string | no | Renderer to use (e.g. `"lightpanda"`) |
| `maxLength` | integer | no | Max chars for content fields; `0` = unlimited (default: ~15 000) |

### crw_crawl

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `url` | string | **yes** | Starting URL |
| `maxDepth` | integer | no | Max crawl depth (default: 2) |
| `maxPages` | integer | no | Max pages (default: 10) |
| `jsonSchema` | object | no | JSON schema for structured extraction |
| `renderJs` | boolean | no | Enable JS rendering via a CDP browser |
| `waitFor` | integer | no | ms to wait after page load |
| `renderer` | string | no | Renderer to use (e.g. `"lightpanda"`) |

### crw_check_crawl_status

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | string | **yes** | Job ID from `crw_crawl` |
| `maxLength` | integer | no | Max chars for content fields; `0` = unlimited (default: ~15 000) |

### crw_map

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `url` | string | **yes** | URL to map |
| `maxDepth` | integer | no | Discovery depth (default: 2) |
| `useSitemap` | boolean | no | Read sitemap.xml (default: true) |
| `crawlFallback` | boolean | no | Fall back to a short crawl if sitemap is absent (default: true) |
| `limit` | integer | no | Max URLs returned; `0` = unlimited (default: 100) |

### crw_search

In **proxy mode** `crw_search` is always advertised. In **embedded mode** it is advertised only when a search backend is configured (e.g. via the Docker compose sidecar — see [Docker → Search](/docker)). With no backend configured, the tool is hidden from `tools/list`.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `query` | string | **yes** | The search query |
| `limit` | integer | no | Max results (default: 5) |
| `lang` | string | no | Language code (e.g. `"en"`, `"tr"`) |
| `tbs` | string | no | Time-based filter (e.g. `"qdr:d"` for past day) |
| `sources` | string[] | no | Group results by source instead of a flat list |
| `categories` | string[] | no | Category bias (e.g. `["general","news"]`) |
| `scrapeOptions` | object | no | Scrape each result page (e.g. `{"formats": ["markdown"]}`) |

> **Response shape depends on mode.** A self-hosted engine (embedded mode or proxying to your own `crw-server`) nests search results under `data.results`. The hosted API at `api.fastcrw.com` puts them directly in `data`. If you write your own client instead of using the official SDKs (which handle both shapes), check for `data.results` before falling back to `data`. See [response shapes](/response-shapes).

### crw_parse_file

Parse a local PDF supplied as a base64-encoded string. No OCR — works on text-layer PDFs.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `contentBase64` | string | **yes** | Base64-encoded PDF content |
| `filename` | string | no | Original filename (used for format hints) |
| `formats` | string[] | no | Output formats (e.g. `["markdown"]`) |
| `jsonSchema` | object | no | JSON schema for structured extraction |
| `parsers` | string[] | no | Parser hints to use |
| `maxLength` | integer | no | Max chars for content fields; `0` = unlimited (default: ~15 000) |

## Example Agent Tool Flow

A clean MCP setup often assigns each CRW route a narrow purpose:

- `search` for web discovery when you don't know the URL (needs a search-enabled server),
- `map` for site-specific URL discovery,
- `scrape` for single-page extraction,
- `crawl` for bounded recursive work.

That keeps tool selection obvious for the host agent. If you expose one broad "web tool" instead, agents tend to overuse it and produce noisier traces.

A common workflow:

1. The agent identifies a site or page it needs.
2. It calls an MCP-exposed CRW tool.
3. CRW returns scrape, map, or crawl output.
4. The agent decides whether to continue exploring or move into summarization, ranking, or retrieval.

## When MCP Is Better Than Direct HTTP

Choose MCP when the host environment already expects tool discovery through a shared protocol, especially in local agent runtimes or IDE workflows. Choose direct HTTP when your application already owns orchestration and just needs API access from the backend.

In other words, MCP is ideal when the caller is an agent platform. Direct HTTP is often simpler when the caller is your own service code.

## Detailed host setup

Use [MCP Client Setup](#mcp-clients) for:

- Claude Code one-liners and HTTP transport
- Codex CLI one-liners and `~/.codex/config.toml`
- Claude Desktop config paths
- Cursor, Windsurf, and Cline JSON examples
- Continue YAML examples
- local embedded and fastcrw.com cloud config variants side by side

## Verify Installation

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"test"},"protocolVersion":"2025-06-18"}}' \
  | crw-mcp 2>/dev/null
```

Expected:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "protocolVersion": "2025-06-18",
    "capabilities": {"tools": {}},
    "serverInfo": {"name": "crw-mcp", "version": "<current>"}
  }
}
```

## How It Works

**Embedded mode (default):**

```
AI Assistant → stdin (JSON-RPC 2.0) → crw-mcp [scraping engine] → Web pages
```

**Proxy mode:**

```
AI Assistant → stdin (JSON-RPC 2.0) → crw-mcp → HTTP → crw-server → Web pages
```

**HTTP transport:**

```
AI Assistant → HTTP POST (JSON-RPC 2.0) → crw-server /mcp → Web pages
```

In embedded mode, the scraping engine runs in-process with zero overhead. In proxy mode, tool calls are forwarded over HTTP. The HTTP transport calls `crw-server` functions directly.

Protocol version: `2025-06-18`

Every tool call returns its result as a text content block, and tools with a structured result also return it as a spec-compliant `structuredContent` object (MCP 2025-06-18). The two are always the same value, so lenient clients can keep reading the text block unchanged.

An `outputSchema` is advertised by the two surfaces that produce the body themselves — **embedded stdio** and the engine's own **HTTP `/mcp`** — because there the shape is guaranteed. The **stdio proxy** (`crw-mcp --api-url …`) advertises none: as noted under `crw_search` above, `--api-url` can point at either response shape, and under MCP 2025-06-18 a declared schema is a promise the server MUST keep — on a mismatch, a strict client rejects the entire call. `structuredContent` is still emitted in proxy mode; there is simply no advertised schema to validate it against. See [response shapes](/response-shapes) for both bodies.

## Operational Notes

- Keep MCP tool descriptions tight so the agent knows when to use `map` versus `scrape`.
- Start with read-only scraping tools before exposing anything more complex in the same MCP server.
- Log tool usage separately from downstream agent reasoning so debugging stays tractable.

## Common Mistakes

- Registering ambiguous tool descriptions that do not explain when to use `map` versus `scrape`.
- Mixing operational secrets and agent prompts in the same configuration surface.
- Assuming MCP replaces deployment or auth decisions; it only standardizes the tool interface.
