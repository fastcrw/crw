# SDK Examples

## Official Packages

| Package | Language | Version | Install |
|---|---|---|---|
| [`crw`](https://pypi.org/project/crw/) | Python ≥ 3.10 | 0.16.0 | `pip install crw` |
| [`crw-sdk`](https://www.npmjs.com/package/crw-sdk) | TypeScript / Node ≥ 18 | 0.16.0 | `npm install crw-sdk` |

Both SDKs are cloud-first: with no arguments they talk to `https://api.fastcrw.com` using the `CRW_API_KEY` environment variable. Set `CRW_LOCAL=1` to run the embedded engine subprocess locally with no API key.

---

## Scrape

### Python

```python
import os
from crw import CrwClient

client = CrwClient(api_key=os.environ["CRW_API_KEY"])
result = client.scrape("https://example.com", formats=["markdown"])
print(result["markdown"])
```

### TypeScript

```ts
import { CrwClient } from "crw-sdk";

const client = new CrwClient({ apiKey: process.env.CRW_API_KEY });
const result = await client.scrape("https://example.com", { formats: ["markdown"] });
console.log(result.markdown);
```

### curl (fallback)

```bash
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com","formats":["markdown"]}'
```

### Scrape with JS rendering

```python
result = client.scrape(
    "https://example.com",
    formats=["markdown", "html"],
    render_js=True,
    wait_for=1500,  # ms
)
```

```ts
const result = await client.scrape("https://example.com", {
  formats: ["markdown", "html"],
  renderJs: true,
  waitFor: 1500,
});
```

---

## Crawl

`crawl()` starts an async job, polls for completion, and returns all page results as a list.

### Python

```python
pages = client.crawl(
    "https://example.com/docs",
    max_depth=3,
    max_pages=50,
)

for page in pages:
    print(page.get("url"), page.get("markdown", "")[:80])
```

### TypeScript

```ts
const pages = await client.crawl("https://example.com/docs", {
  maxDepth: 3,
  maxPages: 50,
});

for (const page of pages) {
  console.log(page.url, String(page.markdown ?? "").slice(0, 80));
}
```

### curl (start + poll)

```bash
# 1. Start the crawl
CRAWL=$(curl -s -X POST https://api.fastcrw.com/v1/crawl \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com/docs","maxDepth":3,"maxPages":50}')

ID=$(echo $CRAWL | jq -r '.id')

# 2. Poll until status is "completed" or "failed"
curl -s https://api.fastcrw.com/v1/crawl/$ID \
  -H "Authorization: Bearer $CRW_API_KEY" | jq '.status'
```

Crawl status values: `scraping` → `completed` | `failed`.

---

## Search

### Python

```python
results = client.search("web scraping tools 2026", limit=5)
for r in results:
    print(r.get("url"), r.get("title"))
```

### TypeScript

```ts
const results = await client.search("web scraping tools 2026", { limit: 5 });
for (const r of results as Record<string, unknown>[]) {
  console.log(r.url, r.title);
}
```

### Search + scrape results in one call

Pass `scrapeOptions` to fetch each result's page content in the same request:

```python
results = client.search(
    "machine learning papers 2026",
    limit=3,
    scrape_options={"formats": ["markdown"]},
)
```

```ts
const results = await client.search("machine learning papers 2026", {
  limit: 3,
  scrapeOptions: { formats: ["markdown"] },
});
```

### curl (fallback)

```bash
curl -X POST https://api.fastcrw.com/v1/search \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"query":"web scraping tools 2026","limit":5}'
```

---

## Map

Discover all URLs on a site without downloading page content.

### Python

```python
urls = client.map("https://example.com", max_depth=2, use_sitemap=True)
print(f"Found {len(urls)} URLs across {len(urls.sitemaps)} sitemaps")
```

### TypeScript

```ts
const urls = await client.map("https://example.com", { maxDepth: 2, useSitemap: true });
console.log(`Found ${urls.length} URLs across ${(urls.sitemaps ?? []).length} sitemaps`);
```

### curl (fallback)

```bash
curl -X POST https://api.fastcrw.com/v1/map \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com","maxDepth":2,"useSitemap":true}'
```

---

## Structured Extraction

Pass a `json_schema` / `jsonSchema` to scrape and return structured JSON in one call. Requires an LLM provider configured on the engine.

### Python

```python
schema = {
    "type": "object",
    "properties": {
        "title": {"type": "string"},
        "price": {"type": "number"},
    },
    "required": ["title"],
}

result = client.scrape(
    "https://example.com/product/123",
    json_schema=schema,
)
print(result.get("json"))
```

### TypeScript

```ts
const schema = {
  type: "object",
  properties: {
    title: { type: "string" },
    price: { type: "number" },
  },
  required: ["title"],
};

const result = await client.scrape("https://example.com/product/123", {
  jsonSchema: schema,
});
console.log(result.json);
```

---

## Parse File (PDF)

### Python

```python
result = client.parse_file("report.pdf", formats=["markdown"])
print(result.get("markdown"))
```

### TypeScript

```ts
import { readFileSync } from "fs";

const pdf = new Uint8Array(readFileSync("report.pdf"));
const result = await client.parseFile(pdf, { filename: "report.pdf", formats: ["markdown"] });
console.log(result.markdown);
```

### curl (fallback)

```bash
curl -X POST https://api.fastcrw.com/firecrawl/v2/parse \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -F "file=@report.pdf" \
  -F 'options={"formats":["markdown"]}'
```

---

## Advanced: Extract (multi-URL LLM extraction)

`extract()` / `batchScrape()` are HTTP-mode-only (cloud or self-hosted server).

```python
data = client.extract(
    urls=["https://example.com/about", "https://example.com/team"],
    prompt="List every person's name and role.",
    schema={"type": "array", "items": {"type": "object", "properties": {"name": {"type": "string"}, "role": {"type": "string"}}}},
)
print(data)
```

```ts
const data = await client.extract({
  urls: ["https://example.com/about", "https://example.com/team"],
  prompt: "List every person's name and role.",
});
console.log(data);
```

---

## When to Use Raw HTTP

Raw `curl` / `fetch` is a fine fallback when:

- your service already owns retry, auth, and tracing middleware,
- you're in an environment where adding a package is impractical (shell scripts, serverless cold-path, etc.), or
- you want maximum transparency into the exact wire shape.

For anything beyond a single call — crawl polling, retries, LLM extraction jobs — the SDKs remove significant boilerplate.

---

## Production Checklist

- Load `CRW_API_KEY` from an environment variable, never hardcode.
- Respect `Retry-After` on `429` responses, and never retry a `402` (out of credits or a plan gate: read `errorCode`).
- Log `warning` and `metadata.statusCode` in addition to the HTTP status.
- Validate a single page through `scrape()` before wiring in `crawl()` or extraction.
- Use the `with` statement (Python) or call `client.close()` (TypeScript) to release the subprocess when running in local mode.

---

## What To Read Next

- [Quick Start](/docs/quick-start) — get your first result in 60 seconds.
- [Output Formats](/docs/output-formats) — choose between markdown, html, and structured JSON.
- [Rate Limits](/docs/rate-limits) — before adding parallel workers.
- [Error Codes](/docs/error-codes) — machine-readable failure handling.
