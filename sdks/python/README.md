# crw

Python SDK for [CRW](https://github.com/fastcrw/crw) — the open-source web scraper built for AI agents.

New CRW integrations should use the native `/v1` methods exposed by this SDK. If you are migrating Firecrawl v2 SDK code, use the engine's `/v2` compatibility layer and validate the documented differences.

## Install

```bash
# One-line install (auto-detects OS & arch):
curl -fsSL https://fastcrw.com/install | sh

# npm (zero install):
npx crw-mcp

# Python:
pip install crw

# Cargo:
cargo install crw-mcp

# Docker:
docker run -i ghcr.io/fastcrw/crw crw-mcp
```

## CLI Usage

After installing, you can use `crw-mcp` as an MCP server for any AI coding agent:

```bash
# Start the MCP stdio server
crw-mcp

# Add to Claude Code
claude mcp add crw -- npx crw-mcp
```

MCP client config (works with Cursor, Windsurf, Cline, Claude Desktop, etc.):

```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"]
    }
  }
}
```

## SDK Usage

CRW is **cloud-first**. By default the client uses the managed cloud
(`api.fastcrw.com`) — [sign up for 1000 free credits](https://fastcrw.com/dashboard)
(no payment, no monthly reset; GitHub/Google, ~10s) and set `CRW_API_KEY`.
To self-host the engine locally instead, set `CRW_LOCAL=1` (zero-config, no key).

```python
from crw import CrwClient

# Cloud (default) — reads CRW_API_KEY from the environment:
client = CrwClient()
result = client.scrape("https://example.com")
print(result["markdown"])

# ...or pass the key explicitly:
client = CrwClient(api_key="crw_live_...")

# Self-hosted server:
client = CrwClient(api_url="http://localhost:3000")

# Local zero-config engine (no server, no key): run with CRW_LOCAL=1 in the env.

# Scrape with options:
result = client.scrape("https://example.com", formats=["markdown", "links"])
print(result["markdown"])
print(result["links"])

# Crawl a site:
job = client.crawl("https://example.com", max_depth=2, max_pages=10)
print(job["id"])

# Map all URLs on a site:
urls = client.map("https://example.com")
print(urls)
print(urls.sitemaps)  # sitemap URLs the engine read, [] if the site has none
```

### Search

Works in both modes. In subprocess mode the engine needs a search backend
configured (`[search].searxng_url` or `CRW_SEARCH__SEARXNG_URL`); the managed
cloud has one preconfigured.

```python
from crw import CrwClient

client = CrwClient(api_key="YOUR_KEY")  # cloud (default)

# Basic search
results = client.search("web scraping tools 2026")

# Search with options
results = client.search(
    "AI news",
    limit=10,
    sources=["web", "news"],
    tbs="qdr:w",
)

# Search + scrape content
results = client.search(
    "python tutorials",
    scrape_options={"formats": ["markdown"]},
)
```

> **Note:** If search isn't configured, the engine returns a clear `search_disabled` error.

### Scrape options & structured (LLM) extraction

```python
# Force the renderer, wait for JS, pin a renderer tier:
result = client.scrape("https://example.com", render_js=True, wait_for=1500, renderer="chrome")

# Structured extraction with a JSON Schema (adds the `json` format automatically).
# Requires an LLM provider configured on the engine.
result = client.scrape(
    "https://example.com",
    json_schema={"type": "object", "properties": {"title": {"type": "string"}}},
)
print(result["json"])
```

### Parse a document (PDF → markdown / JSON)

Works in both modes.

```python
# From a path:
doc = client.parse_file("invoice.pdf", formats=["markdown"])
print(doc["markdown"], doc["metadata"]["numPages"])

# From bytes, with structured extraction:
doc = client.parse_file(
    content=pdf_bytes,
    filename="invoice.pdf",
    json_schema={"type": "object", "properties": {"total": {"type": "number"}}},
)
```

### Extract, batch, capabilities, change-tracking (HTTP mode)

These require `api_url` (a running server / cloud):

```python
client = CrwClient(api_key="YOUR_KEY")  # cloud (default)

# Structured LLM extraction across URLs (async job, polled to completion).
# Returns a per-URL results array: [{url, status, data, error, llmUsage}]
results = client.extract(
    ["https://example.com"],
    schema={"type": "object", "properties": {"title": {"type": "string"}}},
)
for r in results:
    if r["status"] == "completed":
        print(r["url"], r["data"])

# Explicit typed lifecycle. start_extract always sends Prefer: respond-async.
accepted = client.start_extract(
    ["https://a.example", "https://b.example"],
    schema={"type": "object", "properties": {"title": {"type": "string"}}},
    basis=True,
)
status = client.get_extract(accepted["id"])
client.cancel_extract(accepted["id"])  # idempotent

# Scrape many URLs in one async batch:
pages = client.batch_scrape(["https://a.com", "https://b.com"], formats=["markdown"])

# Feature-detect the server:
caps = client.capabilities()

# Diff a page against a prior snapshot (stateless):
diff = client.change_tracking_diff(
    current={"markdown": "new content"},
    previous={"markdown": "old content"},
)
```
