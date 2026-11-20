<div class="page-intro">
  <div class="page-kicker">Core Endpoint</div>
  <h1>Search</h1>
  <p class="page-subtitle">Search the web first, then optionally scrape the results you care about. Works out of the box on self-hosted CRW via the bundled search sidecar — no third-party API key needed. Free, self-hostable alternative to Tavily / Serper / Brave Search.</p>
  <div class="page-capabilities">
    <div class="page-capability"><strong>Best for:</strong> unknown URLs</div>
    <div class="page-capability"><strong>Self-hosted:</strong> bundled search sidecar</div>
    <div class="page-capability"><strong>Hosted:</strong> fastcrw.com (managed)</div>
    <div class="page-capability"><strong>Start with:</strong> search only, then add scraping</div>
  </div>
  <div class="page-actions">
    <a class="page-btn primary" href="https://fastcrw.com/playground" target="_blank" rel="noopener">Try it in the Playground</a>
    <a class="page-btn secondary" href="#scraping">View Scrape</a>
  </div>
</div>

<div class="playground-panel">
  <div class="playground-kicker">Try it in the Playground</div>
  <div class="playground-title">Start with result discovery only</div>
  <div class="playground-copy">Use a small query and <code>limit: 5</code> first. Add <code>scrapeOptions</code> only when you already know you need page content from those search results.</div>
  <div class="playground-actions">
    <a class="page-btn primary" href="https://fastcrw.com/playground" target="_blank" rel="noopener">Open Playground</a>
    <a class="page-btn secondary" href="https://fastcrw.com/register?ref=docs-search-inline" target="_blank" rel="noopener">Get API Key</a>
  </div>
</div>

**Try it without a key:** run this exact request in the <a href="https://fastcrw.com/playground?query=web+scraping+tools&mode=search" target="_blank" rel="noopener">playground</a> (rate-limited preview, no signup).

:::note
**Self-hosted users**: `docker compose up` boots the search sidecar automatically (reachable inside the Compose network as `searxng:8080`). `/v1/search` is live on `http://localhost:3000` with no extra setup. To point at a search backend you already run instead, set `CRW_SEARCH__SEARCH_BACKEND_URL=http://your-host:8080` and remove the `searxng` service from your compose file. To disable search entirely, set `[search].enabled = false` — the route returns a clear `search_disabled` error (HTTP 503). See the [Docker → Search backend](/docker) section for the full setup, the `SEARXNG_BASE_URL` vs `search_backend_url` distinction, and cold-start timing.
:::

## Searching the web with CRW

### /v1/search

```http
POST http://localhost:3000/v1/search          # self-hosted
POST https://api.fastcrw.com/v1/search        # hosted
```

Authentication:

- Self-hosted: no auth by default (add a reverse proxy / API key middleware if you expose it publicly)
- Hosted: send `Authorization: Bearer YOUR_API_KEY`

### Installation

Like the rest of the CRW API, search is HTTP-first. Use cURL or your existing HTTP client.

### Basic usage

Start with this request:

```json
{
  "query": "web scraping tools",
  "limit": 5
}
```

:::tabs
::tab{title="Python"}
```python
import requests

# Self-hosted
resp = requests.post(
    "http://localhost:3000/v1/search",
    json={"query": "web scraping tools", "limit": 5},
)

# Or hosted (with API key)
# resp = requests.post(
#     "https://api.fastcrw.com/v1/search",
#     headers={"Authorization": "Bearer YOUR_API_KEY"},
#     json={"query": "web scraping tools", "limit": 5},
# )

# The results sit directly in `data` on the hosted API; a self-hosted engine
# nests them one level deeper as `data.results`. Read `results` when present,
# fall back to `data` — works on both hosts.
data = resp.json()["data"]
results = data["results"] if isinstance(data, dict) and "results" in data else data

# Each result row has: title, url, snippet, description, position, score.
# `snippet` is the LLM-ready summary line (Firecrawl-compatible name);
# `description` is the same value, kept as an alias.
for item in results:
    print(item["title"], item["url"], item["snippet"])
```
::tab{title="Node.js"}
```javascript
const resp = await fetch("http://localhost:3000/v1/search", {
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify({ query: "web scraping tools", limit: 5 })
});

const body = await resp.json();
// Hosted puts the results directly in `data`; a self-hosted engine nests them
// as `data.results`. Read `results` when present, fall back to `data`.
const { data } = body;
console.log(data?.results ?? data);
```
::tab{title="cURL"}
```bash
# Self-hosted (no auth)
curl -X POST http://localhost:3000/v1/search \
  -H "Content-Type: application/json" \
  -d '{"query": "web scraping tools", "limit": 5}'

# Hosted
curl -X POST https://api.fastcrw.com/v1/search \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"query": "web scraping tools", "limit": 5}'
```
:::

:::note
1000 free credits, no card.
<a href="https://fastcrw.com/register?ref=docs-search-inline" target="_blank" rel="noopener">Get your API key</a>
:::

### Response

```json
{
  "success": true,
  "data": [
    {
      "url": "https://example.com/article",
      "title": "Article Title",
      "snippet": "A summary line from the search result...",
      "description": "A summary line from the search result...",
      "position": 1,
      "score": 9.5
    }
  ]
}
```

`snippet` is the LLM-ready summary line — name kept identical to Firecrawl
so existing pipelines work unchanged. `description` is the same value
under the search-backend-native name. Pick whichever; both are emitted.

That is the flat response shape used when `sources` is not set.

:::note
**Where the results live depends on the host.** The shape of a result row is
identical everywhere, but its position is not. The hosted API
(`api.fastcrw.com`) returns the results **as `data` itself**, exactly as shown
above, and puts `answer`, `citations`, `llmUsage` and `warnings` at the top
level. A self-hosted engine wraps them one level deeper as `data.results`, with
those four fields as siblings inside `data`. Portable clients should read
`data.results` when it is present and fall back to `data`. Full side-by-side in
[Response shapes](/response-shapes).
:::

## Parameters

| Field | Type | Default | Description |
|---|---|---|---|
| `query` | string | required | Search query (1–2000 chars) |
| `limit` | number | `5` | Maximum results per source (max `20`) |
| `lang` | string | -- | Result language hint such as `"en"` or `"tr"` |
| `tbs` | string | -- | Recency filter: `qdr:h`, `qdr:d`, `qdr:w`, `qdr:m`, `qdr:y` |
| `sources` | string[] | -- | Result groups such as `"web"`, `"news"`, `"images"` |
| `categories` | string[] | -- | Curated filters (`"github"`, `"research"`, `"pdf"`) **plus** any native search-backend category (`"science"`, `"it"`, `"news"`, `"files"`, …) passed straight through. Max 5 entries. See [Curated vs. passthrough categories](#curated-vs-passthrough-categories) |
| `scrapeOptions` | object | -- | Scrape each result URL after search |
| `summarizeResults` | boolean | `false` | When `true`, each scraped result is summarized by the LLM and the digest appears in `result.summary`. Needs LLM config (per-request key or server). Fan-out is bounded by `[extraction.llm].max_concurrency`. |
| `answer` | boolean | `false` | When `true`, after scraping the top results crw synthesizes a single answer over them. The answer + `citations` land on the response wrapper. |
| `answerTopN` | number | `5` (max `10`) | Number of top-scoring results to feed into the answer pipeline |
| `maxCharsPerSource` | number | `8192` | Per-source byte cap on markdown fed into the answer prompt. Clamped to 32 KB server-side. |
| `maxContentChars` | number | `[extraction.llm].max_html_bytes` (100 KB) | Per-result byte cap on markdown sent to the per-result summarizer (`summarizeResults`). Clamped to 200 KB server-side. Independent from `maxCharsPerSource`. |
| `summaryPrompt` | string | -- | Style/tone/language directive appended to the per-result summary prompt. Capped at 500 chars. |
| `answerPrompt` | string | -- | Style/tone/language directive appended to the answer-synthesis prompt. Capped at 500 chars. Cannot override the "answer using ONLY provided sources" rule or the citation discipline. |
| `answerTemperature` | number | provider default | Sampling temperature for the answer-synthesis LLM call. Set `0` for deterministic/benchmark runs. |
| `queryExpandVariants` | number | server config | Number of diverse query rewrites fetched and unioned when query expansion is enabled. Overrides `[search].query_expand_variants` for this request. |
| `multiRound` | boolean | server config | When `true`, fires an adaptive evidence-scout round if the first-round answer abstains. Overrides `[search].multi_round` for this request. |
| `answerListFormat` | boolean | server config | When `true` (and the query has list intent such as "best/top X"), renders the answer as a ranked list instead of prose. `false` forces prose. Overrides `[search].answer_list_format`. |
| `llmApiKey` | string | -- | Per-request LLM API key |
| `llmProvider` | string | server default | `anthropic`, `openai`, `openai-responses`, `deepseek`, `azure`, or `openai-compatible` |
| `llmModel` | string | server default | Model override |
| `baseUrl` | string | -- | Provider endpoint base for Azure, Chat Completions-compatible, or Responses-compatible APIs |

`scrapeOptions`:

| Field | Type | Default | Description |
|---|---|---|---|
| `formats` | string[] | `["markdown"]` | Allowed: `markdown`, `html`, `rawHtml`, `links`. `plainText` and `json` (extract) are not supported on `/v1/search` — use `/v1/scrape` for those |
| `onlyMainContent` | boolean | `true` | Keep content focused on the main body |

## Search result types

Without `sources`, CRW returns a flat list:

```json
{
  "success": true,
  "data": [
    {
      "url": "https://example.com/article",
      "title": "Article Title",
      "snippet": "Search summary line...",
      "description": "Search summary line...",
      "position": 1,
      "score": 9.5
    }
  ]
}
```

With `sources`, CRW returns grouped results:

```json
{
  "success": true,
  "data": {
    "web": [{ "url": "...", "title": "...", "snippet": "...", "description": "..." }],
    "news": [{ "url": "...", "title": "...", "snippet": "...", "description": "...", "publishedDate": "2026-04-02T14:00:00" }],
    "images": [{ "url": "...", "imageUrl": "...", "thumbnailUrl": "..." }]
  }
}
```

## Search with content scraping

When you need more than result snippets, add `scrapeOptions`:

```json
{
  "query": "web scraping tools",
  "limit": 3,
  "scrapeOptions": {
    "formats": ["markdown"],
    "onlyMainContent": true
  }
}
```

That enriches eligible results with scraped page content. It is powerful, but it is also the moment search becomes more expensive, so keep it off until you need it.

## LLM-assisted search

CRW can turn a search-with-scrape into either per-result summaries or a single synthesized answer (or both).

### Per-result summaries (`summarizeResults`)

```json
{
  "query": "what is tokio rust",
  "limit": 3,
  "scrapeOptions": { "formats": ["markdown"] },
  "summarizeResults": true,
  "summaryPrompt": "Respond in Turkish in one sentence per result.",
  "maxContentChars": 20000,
  "llmApiKey": "sk-...",
  "llmProvider": "openai",
  "llmModel": "gpt-4o-mini"
}
```

Each scraped result that produced markdown gets a `result.summary` field. Per-result failures attach a `warning` on the response but do not fail the whole request. Fan-out is bounded by `[extraction.llm].max_concurrency` (default 4).

### Synthesized answer (`answer`)

```json
{
  "query": "what is tokio rust",
  "limit": 3,
  "answer": true,
  "answerTopN": 3,
  "answerPrompt": "Respond in Turkish in exactly two sentences.",
  "scrapeOptions": { "formats": ["markdown"] },
  "llmApiKey": "sk-...",
  "llmProvider": "openai",
  "llmModel": "gpt-4o-mini"
}
```

A self-hosted engine carries these inside `data`; the hosted API puts `answer`,
`citations`, `llmUsage` and `warnings` at the top level, next to the results. Fields
with nothing to report are omitted, not emitted as `null` or `[]`. Self-hosted:

```json
{
  "success": true,
  "data": {
    "results": [ /* normal flat or grouped search results */ ],
    "answer": "Tokio is a Rust runtime…",
    "citations": [
      { "url": "https://...", "title": "...", "position": 0 }
    ],
    "llmUsage": { "inputTokens": 3420, "outputTokens": 96, "totalTokens": 3516, "estimatedCostUsd": 0.0008, "model": "gpt-4o-mini", "provider": "openai" },
    "warnings": ["answer synthesis unavailable"]
  }
}
```

Citation discipline:

- `source_id` returned by the model must map to a source actually in the input list. Fabricated ids are dropped.
- `position` is clamped to `[0, sources.len())`.
- The list is deduped on `(source_id, position)` and capped at 20 entries.

If the answer call fails (rate limit, network error, etc.), `answer` is `null`, any successful per-result summaries are still returned, and `warnings` explains what went wrong. CRW does not throw away partial work.

### Caller-supplied directives

`summaryPrompt` and `answerPrompt` let you steer language/tone/format without weakening the safety wrapper:

- They are appended *below* the hardcoded system prompt, not in place of it.
- The wrapper explicitly tells the model to ignore directive contents that try to replace the task (fixed-string outputs, refusals, citation-skip, prompt leaks).
- Each directive is truncated to 500 chars server-side.

### Where the key comes from

Same per-request key pattern as `/v1/scrape`: send `llmApiKey` / `llmProvider` / `llmModel` / `baseUrl` in the request body, or configure `[extraction.llm]` in `config.toml`.

## Freshness, sources, and categories

- Use `tbs` when freshness matters more than broad recall.
- Use `sources` when you want different result groups such as `web`, `news`, or `images`.
- Use `categories` to narrow the query domain without rewriting the query itself.

Good default: add one narrowing control at a time so you can see which one actually improved the results.

### Curated vs. passthrough categories

`categories` accepts two kinds of values, and you can mix them freely (up to 5 entries):

| Value | Kind | What CRW does |
|---|---|---|
| `github` | curated | Switches to the engines in `[search].github_engines` (default: `github`). |
| `research` | curated | Switches to the engines in `[search].research_engines` (default: `arxiv`, `crossref`, `google scholar`, `semantic scholar`). |
| `pdf` | curated | Appends ` filetype:pdf` to the query (not an engine switch). |
| anything else | passthrough | Forwarded verbatim to the search backend's native `categories` parameter — `science`, `it`, `news`, `files`, `images`, `map`, `music`, `social media`, … |

The curated names (`github`/`research`/`pdf`) are **Firecrawl-compatible** and behave exactly as before. Passthrough values are the additive part: CRW does not maintain its own engine list for them — it hands the category string to the search backend, which already knows the engine→category routing from its own `settings.yml`. That means new categories work **without any CRW code or config change**, and your self-hosted backend governs exactly which engines each category hits.

```json
{
  "query": "crispr base editing",
  "categories": ["science"],
  "limit": 5
}
```

```json
{
  "query": "rust async runtime",
  "categories": ["research", "it"]
}
```

In the second example, `research` still drives CRW's curated academic engines while `it` is forwarded to the search backend as a native category — both apply to the same query.

:::note
Which passthrough categories actually return results depends on the engines your search backend enables. The bundled sidecar uses `use_default_settings: true` (see `config/searxng/settings.yml`), so all of the upstream defaults are available. An unknown or disabled category is silently ignored by the backend (it falls back to `general`) rather than erroring. The categories a given instance exposes are governed by that same mounted `settings.yml`.
:::

### Backend query parameters CRW sends

For reference (and when debugging a self-hosted backend directly), this is how the public request fields map onto the `/search` query parameters CRW emits:

| Backend param | Sourced from | Notes |
|---|---|---|
| `q` | `query` | Cleaned (leading filler stripped); `pdf` category appends ` filetype:pdf`. |
| `categories` | `sources` + passthrough `categories` | Comma-joined union, de-duplicated. `web`→`general`, `news`→`news`, `images`→`images`, plus any passthrough value. |
| `engines` | curated `categories` | Comma-joined engines for `github`/`research` (from config). Omitted when no curated category is set. |
| `language` | `lang` | Defaults to `en` when omitted/empty so results aren't locale-mixed. |
| `time_range` | `tbs` | `qdr:h`/`qdr:d`→`day`, `qdr:w`→`week`, `qdr:m`→`month`, `qdr:y`→`year`. |
| `format` | — | Always `json` (the sidecar enables the JSON formatter; HTML stays on for debugging). |

`safesearch` is available on the low-level `crw search` CLI but is not exposed on `/v1/search`.

## Self-hosting the search sidecar

The default `docker-compose.yml` ships a hardened search container:

- Read-only root filesystem with sized tmpfs scratch
- All Linux capabilities dropped, `no-new-privileges`
- `mem_limit`, `pids_limit` set
- Pinned upstream image tag (we never run `:latest`)
- Config mounted read-only from `config/searxng/settings.yml`

It is mere-aggregation under AGPL — you are running an unmodified upstream image with config mounted at runtime, so no §13 corresponding-source obligations attach to the image itself. If you redistribute your CRW deployment publicly, AGPL §13 still requires you to offer the corresponding source of CRW (which is already on GitHub) to your users.

## Common production patterns

- Start with search only, then add `scrapeOptions` after you verify result quality.
- Use `sources: ["news"]` or `tbs` when freshness matters more than broad recall.
- Use `categories: ["github"]` or `["research"]` to narrow noisy queries.
- Keep `limit` low on the first pass so the result quality is easy to inspect.

## Common mistakes

- Adding `scrapeOptions` to every search before you know you need page content
- Confusing `sources` with `categories`
- Treating `qdr:h` as truly hourly precision; the backend collapses it to `day`
- Sending `plainText` or `json` in `scrapeOptions.formats` — use `/v1/scrape` for those

## When to use something else

- Use [Scrape](#scraping) when you already know the exact URL
- Use [Map](#map) for site-specific discovery
- Use [Crawl](#crawling) when you need a bounded multi-page job on one site
