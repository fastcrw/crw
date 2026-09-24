# crw-browse: Browser Automation MCP Server

`crw-browse` is a **separate MCP server** (since v0.4.0) that drives a real Chrome-family browser over the Chrome DevTools Protocol (CDP) for stateful, multi-step interaction. It complements `crw-mcp`'s one-shot scraping tools with an interactive session that persists state across tool calls.

## When to use crw-browse

`crw_scrape` (from `crw-mcp`) is a one-shot tool: navigate, render, return content, done. That covers the vast majority of agent web tasks. Use `crw-browse` when you need something `crw_scrape` cannot do:

| Situation | crw_scrape | crw-browse |
|-----------|-----------|------------|
| Read article / extract text | Yes | Unnecessary |
| JS-gated SPA that returns empty on HTTP request | `renderJs: true` usually works | Use when `renderJs` still returns empty |
| Multi-step flow (login → fill form → submit) | No — one-shot only | **Yes** |
| Click a button and observe the result | No | **Yes** |
| Read the DOM or accessibility tree after interaction | No | **Yes** |
| Maintain session state (cookies, auth) across requests | No | **Yes** |
| Take a screenshot of the current state | No | **Yes** (requires Chrome; see below) |

The short rule: if your agent needs to *interact* (click, type, fill) or read the DOM *after* interaction, use `crw-browse`. If it only needs to *read* a URL — even a JS-heavy one — try `crw_scrape` with `renderJs: true` first.

## How it differs from crw-mcp

| | crw-mcp | crw-browse |
|--|---------|------------|
| **Purpose** | Bulk scrape / crawl / search | Stateful browser interaction |
| **Session** | Stateless (each call independent) | Persistent session across calls |
| **State** | None carried between calls | Cookies, auth, DOM state, scroll position all persist |
| **Tools** | `crw_scrape`, `crw_crawl`, `crw_map`, `crw_extract`, `crw_check_extract_status`, `crw_cancel_extract`, `crw_search`, `crw_parse_file`, `crw_check_crawl_status` | `goto`, `tree`, `click`, `fill`, `type_text`, `evaluate`, `text`, `html`, `wait`, `console`, `network`, `storage`, `screenshot`, `script` |
| **Browser required?** | Optional (HTTP-only mode works without CDP) | Required (must have Chrome or Lightpanda running separately) |
| **Cloud option** | fastcrw.com | Self-hosted only |
| **Availability** | All modes (embedded + proxy + HTTP) | Self-hosted binary only |

## Architecture

`crw-browse` builds on `crw-renderer`'s persistent `CdpConnection` primitive — the same CDP machinery the scrape pipeline uses — so it inherits tested session management, event routing, and browser-pool code.

```
Agent (MCP client)
  → stdin JSON-RPC 2.0
  → crw-browse process
  → CDP WebSocket (ws://localhost:9222)
  → Chrome / Lightpanda
```

A `SessionRegistry` holds named `BrowserSession` objects keyed by UUID, with 4-char base62 `short_id` tokens returned to the agent. Session state is automatically swept after an idle TTL.

## Prerequisites: start a CDP browser first

`crw-browse` does **not** launch a browser. You must start one yourself and point `crw-browse` at its CDP endpoint.

```bash
# Chrome/Chromium — macOS
/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome \
  --remote-debugging-port=9222 \
  --headless=new \
  --user-data-dir=/tmp/crw-chrome

# Chrome/Chromium — Linux
google-chrome \
  --remote-debugging-port=9222 \
  --headless=new \
  --user-data-dir=/tmp/crw-chrome

# Lightpanda (faster and lighter — no screenshots)
lightpanda serve --host 127.0.0.1 --port 9222 --block-private-networks
```

## Install

```bash
# From crates.io
cargo install crw-browse

# Or build from the workspace
cargo build -p crw-browse --release
# Binary: target/release/crw-browse
```

Prebuilt binaries are attached to the [v0.4.0 GitHub release](https://github.com/fastcrw/crw/releases/tag/v0.4.0) and later.

## Start crw-browse

```bash
# Default: CDP at ws://localhost:9222
crw-browse

# Explicit endpoint
crw-browse --ws-url ws://localhost:9222

# Lightpanda as interactive browser + separate Chrome for screenshots
crw-browse \
  --ws-url ws://lightpanda:9222 \
  --chrome-ws-url ws://chrome:9223
```

### Environment variables

| Variable | Default | Meaning |
|----------|---------|---------|
| `CRW_BROWSE_WS_URL` | `ws://localhost:9222` | CDP WebSocket endpoint for interactive tools |
| `CRW_BROWSE_PAGE_TIMEOUT_MS` | `30000` | Per-page load timeout (ms) |
| `CRW_BROWSE_CHROME_WS_URL` | unset | Chrome-only fallback for `screenshot`; when unset `crw-browse` reuses `CRW_BROWSE_WS_URL` |
| `CRW_BROWSE_SCREENSHOT_DIR` | unset | Directory for screenshot disk output; inline base64 when unset |
| `RUST_LOG` | unset | Tracing filter, e.g. `crw_browse=debug` |

### `--ws-url` vs `--chrome-ws-url`

- **`--ws-url`** backs every interactive tool (`goto`, `tree`, `click`, `type_text`, `fill`, `wait`, …). It accepts Chrome **or** Lightpanda. Lightpanda is faster and lighter — use it when you do not need screenshots.
- **`--chrome-ws-url`** is a screenshot-only fallback that **must** point at real Chrome/Chromium. Lightpanda's `Page.captureScreenshot` returns a stub, so screenshot calls are routed to a separate Chrome connection when one is configured.

Three common setups:

| Setup | `--ws-url` | `--chrome-ws-url` | Tradeoff |
|-------|-----------|-------------------|----------|
| Lightpanda + Chrome screenshots | `ws://lightpanda:9222` | `ws://chrome:9223` | Fast interactive, working screenshots |
| Chrome only (simplest) | `ws://chrome:9222` | `ws://chrome:9222` | Single browser, screenshots work |
| Lightpanda only (no screenshots) | `ws://lightpanda:9222` | _(unset)_ | Lightest; `screenshot` returns `NOT_IMPLEMENTED` |

Both flags may point at the **same** Chrome instance — there is no harm in doing so.

## Wire into your MCP client

`crw-browse` uses stdio transport, same as `crw-mcp`. The JSON config is the same shape:

### Claude Code

```bash
claude mcp add crw-browse -- crw-browse --ws-url ws://localhost:9222
```

Or via config file (`~/.claude/mcp.json` / `.mcp.json`):

```json
{
  "mcpServers": {
    "crw-browse": {
      "command": "crw-browse",
      "args": ["--ws-url", "ws://localhost:9222"]
    }
  }
}
```

### Claude Desktop

`~/Library/Application Support/Claude/claude_desktop_config.json` (macOS):

```json
{
  "mcpServers": {
    "crw-browse": {
      "command": "crw-browse",
      "args": ["--ws-url", "ws://localhost:9222"]
    }
  }
}
```

## Available tools

| Tool | Description |
|------|-------------|
| `goto` | Navigate to an `http`/`https` URL and wait for `Page.loadEventFired`. Creates a default session on first call. Returns session token, URL, HTTP status, and `elapsed_ms`. |
| `tree` | Snapshot the current page as an indented accessibility tree. Each line is `@e<N> role: name`; `@e<N>` ref tokens are accepted by interaction tools until the next `tree` call. |
| `click` | Click an element by CSS `selector` or `@e<N>` ref from `tree`. Dispatches a synthetic `click` event so framework handlers (React, Vue) fire. |
| `fill` | Set an input's value and dispatch `input` + `change` events. Pass `selector` or `ref`. |
| `type_text` | Type characters into the focused element via `Input.dispatchKeyEvent`. Use `click` first to focus. |
| `wait` | Block until a CSS selector is visible/present, or until `load`/`networkidle`. |
| `evaluate` | Run a JavaScript expression on the current page (`Runtime.evaluate`, `awaitPromise: true`). |
| `text` | Read visible text: `document.body.innerText` or a specific `selector`. Capped at 50 KB. |
| `html` | Read raw HTML: full `outerHTML` or a specific `selector`. Capped at 200 KB. |
| `console` | Drain the session's console-message ring buffer (up to 200 entries). Filter by `level`; set `clear: true` to wipe. |
| `network` | Drain the session's network-event ring buffer (up to 500 entries). Filter by `all`/`failed`/`requests`/`responses`. |
| `storage` | Read or write browser storage: `action` ∈ {`get`, `set`, `clear`}, `kind` ∈ {`cookie`, `local`, `session`}. |
| `screenshot` | Capture the page as PNG/JPEG. Requires `--chrome-ws-url` (or Chrome at `--ws-url`). Pass `path` to write to disk or omit for inline base64. |
| `script` | Run up to 50 tool calls in one request; steps execute sequentially, first error aborts remaining. |

> `goto` only accepts `http` and `https` schemes. Attempts to navigate to `file://`, `javascript:`, `data:`, or internal network addresses are rejected with `INVALID_ARGS`.

## Basic example: login flow

A complete agent flow navigating a login page:

```json
// 1. Navigate to login page
{"method": "tools/call", "params": {"name": "goto", "arguments": {"url": "https://example.com/login"}}}
// → {"ok": true, "session": "a3f1", "url": "https://example.com/login", "data": {"status": 200}}

// 2. Snapshot accessibility tree to find input refs
{"method": "tools/call", "params": {"name": "tree", "arguments": {"max_nodes": 50}}}
// → data.tree = "[1] WebArea: Login\n  [2] textbox: Email\n  [3] textbox: Password\n  [4] button: Sign in\n..."

// 3. Fill the email field by CSS selector
{"method": "tools/call", "params": {"name": "fill", "arguments": {"selector": "input[type=email]", "value": "user@example.com"}}}

// 4. Fill the password field
{"method": "tools/call", "params": {"name": "fill", "arguments": {"selector": "input[type=password]", "value": "secret"}}}

// 5. Click Sign in
{"method": "tools/call", "params": {"name": "click", "arguments": {"selector": "button[type=submit]"}}}

// 6. Wait for the dashboard to appear
{"method": "tools/call", "params": {"name": "wait", "arguments": {"selector": "#dashboard", "condition": "visible"}}}

// 7. Read the authenticated content
{"method": "tools/call", "params": {"name": "text", "arguments": {}}}
```

The same flow using `script` in a single round-trip:

```json
{
  "method": "tools/call",
  "params": {
    "name": "script",
    "arguments": {
      "actions": [
        {"act": "goto", "url": "https://example.com/login"},
        {"act": "fill", "selector": "input[type=email]", "value": "user@example.com"},
        {"act": "fill", "selector": "input[type=password]", "value": "secret"},
        {"act": "click", "selector": "button[type=submit]"},
        {"act": "wait", "selector": "#dashboard", "condition": "visible"},
        {"act": "text"}
      ]
    }
  }
}
```

## Verify installation

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"test"},"protocolVersion":"2024-11-05"}}' \
  | crw-browse 2>/dev/null
```

Expected:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "protocolVersion": "2024-11-05",
    "capabilities": {"tools": {}},
    "serverInfo": {"name": "crw-browse", "version": "<current>"},
    "instructions": "Interactive browser automation over CDP. Call `goto` to navigate, then `tree` to inspect the rendered accessibility tree."
  }
}
```

## Current limitations

- **Single default session per process.** If two MCP clients connect to the same `crw-browse` process, they share browser state. Multi-session isolation (via `session.new` / `session.close`) is on the roadmap.
- **No cloud option.** `crw-browse` is self-hosted only — you manage the browser process.
- **Screenshots require Chrome.** Lightpanda returns a stub from `Page.captureScreenshot`; `crw-browse` routes screenshot calls to a separate Chrome connection (`--chrome-ws-url`) when configured.

## See also

- [MCP Server](/docs/mcp) — `crw-mcp` and its 8 scraping tools
- [MCP Client Setup](/docs/mcp-clients) — host-by-host config for Claude Code, Cursor, Windsurf, Cline, Continue
- [JS Rendering](/docs/js-rendering) — when `crw_scrape` with `renderJs: true` is enough
