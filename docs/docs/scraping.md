<div class="page-intro">
  <div class="page-kicker">Core Endpoint</div>
  <h1>Scrape</h1>
  <p class="page-subtitle">Turn any known URL into clean markdown, HTML, links, or structured JSON in one request. This is the default CRW workflow and the fastest path to a first successful integration.</p>
  <div class="page-capabilities">
    <div class="page-capability"><strong>Best for:</strong> one known page</div>
    <div class="page-capability"><strong>Returns:</strong> markdown, HTML, links, JSON</div>
    <div class="page-capability"><strong>Start with:</strong> markdown only</div>
  </div>
  <div class="page-actions">
    <a class="page-btn primary" href="https://fastcrw.com/playground" target="_blank" rel="noopener">Try it in the Playground</a>
    <a class="page-btn secondary" href="#quick-start">Open Quick Start</a>
  </div>
</div>

<div class="playground-panel">
  <div class="playground-kicker">Try it in the Playground</div>
  <div class="playground-title">Validate the happy path first</div>
  <div class="playground-copy">Paste one URL, request <code>formats: ["markdown"]</code>, and confirm you get a clean response back. Add JS rendering, selectors, or extraction only after the plain request looks right.</div>
  <div class="playground-actions">
    <a class="page-btn primary" href="https://fastcrw.com/playground" target="_blank" rel="noopener">Open Playground</a>
    <a class="page-btn secondary" href="#extract">Jump to Extract</a>
  </div>
</div>

**Try it without a key:** run this exact request in the <a href="https://fastcrw.com/playground?url=https%3A%2F%2Fexample.com&mode=scrape" target="_blank" rel="noopener">playground</a> (rate-limited preview, no signup).

## Scraping a URL with CRW

### /v1/scrape

```http
POST /v1/scrape
```

Authentication:

- Hosted: send `Authorization: Bearer YOUR_API_KEY`
- Self-hosted: only required when `auth.api_keys` is configured

### Installation

Official SDKs: `pip install crw` (Python) and `npm install crw-sdk` (TypeScript). Both wrap every endpoint and return plain objects; see the [SDK reference](/sdk-reference). Plain HTTP works too: the [Basic usage](#basic-usage) tabs below show the SDK, `requests`, `fetch`, and cURL.

### Basic usage

Start with this request:

```json
{
  "url": "https://example.com",
  "formats": ["markdown"],
  "onlyMainContent": true,
  "renderJs": null
}
```

:::tabs
::tab{title="Python SDK"}
```python
# pip install crw
from crw import CrwClient

client = CrwClient()  # reads CRW_API_KEY
page = client.scrape("https://example.com", formats=["markdown", "links"])

print(page["markdown"])  # str
print(page["links"])     # list[str] of absolute URLs
```
::tab{title="Python"}
```python
import requests

resp = requests.post(
    "https://api.fastcrw.com/v1/scrape",
    headers={
        "Authorization": "Bearer YOUR_API_KEY",
        "Content-Type": "application/json",
    },
    json={
        "url": "https://example.com",
        "formats": ["markdown"],
        "onlyMainContent": True,
    },
)

print(resp.json()["data"]["markdown"])
```
::tab{title="Node.js"}
```javascript
const resp = await fetch("https://api.fastcrw.com/v1/scrape", {
  method: "POST",
  headers: {
    "Authorization": "Bearer YOUR_API_KEY",
    "Content-Type": "application/json"
  },
  body: JSON.stringify({
    url: "https://example.com",
    formats: ["markdown"],
    onlyMainContent: true
  })
});

const body = await resp.json();
console.log(body.data.markdown);
```
::tab{title="cURL"}
```bash
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://example.com",
    "formats": ["markdown"],
    "onlyMainContent": true
  }'
```
:::

:::note
500 credits to start, up to 500 more from onboarding tasks, no card.
<a href="https://fastcrw.com/register?ref=docs-scraping-inline" target="_blank" rel="noopener">Get your API key</a>
:::

### Response

```json
{
  "success": true,
  "data": {
    "markdown": "# Example Domain\n\nThis domain is for use in illustrative examples...",
    "metadata": {
      "title": "Example Domain",
      "sourceURL": "https://example.com",
      "statusCode": 200,
      "elapsedMs": 32
    }
  }
}
```

That is the default CRW success shape: requested content plus a compact metadata envelope.

## Parameters

| Field | Type | Default | Description |
|---|---|---|---|
| `url` | string | required | URL to scrape |
| `formats` | string[] | `["markdown"]` | `markdown`, `html`, `rawHtml`, `plainText`, `links`, `json`, `summary`, `changeTracking` |
| `onlyMainContent` | boolean | `true` | Remove nav, footer, and boilerplate before conversion |
| `renderJs` | boolean or null | `null` | `null` auto-detects, `true` forces browser rendering, `false` stays HTTP-only |
| `waitFor` | number | -- | Milliseconds to wait after JS rendering |
| `renderer` | string | `auto` | Pin to a specific renderer: `auto`, `lightpanda`, `chrome`, `chrome_proxy`, `camoufox`, or `impersonated-http`. Self-hosted instances can also configure `playwright`; the managed cloud does not run it, and pinning a tier an instance has not configured returns HTTP 400. `GET /v1/capabilities` reports the tiers a given instance actually has. Browser-tier values hard-pin (no fallback) and imply `renderJs:true` unless `renderJs:false` is set explicitly; `impersonated-http` is a JS-less Chrome-TLS-impersonating HTTP tier that never implies `renderJs`. It needs a build with `--features impersonated` (the published Docker image has it; `[renderer.impersonated] enabled = false` turns it off at runtime). Check `GET /v1/capabilities` for the tiers a given instance actually serves. See [JS rendering](#js-rendering) |
| `includeTags` | string[] | `[]` | CSS selectors to keep |
| `excludeTags` | string[] | `[]` | CSS selectors to remove |
| `headers` | object | `{}` | Custom HTTP headers, forwarded on both the HTTP and browser render paths. A `User-Agent` here overrides the default. On a browser render they apply to every request the page makes (subresources included), so avoid putting cross-origin-sensitive credentials like `Authorization` here. |
| `cssSelector` | string | -- | Narrow extraction to one CSS selector |
| `xpath` | string | -- | Narrow extraction to one XPath expression |
| `chunkStrategy` | object | -- | Topic, sentence, or regex chunking |
| `query` | string | -- | Ranking query for chunk filtering |
| `filterMode` | string | -- | `bm25` or `cosine` |
| `topK` | number | `5` | Number of top chunks to keep |
| `proxy` | string | -- | Per-request proxy URL |
| `maxAge` | number | `3600000` | How old a cached copy of this fetch may be, in milliseconds. `0` forces a fresh fetch. Capped at 24 hours. See [Caching](#caching) |
| `storeInCache` | boolean | `true` | `false` reads the cache but stores nothing |
| `country` | string | -- | 2-letter ISO 3166-1 alpha-2 country code (lowercase, e.g. `us`, `gb`, `de`). Routes the request through the named residential pool when the `chrome_proxy` renderer tier is configured. Ignored if no proxy tier is set up. See [JS rendering — Per-request country](#js-rendering) |
| `stealth` | boolean | -- | Override global stealth setting |
| `jsonSchema` | object | -- | Schema for structured extraction |
| `extract` | object | -- | Firecrawl-compatible alias wrapper for extraction schema |
| `llmApiKey` | string | -- | Per-request LLM API key |
| `llmProvider` | string | server default | `anthropic`, `openai`, `openai-responses`, `deepseek`, `azure`, or `openai-compatible` |
| `llmModel` | string | server default | Model override (extraction and summary) |
| `baseUrl` | string | -- | Provider endpoint base. CRW appends `/chat/completions` for Chat Completions providers or `/responses` for `openai-responses`; complete endpoint URLs are accepted. |
| `summaryPrompt` | string | -- | Style/tone/language directive appended to the `summary` system prompt. Safety wrapper kept intact. Capped at 500 chars. |
| `maxContentChars` | number | `[extraction.llm].max_html_bytes` (100 KB) | Per-request byte cap on content sent to the LLM for `summary`. Clamped to 200 KB server-side. |
| `deadlineMs` | number | `8000` | End-to-end request deadline in milliseconds. Must be in `(0, 60000]`. Requests above 8 000 ms land in a separate slow-path histogram and are excluded from the standard SLO p95 metric. |
| `debug` | boolean | `false` | When `true`, includes a `debugExtraction` field in the response with a trace of every extraction candidate considered and why one was selected. |
| `parsers` | object[] | PDF auto-parsed | Firecrawl-compatible document parser directives. Omit for the default (PDFs auto-converted to markdown). Pass `[]` to disable PDF parsing. Pass `[{"type":"pdf","maxPages":10}]` to cap pages. Accepted fields: `type` (`"pdf"`), `mode` (`"auto"` \| `"fast"` \| `"ocr"`), `maxPages`. |
| `actions` | any | -- | Rejected with a clear error; use `cssSelector` or `xpath` instead |
| `mode` | string | `standard` | Cloud only. `standard`, `auto` or `max`. `auto` and `max` get through anti-bot walls and are billed by the traffic the page used. See [Auto and max mode](#auto-and-max-mode) |
| `maxCredits` | number | -- | Cloud only, with `mode: "auto"` or `"max"`. The most this page may cost in credits, from 2 to 1000. A page that would cost more is stopped and not charged |

## Caching

A repeat of the same fetch is served from cache for one hour by default. What
is cached is the FETCH, not the answer, so what counts as identical is what
would change the page the server gets back: the URL, `renderer`, `renderJs`,
`waitFor`, `country`, and any headers you send. Everything you do with the page
afterwards is not part of it, so a second pass with different `formats`,
`onlyMainContent`, selectors or a different extraction schema reuses the fetch
instead of paying for it again.

Caches are per account, never shared between customers.

Requests that carry anything caller-specific are never cached at all: your own
headers, your own proxy, a stealth override, or a screenshot.

```json
{ "url": "https://example.com", "formats": ["markdown"], "maxAge": 0 }
```

Use `maxAge` to choose your own trade-off:

| You are scraping | Suggested `maxAge` |
|---|---|
| Live prices, flight positions, availability, anything that moves by the minute | `0` |
| News and feeds | a few minutes, e.g. `300000` |
| Documentation, reference pages, filings, archived records | hours, up to the 24h ceiling |

`0` disables the cache for that call and always fetches. The ceiling is 24 hours,
so a page can never be pinned for longer than a day.

A response served from cache says so: `cached: true` on `/v1`, and
`metadata.cacheState: "hit"` on the Firecrawl-compatible surface.

Two things are never cached, whatever you pass:

- **`changeTracking`.** It compares a live fetch against the `previous` snapshot
  you send, so replaying an earlier answer would hide the very change you asked
  about.
- **Failed scrapes.** A block page or an error is never stored, so a retry always
  gets a real attempt.

A cached response is billed the same as a fresh one — caching buys latency, not
a discount.

## Formats

Use the smallest output shape that solves the job:

- `markdown` is the default and best first request for most pipelines.
- `html` or `rawHtml` is useful when downstream systems need original structure.
- `links` is useful when you want lightweight discovery without page bodies. It returns `data.links` as a flat `string[]` of absolute URLs (no anchor text); when you need each link's text, read it from `markdown`, where links stay inline as `[text](url)`.
- `summary` is the LLM-prose path — needs `llmApiKey` or a server `[extraction.llm]` config. Optional `summaryPrompt` lets the caller pick language/tone without weakening the safety wrapper.
- `json` is the extraction path and should be paired with `jsonSchema`. On the hosted API, `json` and `summary` run a managed LLM and need a paid plan (Hobby or above); the FREE plan returns an error for them, so parse `markdown` or `links` there.

If you ask for multiple formats, only those formats are populated in the response.

## Structured extraction

For a single page, extraction is part of `scrape`: when you want fields instead of prose, request `formats: ["json"]` and provide a schema. (For structured extraction across many URLs in one job, use the native async [`POST /v1/extract`](/docs/extract).)

```json
{
  "url": "https://example.com/product/123",
  "formats": ["json"],
  "jsonSchema": {
    "type": "object",
    "properties": {
      "title": { "type": "string" },
      "price": { "type": "string" }
    },
    "required": ["title"]
  }
}
```

Use [Extract](#extract) for the schema-first version of this flow.

## LLM summary

Add `summary` to `formats` to get a short prose digest of the page in `data.summary`. Token usage and best-effort cost are returned in `data.llmUsage`.

```json
{
  "url": "https://example.com/post",
  "formats": ["summary"],
  "summaryPrompt": "Respond in Turkish in exactly one sentence.",
  "maxContentChars": 20000,
  "llmApiKey": "sk-...",
  "llmProvider": "openai",
  "llmModel": "gpt-4o-mini"
}
```

Notes:

- The caller's `summaryPrompt` is appended *below* the safety wrapper. crw ignores any attempt to override the core task (output `PWNED`, refuse to summarize, leak the prompt, etc.) and still produces a real summary.
- `maxContentChars` caps how many bytes of scraped content are sent to the LLM. The default comes from `[extraction.llm].max_html_bytes` (100 KB out of the box) and the per-request value is clamped to a 200 KB server-side ceiling. Truncation, when it happens, is reported in `data.warnings`.
- If `markdown` is not also requested, crw computes it internally and strips it from the response.

## JS rendering and targeting

Keep the first request simple:

- Leave `renderJs` at `null` until the plain HTTP path clearly fails.
- Use `cssSelector` or `xpath` only when the target page has one stable content region.
- Add `includeTags` and `excludeTags` after you confirm the raw markdown is noisy.

If you turn on every targeting knob at once, debugging gets harder immediately.

## Auto and max mode

:::note
Cloud only (fastcrw.com), Standard plan and up. A self-hosted engine ignores `mode`.
:::

Some pages answer with an anti-bot challenge instead of content. The default `standard` mode reports those as a failed scrape, refunded. `mode` lets you pay to get through:

| `mode` | What it does | Cost |
| --- | --- | --- |
| `standard` (default) | The normal renderer ladder. | 1 credit |
| `auto` | Runs the normal ladder first and switches to the unblocking path only when the page is behind a wall. | 1 credit when the ladder gets the page; otherwise billed by traffic |
| `max` | Starts on the unblocking path and skips the ladder's failed tries. Use it for sites you already know are walled. | Billed by traffic |

"Billed by traffic" means 1 credit plus the network traffic the unblocking path used for this page. The page shows what it cost in `data.creditCost`. In our measurements a walled page on its own cost 20 to 45 credits; further pages on the same site while its clearance is still warm cost about 2.

```json
{
  "url": "https://example.com/walled-page",
  "formats": ["markdown"],
  "mode": "auto",
  "maxCredits": 60
}
```

How the charge works:

- The request holds `maxCredits` (or the per-page limit when you leave it out) while it runs, then keeps only what the page cost and returns the rest within the same request. Your balance has to cover the hold; below it you get `402 insufficient_credits_for_mode` and plain scrapes keep working.
- A page that would cost more than `maxCredits` is stopped and not charged.
- A failed page is not charged, in any mode.

Limits:

- Only `/v1/scrape`. Crawl, batch, map and the `/v2` surface ignore `mode`.
- Not with the LLM formats (`summary`, `json`, the change-tracking judge).
- `max` needs JavaScript rendering: it rejects `renderJs: false`, a pinned `renderer`, a screenshot, and a `timeout` under 35000 ms. Leave `timeout` unset and the page gets 58 seconds. When the unblocking path is busy or the site keeps refusing it, a `max` page continues on the normal ladder, the same way `auto` does.
- At most 4 auto or max pages in flight per account.

The errors these cases return are listed under [Cloud-only error codes](/error-codes#cloud-only-error-codes).

## Common production patterns

- Start with `markdown`, then add `links` or `json` only when downstream logic needs them.
- Validate extraction with markdown first, then add `jsonSchema`.
- Keep browser rendering as a fallback, not the default.
- Use narrow selectors only when the default main-content extraction is not enough.

## Common mistakes

- Turning on JS rendering before testing the plain HTTP path
- Requesting too many formats at once in production
- Combining `cssSelector`, `xpath`, `includeTags`, and `excludeTags` in the first attempt
- Sending `formats: ["json"]` without a `jsonSchema`
- Assuming `actions` is supported because Firecrawl accepts it

## When to use something else

- Use [Search](#search) when you do not know the URL yet
- Use [Map](#map) when you want URL discovery before scraping
- Use [Crawl](#crawling) when you need many pages from one site
- Use [Extract](#extract) when the output must be structured JSON
