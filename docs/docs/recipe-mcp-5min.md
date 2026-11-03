# Give Claude Code Live Web Access in 5 Minutes

This recipe walks from zero to a working Claude Code agent that can search the web and scrape pages on demand. You will run one install command, register the MCP server, then see the exact tool calls Claude makes under the hood.

**Time to complete:** ~5 minutes  
**Prerequisites:** Node.js 18+, Claude Code installed  
**Result:** Claude Code gains `crw_search`, `crw_scrape`, `crw_crawl`, `crw_check_crawl_status`, `crw_map`, `crw_extract`, `crw_check_extract_status`, `crw_cancel_extract`, and `crw_parse_file`

---

## Step 1 — Register the MCP server

### Option A: Local embedded mode (zero setup, no API key)

The MCP binary contains a full scraping engine. No server, no account needed.

```bash
claude mcp add crw -- npx -y crw-mcp
```

Claude Code writes this into your project `.claude/mcp.json` automatically. You are done. Start a new Claude Code session and the tools are available.

> **What you get:** `crw_scrape`, `crw_crawl`, `crw_check_crawl_status`, `crw_map`, `crw_extract`, `crw_check_extract_status`, `crw_cancel_extract`, `crw_parse_file`. `crw_search` requires a search backend — use cloud mode to get it instantly.

### Option B: Cloud mode (all 11 tools, including `crw_search`)

Get a free API key at [fastcrw.com](https://fastcrw.com): 500 credits to start, and up to 500 more from onboarding tasks, no monthly reset.

```bash
claude mcp add crw \
  -e CRW_API_URL=https://api.fastcrw.com \
  -e CRW_API_KEY=crw_live_xxxxxxxxxxxx \
  -- npx -y crw-mcp
```

This registers the same `crw-mcp` binary in proxy mode. Tool calls are forwarded to `api.fastcrw.com`. All 11 tools are advertised, including `crw_search` and the data source tools `crw_find_data` and `crw_get_data`.

**Verify it works:**

```bash
claude mcp list
# crw   npx crw-mcp   stdio
```

---

## Step 2 — Verify the server responds

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"test"},"protocolVersion":"2025-06-18"}}' \
  | npx crw-mcp 2>/dev/null
```

Expected output:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "protocolVersion": "2025-06-18",
    "capabilities": { "tools": {} },
    "serverInfo": { "name": "crw-mcp", "version": "0.16.0" }
  }
}
```

---

## Step 3 — Sample agent conversation

Below is a realistic Claude Code session. The user asks about recent Rust async news. Claude uses `crw_search` to find current results, then `crw_scrape` to read the top article in full. The raw MCP JSON-RPC messages are shown so you can see exactly what Claude sends and receives.

### User prompt

```
What are the latest improvements to Rust's async ecosystem in 2025?
Find current sources, not training data.
```

---

### Turn 1: Claude calls `crw_search`

Claude recognizes it needs live data and calls the search tool first.

**Tool call (JSON-RPC request Claude sends to `crw-mcp`):**

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "crw_search",
    "arguments": {
      "query": "Rust async ecosystem improvements 2025",
      "limit": 5,
      "lang": "en",
      "tbs": "qdr:y"
    }
  }
}
```

**Tool result (JSON-RPC response `crw-mcp` returns):**

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\"success\":true,\"data\":{\"results\":[{\"title\":\"Async Rust in 2025: What's new in Tokio 1.40 and async-std\",\"url\":\"https://blog.rust-lang.org/2025/03/async-update\",\"description\":\"An overview of the improvements shipped in Tokio 1.40, including structured concurrency primitives and reduced poll overhead.\",\"position\":1},{\"title\":\"async-trait is now stable: trait async fn lands in Rust 1.78\",\"url\":\"https://this-week-in-rust.org/blog/2025/01/async-trait-stable\",\"description\":\"RFC 3185 (async fn in traits) is now stable, eliminating the need for the async-trait crate in most cases.\",\"position\":2},{\"title\":\"Rust async generators RFC merged\",\"url\":\"https://github.com/rust-lang/rfcs/pull/3513\",\"description\":\"The async generators RFC was merged in early 2025, enabling yield-based async iteration.\",\"position\":3},{\"title\":\"Tokio 1.40 release notes\",\"url\":\"https://tokio.rs/blog/2025-02-tokio-1-40\",\"description\":\"Full changelog for Tokio 1.40 including task_local improvements, CancellationToken updates, and select! macro fixes.\",\"position\":4},{\"title\":\"Embassy: async embedded Rust matures in 2025\",\"url\":\"https://embassy.dev/blog/2025-q1-update\",\"description\":\"Embassy embedded async framework now supports stable async fn in traits and ships without nightly.\",\"position\":5}]}}"
      }
    ]
  }
}
```

Claude now has 5 live search results with titles, URLs, and descriptions. It picks the most informative result to read in full.

> **Response shape note:** the envelope above is the self-hosted engine's — it nests the results as `data.results`. In cloud mode (`CRW_API_URL=https://api.fastcrw.com`) the hosted REST contract puts them **directly in `data`**, i.e. `{"success":true,"data":[{…}]}`. Read `data.results` when present and fall back to `data`. Because the two differ, `crw-mcp` advertises an `outputSchema` for its tools in embedded mode only; in proxy mode it still returns `structuredContent`, just without a schema to validate against. (The engine's own HTTP `/mcp` advertises them too — it produces the body itself.)

---

### Turn 2: Claude calls `crw_scrape`

**Tool call (JSON-RPC request):**

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "tools/call",
  "params": {
    "name": "crw_scrape",
    "arguments": {
      "url": "https://this-week-in-rust.org/blog/2025/01/async-trait-stable",
      "formats": ["markdown"],
      "onlyMainContent": true
    }
  }
}
```

**Tool result (JSON-RPC response):**

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\"success\":true,\"data\":{\"markdown\":\"# async fn in traits is now stable\\n\\nRust 1.78 stabilizes `async fn` in traits (RFC 3185). You can now write:\\n\\n```rust\\ntrait Fetcher {\\n    async fn fetch(&self, url: &str) -> Result<String, Error>;\\n}\\n```\\n\\nwithout adding the `async-trait` proc-macro crate as a dependency.\\n\\n## What changes\\n\\n- `async fn` in public trait methods works on stable.\\n- Return-position `impl Trait` (RPIT) in traits is also stable.\\n- The `async-trait` crate still works and is useful for object-safe traits (dyn Trait).\\n\\n## Migration\\n\\nRemove `#[async_trait]` attributes and the crate dependency for non-dyn traits. Dynamic dispatch still requires a workaround or the `async-trait` crate.\\n\\n## Credits\\n\\nThis feature was stabilized after 3 years of design work led by @nikomatsakis and the async working group.\",\"metadata\":{\"sourceURL\":\"https://this-week-in-rust.org/blog/2025/01/async-trait-stable\",\"title\":\"async fn in traits is now stable\",\"statusCode\":200,\"renderedWith\":\"http\",\"elapsedMs\":450}}}"
      }
    ]
  }
}
```

> **Response shape note:** In cloud/proxy mode (`CRW_API_URL=https://api.fastcrw.com`), `crw-mcp` forwards the call to the REST API and returns the full envelope `{"success":true,"data":{...}}` verbatim as `content[0].text`. In local embedded mode (`npx crw-mcp` with no API URL resolved, or with `CRW_LOCAL` truthy), the MCP server returns the bare `ScrapeData` shape `{"markdown":"...","metadata":{...}}` directly, without the `success`/`data` wrapper.

---

### Claude's final answer to the user

> Based on live sources fetched right now:
>
> **1. `async fn` in traits is stable (Rust 1.78)**
> RFC 3185 landed — you can write `async fn` directly in trait definitions without the `async-trait` crate. Dynamic dispatch (`dyn Trait`) still needs a workaround.
>
> **2. Tokio 1.40 ships structured concurrency primitives**
> `CancellationToken` updates and reduced poll overhead are the headlining changes in the February release.
>
> **3. Async generators RFC merged**
> Yield-based async iteration via `async gen {}` blocks is coming after the RFC merged in early 2025.
>
> **4. Embassy runs on stable**
> The embedded async framework no longer requires nightly after async fn in traits stabilized.
>
> Sources: blog.rust-lang.org, this-week-in-rust.org, tokio.rs, github.com/rust-lang/rfcs, embassy.dev

---

## Tool reference (MCP)

All 11 tools registered by `crw-mcp` in cloud mode (embedded mode has the first 9):

| Tool | Required params | Returns |
|------|-----------------|---------|
| `crw_search` | `query` | `{ success, data: [{title, url, description, position}] }` (cloud); `{ success, data: { results: [...] } }` (self-hosted engine) |
| `crw_scrape` | `url` | `{ success, data: { markdown, metadata: { sourceURL, title, statusCode, renderedWith } } }` (cloud); `{ markdown, metadata: {...} }` (embedded) |
| `crw_crawl` | `url` | `{ success, id }` — async job ID |
| `crw_check_crawl_status` | `id` | `{ status, data: [...], total, completed }` |
| `crw_map` | `url` | `{ links: ["url1", ...] }` |
| `crw_extract` | `urls` | `{ success, id }` — async job ID |
| `crw_check_extract_status` | `id` | `{ status, results: [{url, status, data, error, llmUsage}] }` |
| `crw_cancel_extract` | `id` | `{ success, id, status, results, creditsUsed, tokensUsed }` |
| `crw_parse_file` | `contentBase64` | `{ success, data: { markdown } }` |
| `crw_find_data` | none (`query`, `category`, `provider` optional) | `{ items: [{provider, capability, requiredOptions, optionalOptions, creditsCost}], total }` |
| `crw_get_data` | `provider`, `capability` | `{ provider, capability, creditsUsed, provenance, data }` |

**Key points:**

- `crw_search` advertises a `country` field in its schema but the backend ignores it — use `lang` for language filtering instead. Use `tbs` for time filtering (`qdr:h|qdr:d|qdr:w|qdr:m|qdr:y`).
- `crw_search` is only available in proxy/cloud mode, or in embedded mode when a search backend is configured.
- Crawl status values: `scraping` | `completed` | `failed`.
- All content fields default to a ~15 000 char limit. Pass `maxLength: 0` to disable truncation.

---

## What's next

- [MCP Server overview](/docs/mcp) — two modes, all CLI flags, transport options
- [MCP Client Setup](/docs/mcp-clients) — Cursor, Windsurf, Cline, Continue, Gemini CLI configs
- [Scraping guide](/docs/scraping) — `formats`, `renderJs`, `onlyMainContent` in depth
- [Crawling guide](/docs/crawling) — `maxDepth`, `maxPages`, poll loop
