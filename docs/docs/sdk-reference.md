# SDK Reference

CRW ships two first-party client libraries that wrap the engine API and the local
subprocess mode behind a single interface. Both share the same constructor contract,
the same mode-selection logic, and the same exception hierarchy.

| Package | Registry | Runtime |
|---|---|---|
| `crw` | PyPI | Python 3.10+ |
| `crw-sdk` | npm | Node 18+ |

---

## Installation

```bash
# Python
pip install crw

# TypeScript / JavaScript (Node 18+)
npm install crw-sdk
```

---

## CrwClient — constructor modes

CRW is **cloud-first**. The client has three modes, selected at construction time.

### Cloud (default)

With no arguments the client points at the managed cloud (`https://api.fastcrw.com`)
and raises immediately if no API key is found.

```python
# Python — reads CRW_API_KEY from the environment
from crw import CrwClient

client = CrwClient()

# …or pass the key explicitly
client = CrwClient(api_key="crw_live_...")
```

```ts
// TypeScript — reads CRW_API_KEY from process.env
import { CrwClient } from "crw-sdk";

const crw = new CrwClient();

// …or pass it explicitly
const crw = new CrwClient({ apiKey: "crw_live_..." });
```

Sign up for **500 free credits, and up to 500 more from onboarding tasks** (no payment card, no monthly reset) at
<https://fastcrw.com/dashboard> and then set `CRW_API_KEY` in your environment.

### Self-hosted HTTP server

Pass your server's base URL. No key is required by default (only when
`auth.api_keys` is configured on the server).

```python
# Python
client = CrwClient(api_url="http://localhost:3000")

# Environment variable alternative
# CRW_API_URL=http://localhost:3000
```

```ts
// TypeScript
const crw = new CrwClient({ apiUrl: "http://localhost:3000" });

// Environment variable alternative: CRW_API_URL=http://localhost:3000
```

### Local binary (CRW_LOCAL=1)

Setting `CRW_LOCAL=1` opts the client into subprocess mode: no server, no API key.
The client spawns the `crw-mcp` binary over stdio.

**Python** auto-downloads the binary from GitHub Releases on first use (cached in
the OS user-cache dir). Resolution order:

1. `CRW_BINARY` env var — explicit path to any `crw-mcp` binary
2. `crw-mcp` native binary already on `PATH` (e.g. installed via `cargo install crw-mcp`)
3. Fetch the latest version tag from the GitHub API — if the API is unreachable
   (offline), fall back to the newest version already cached in `~/.cache/crw/`
   (Linux/macOS) or the OS equivalent; if no cached version exists either, raise
   `CrwBinaryNotFoundError`
4. Check whether that specific latest version is already cached; if so, return it
5. Download the binary from GitHub Releases and store it in the cache

If the binary cannot be found or downloaded, `CrwBinaryNotFoundError` is raised.

**TypeScript** resolves the binary via `CRW_BINARY` env var or `crw-mcp` on `PATH`.
Auto-download is **not** implemented in the TS SDK (v1). If the binary is absent,
a `CrwBinaryNotFoundError` is raised with an install hint.

```bash
# Set in the environment before starting your process
CRW_LOCAL=1 python app.py
CRW_LOCAL=1 node app.js
```

No code changes needed in either SDK — the constructor detects `CRW_LOCAL` and
switches to subprocess mode automatically.

---

## Method overview

| Method | Python | TypeScript | Mode |
|---|---|---|---|
| Scrape one URL | `scrape(url, ...)` | `scrape(url, opts?)` | both |
| Crawl a site | `crawl(url, ...)` | `crawl(url, opts?)` | both |
| Discover URLs | `map(url, ...)` | `map(url, opts?)` | both |
| Web search | `search(query, ...)` | `search(query, opts?)` | both¹ |
| Parse a document | `parse_file(path, ...)` | `parseFile(bytes, opts?)` | both |
| LLM extraction | `extract(urls, ...)` | `extract(opts)` | HTTP only |
| Batch scrape | `batch_scrape(urls, ...)` | `batchScrape(urls, opts?)` | HTTP only |
| Feature-detect | `capabilities()` | `capabilities()` | HTTP only |
| Change diff | `change_tracking_diff(...)` | `changeTrackingDiff(...)` | HTTP only |
| Shutdown | `close()` | `close()` | — |

¹ Local mode requires `[search].search_backend_url` configured on the engine; the
managed cloud has a search backend preconfigured.

---

## Method reference

### scrape

Scrape a single URL and return its content.

```python
# Python
result = client.scrape(
    "https://example.com",
    formats=["markdown", "links"],
    only_main_content=True,    # omit to use the engine default (true)
    render_js=True,            # force JS renderer
    wait_for=1500,             # ms to wait after load
    renderer="chrome",         # pin renderer tier
)
print(result["markdown"])
```

```ts
// TypeScript
const result = await crw.scrape("https://example.com", {
  formats: ["markdown", "links"],
  onlyMainContent: true,   // omit to use the engine default (true)
  renderJs: true,
  waitFor: 1500,
  renderer: "chrome",
});
console.log(result.markdown);
```

Key options (both SDKs):

| Option | Type | Default | Notes |
|---|---|---|---|
| `formats` / `formats` | `string[]` | engine default | `markdown`, `html`, `rawHtml`, `plainText`, `links`, `json`, `summary`, `changeTracking` |
| `only_main_content` / `onlyMainContent` | `bool` | engine default (`true`) | strips nav/boilerplate |
| `render_js` / `renderJs` | `bool` | — | force JS renderer on/off |
| `renderer` | `string` | — | `auto`, `lightpanda`, `chrome`, `chrome_proxy` (self-host may also configure `playwright`) |
| `wait_for` / `waitFor` | `int` | — | milliseconds to wait after page load |
| `json_schema` / `jsonSchema` | `dict` | — | JSON Schema for structured LLM extraction; auto-adds `json` format |

### crawl

Starts an async crawl, polls for completion, and returns all page results.
The SDK handles polling internally.

Both SDKs default and always send `maxDepth: 2` / `maxPages: 10` for a crawl, and always send them explicitly. This is a client-side default, not the server's: a raw HTTP call to `POST /v1/crawl` that omits `maxPages` gets `100` from the server instead — see [Crawling](/docs/crawling#parameters).

```python
# Python
pages = client.crawl(
    "https://example.com",
    max_depth=2,      # default
    max_pages=10,     # default
    poll_interval=2.0,
    timeout=300.0,
)
for page in pages:
    print(page["url"], page["markdown"])
```

```ts
// TypeScript
const pages = await crw.crawl("https://example.com", {
  maxDepth: 2,
  maxPages: 10,
  pollInterval: 2,   // seconds
  timeout: 300,      // seconds
});
for (const page of pages) {
  console.log(page.url, page.markdown);
}
```

### map

Discover all reachable URLs on a site without scraping page content.

```python
# Python
urls = client.map("https://example.com", max_depth=2, use_sitemap=True)
print(urls)            # ["https://example.com/about", ...]
print(urls.sitemaps)   # ["https://example.com/sitemap.xml", ...]
```

```ts
// TypeScript
const urls = await crw.map("https://example.com", { maxDepth: 2, useSitemap: true });
console.log(urls);            // ["https://example.com/about", ...]
console.log(urls.sitemaps);   // ["https://example.com/sitemap.xml", ...]
```

The sitemap URLs the engine read ride on the returned list as `sitemaps`. The
list is empty when the site exposes no sitemap or `useSitemap` is off, and
partial when discovery stops early on `limit`, `timeout` or the internal sitemap
budget; in local (subprocess) mode the MCP layer caps it at the map limit, so a
deep sitemap index reports fewer entries there than over HTTP. Iteration,
indexing, `len` and JSON serialization are unchanged, so existing code keeps
working; in Python
the value is now a `list` subclass, so an exact `type(x) is list` check no longer
matches. Because `sitemaps` is an attribute on the list rather than an element of
it, it does not carry over to a new list built by a transform (slicing,
`sorted()`, spread, `structuredClone`); read it off the returned value directly.

### search

Search the web and optionally fetch page content for each result in the same call.

```python
# Python
results = client.search(
    "web scraping tools 2026",
    limit=5,
    lang="en",
    tbs="qdr:w",              # last week
    sources=["web", "news"],
    categories=["github"],
    scrape_options={"formats": ["markdown"]},
)
```

```ts
// TypeScript
const results = await crw.search("web scraping tools 2026", {
  limit: 5,
  lang: "en",
  tbs: "qdr:w",
  sources: ["web", "news"],
  categories: ["github"],
  scrapeOptions: { formats: ["markdown"] },
});
```

Key options (both SDKs):

| Option | Type | Default | Notes |
|---|---|---|---|
| `limit` / `limit` | `int` | `5` | Maximum results to return (1–20) |
| `lang` / `lang` | `string` | — | Language code for results (e.g. `"en"`, `"tr"`) |
| `tbs` / `tbs` | `string` | — | Time filter: `"qdr:h"`, `"qdr:d"`, `"qdr:w"`, `"qdr:m"`, `"qdr:y"` |
| `sources` / `sources` | `string[]` | — | Result types: `"web"`, `"news"`, `"images"`; groups response when set |
| `categories` / `categories` | `string[]` | — | Category filters: `"github"`, `"research"`, `"pdf"` |
| `scrape_options` / `scrapeOptions` | `dict` | — | Scrape each result URL, e.g. `{"formats": ["markdown"]}` |

### parse_file / parseFile

Parse a PDF into markdown or structured JSON.

**Python and TypeScript differ in their call signature:**

| | Python | TypeScript |
|---|---|---|
| Input | `path=` (str) **or** `content=` (bytes) | `content` (Uint8Array) — first positional argument |
| Filename | inferred from path, or `filename=` | `opts.filename` (default `"document.pdf"`) |

```python
# Python — from a file path
doc = client.parse_file("invoice.pdf", formats=["markdown"])
print(doc["markdown"])
print(doc["metadata"]["numPages"])

# Python — from raw bytes
doc = client.parse_file(
    content=pdf_bytes,
    filename="invoice.pdf",
    json_schema={"type": "object", "properties": {"total": {"type": "number"}}},
)
print(doc["json"])
```

```ts
// TypeScript — always pass bytes (Uint8Array / Buffer)
import { readFileSync } from "node:fs";

const doc = await crw.parseFile(readFileSync("invoice.pdf"), {
  formats: ["markdown"],
});
console.log(doc.markdown, doc.metadata?.numPages);

// With structured extraction
const doc2 = await crw.parseFile(pdfBytes, {
  filename: "invoice.pdf",
  jsonSchema: { type: "object", properties: { total: { type: "number" } } },
});
console.log(doc2.json);
```

Under the hood, HTTP mode sends a `multipart/form-data` POST to `POST /firecrawl/v2/parse`;
local (subprocess) mode base64-encodes the bytes and calls `crw_parse_file` over
JSON-RPC. The same method works in both modes — the SDK handles the encoding difference.

### extract (HTTP mode only)

Start an async LLM-extraction job across one or more URLs, poll to completion, and
return the merged result object. Requires an LLM provider configured on the engine.

```python
# Python
data = client.extract(
    ["https://example.com"],
    schema={"type": "object", "properties": {"title": {"type": "string"}}},
    prompt="Extract the page title",
)
```

```ts
// TypeScript
const data = await crw.extract({
  urls: ["https://example.com"],
  schema: { type: "object", properties: { title: { type: "string" } } },
  prompt: "Extract the page title",
});
```

### batch_scrape / batchScrape (HTTP mode only)

Scrape many URLs as a single async job.

```python
# Python
pages = client.batch_scrape(
    ["https://a.com", "https://b.com"],
    formats=["markdown"],
)
```

```ts
// TypeScript
const pages = await crw.batchScrape(
  ["https://a.com", "https://b.com"],
  { formats: ["markdown"] },
);
```

---

## Exception hierarchy

All exceptions inherit from `CrwError`. Import them directly from the top-level
package (`crw` / `crw-sdk`).

```python
# Python
from crw import CrwError, CrwApiError, CrwBinaryNotFoundError, CrwTimeoutError
```

```ts
// TypeScript
import { CrwError, CrwApiError, CrwBinaryNotFoundError, CrwTimeoutError } from "crw-sdk";
```

| Class | Parent | When raised |
|---|---|---|
| `CrwError` | `Exception` / `Error` | Base class for all CRW errors |
| `CrwApiError` | `CrwError` | Engine returned a non-2xx or `success: false` response |
| `CrwBinaryNotFoundError` | `CrwError` | Local binary cannot be found or downloaded (`CRW_LOCAL=1`) |
| `CrwTimeoutError` | `CrwError` | Crawl / extract / batch job exceeded the `timeout` limit |

`CrwApiError` carries an optional `status_code` (Python) / `statusCode` (TS)
integer with the HTTP status code of the failing response.

---

## Error handling

### Python

```python
from crw import CrwClient, CrwApiError, CrwTimeoutError, CrwBinaryNotFoundError

client = CrwClient()  # raises CrwError immediately if CRW_API_KEY is missing

try:
    result = client.scrape("https://example.com")
except CrwApiError as e:
    # Engine returned an error (4xx / 5xx or success:false)
    print(f"API error {e.status_code}: {e}")
except CrwTimeoutError as e:
    # crawl() / extract() / batch_scrape() timed out
    print(f"Timed out: {e}")
except CrwBinaryNotFoundError as e:
    # CRW_LOCAL=1 mode: binary not found or could not be downloaded
    print(f"Binary missing: {e}")
```

Use `CrwClient` as a context manager to ensure the subprocess shuts down cleanly:

```python
with CrwClient() as client:
    result = client.scrape("https://example.com")
# subprocess is terminated automatically here
```

### TypeScript

```ts
import { CrwClient, CrwApiError, CrwTimeoutError, CrwBinaryNotFoundError } from "crw-sdk";

const crw = new CrwClient(); // throws CrwError synchronously if CRW_API_KEY is missing

try {
  const result = await crw.scrape("https://example.com");
} catch (e) {
  if (e instanceof CrwApiError) {
    console.error(`API error ${e.statusCode}:`, e.message);
  } else if (e instanceof CrwTimeoutError) {
    console.error("Timed out:", e.message);
  } else if (e instanceof CrwBinaryNotFoundError) {
    console.error("Binary not found:", e.message);
  } else {
    throw e; // unexpected
  }
} finally {
  crw.close(); // shut down local subprocess if running
}
```

---

## Environment variables

| Variable | Effect |
|---|---|
| `CRW_API_KEY` | API key for cloud or authenticated self-hosted server |
| `CRW_API_URL` | Override the default cloud URL with a self-hosted server URL |
| `CRW_LOCAL` | `1` (or any truthy string except `0`/`false`/`no`) → subprocess mode (no server, no key required). The spawned `crw-mcp` honours it too, so a configured `client.api_url` cannot quietly turn a local run into a remote one. See [MCP](/docs/mcp). |
| `CRW_BINARY` | Explicit path to the `crw-mcp` binary (skips auto-discovery) |

---

## Mode feature matrix

| Feature | Cloud / HTTP | Local (CRW_LOCAL=1) |
|---|---|---|
| `scrape` | yes | yes |
| `crawl` | yes | yes |
| `map` | yes | yes |
| `search` | yes | yes (needs a search backend configured) |
| `parse_file` / `parseFile` | yes | yes |
| `extract` | yes | no |
| `batch_scrape` / `batchScrape` | yes | no |
| `capabilities` | yes | no |
| `change_tracking_diff` / `changeTrackingDiff` | yes | no |

HTTP-only methods raise `CrwError` (not a subclass) when called in local mode with
a message that explains why and how to switch mode.

---

## What to read next

- [Quick Start](/docs/quick-start) — first successful request in 60 seconds
- [Scraping](/docs/scraping) — full scrape parameter reference
- [PDF Parsing](/docs/pdf-parsing) — parse_file / parseFile detail
- [Output Formats](/docs/output-formats) — all eight format types explained
- [Integrations](/docs/integrations) — LangChain and CrewAI wrappers
- [Self-Hosting](/docs/self-hosting) — run the engine locally or on your own server
