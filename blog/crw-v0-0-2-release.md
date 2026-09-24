# CRW v0.0.2: CSS Selectors, Chunking, BM25 Scoring, and Stealth Mode

> CRW v0.0.2 adds CSS/XPath extraction, RAG-ready chunking with BM25 and cosine scoring, stealth mode for bot detection bypass, per-request proxy, and a setup command for JS rendering.

**Published:** 2026-04-23  
**Updated:** 2026-04-23  
**Canonical:** https://fastcrw.com/blog/crw-v0-0-2-release

---

CRW v0.0.2 is the first major feature release since launch. It adds four capabilities that make CRW significantly more useful for RAG pipelines and AI agent workflows: targeted extraction via CSS/XPath selectors, content chunking with relevance scoring, stealth mode for bot detection bypass, and a one-command setup for JavaScript rendering.

## CSS Selector and XPath Extraction

Not every scraping task needs the full page. Product prices live in `.product-price`. API docs live in `#content`. Job listings live in `.job-card`. Before v0.0.2, you'd scrape the entire page and filter client-side. Now you can target exactly what you need at the API level.

### CSS Selectors

```
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer crw_live_YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://docs.example.com/api-reference",
    "formats": ["markdown"],
    "cssSelector": "#main-content"
  }'
```

CRW applies the selector before markdown conversion, so the output is clean and focused — no navigation, no sidebar, just the content you asked for. This is faster than `excludeTags` for pages where you know exactly which element contains the content you want.

### XPath Expressions

```
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer crw_live_YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://store.example.com/product/widget",
    "formats": ["markdown"],
    "xpath": "//div[@class=\"product-details\"]"
  }'
```

XPath gives you more expressive power than CSS selectors — ancestor/sibling traversal, text content matching, attribute predicates. For complex pages with inconsistent markup, XPath is often the only way to reliably target the right element.

Both `cssSelector` and `xpath` return all matching elements concatenated into a single markdown output. If a selector matches nothing, you get an empty markdown string with a warning in the response metadata.

## Chunking for RAG Pipelines

RAG pipelines need content split into chunks that fit embedding model context windows. Before v0.0.2, you'd scrape a page and chunk it yourself. Now CRW handles chunking at the API level with three strategies.

### Topic Chunking

Splits content at heading boundaries (`h1`–`h6`). Each chunk is a self-contained section with its heading preserved. Best for documentation and blog content where headings mark natural topic boundaries.

```
{
  "url": "https://docs.example.com/getting-started",
  "formats": ["markdown"],
  "chunkStrategy": "topic"
}
```

### Sentence Chunking

Splits at sentence boundaries with configurable overlap. Best for long-form prose content where heading structure is sparse or inconsistent.

```
{
  "url": "https://blog.example.com/long-article",
  "formats": ["markdown"],
  "chunkStrategy": "sentence",
  "chunkSize": 500,
  "chunkOverlap": 50
}
```

### Regex Chunking

Splits at custom delimiter patterns. Useful for pages with non-standard separators (horizontal rules, custom markers).

### BM25 and Cosine Relevance Scoring

Once content is chunked, you often don't want all chunks — you want the most relevant ones for your query. CRW v0.0.2 adds two scoring modes:

```
{
  "url": "https://docs.example.com/authentication",
  "formats": ["markdown"],
  "chunkStrategy": "topic",
  "filterMode": "bm25",
  "query": "how to authenticate with API keys",
  "topK": 5
}
```

**BM25** is a term-frequency scoring model that's fast and works well for keyword-style queries. **Cosine** uses TF-IDF vectors for semantic similarity. Both return chunks sorted by relevance score, so you can take the top-K and pass them directly to your embedding pipeline or LLM context.

This means CRW can replace the "scrape → chunk → score → select" pipeline with a single API call. For RAG workflows that process hundreds of pages, eliminating the client-side chunking step is a meaningful simplification.

## Stealth Mode

Some sites detect automated scrapers by inspecting HTTP headers. Default `reqwest` headers are a dead giveaway — missing `Accept-Language`, wrong `Accept-Encoding` order, no `Sec-Fetch-*` headers.

CRW v0.0.2 adds `"stealth": true` to rotate User-Agent strings from a pool of real Chrome, Firefox, and Safari signatures and inject 12 browser-like headers that match what a real browser sends:

```
{
  "url": "https://protected-site.com/data",
  "formats": ["markdown"],
  "stealth": true
}
```

Stealth mode also adds random jitter between requests during crawls — uniform request timing is another common detection signal.

This isn't a full anti-bot solution — it won't beat Cloudflare's JavaScript challenges or DataDome's fingerprinting. But it handles the 80% case: sites that do basic header inspection and block requests that look like scripts.

## Per-Request Proxy

You can now override the global proxy on a per-request basis:

```
{
  "url": "https://geo-restricted.com/content",
  "formats": ["markdown"],
  "proxy": "http://user:pass@us-residential.proxy.com:8080"
}
```

This is useful when you need different exit IPs for different target sites — residential proxies for protected sites, datacenter proxies for public documentation, direct connection for internal sites.

## Markdown Quality Improvements

v0.0.2 switches the markdown converter from a custom implementation to `htmd`, a Rust port of Turndown.js. The result:

- Tables render correctly with proper pipe alignment
- Code blocks include language hints when the source HTML specifies them
- Nested lists (3+ levels) maintain correct indentation
- Headings inside code blocks are no longer misinterpreted as markdown headings

For LLM consumption, the most impactful fix is code block language hints. A fenced block with ````python` gives the LLM explicit context about the code's language, improving code explanation and generation tasks.

## One-Command JS Rendering Setup

```
crw-server setup
```

This downloads LightPanda (the lightweight headless browser) and creates `config.local.toml` with the right paths. Before v0.0.2, setting up JavaScript rendering required manually downloading LightPanda and configuring the CDP endpoint. Now it's one command.

## Upgrade

```
# Docker
docker pull ghcr.io/fastcrw/crw:0.0.2

# Cargo
cargo install crw-server

# Binary
curl -L https://github.com/fastcrw/crw/releases/download/v0.0.2/crw-linux-x86_64 -o crw
```

v0.0.2 is backward-compatible with v0.0.1. All new fields are optional — existing API calls work without changes. See the [documentation](https://docs.fastcrw.com) for full API reference.

## What's Next

v0.0.3+ focuses on rendering reliability and coverage: better success/failure semantics for 4xx pages, improved CDP lifecycle handling, and crawl parameter normalization. Follow the [GitHub repository](https://github.com/fastcrw/crw) for updates.
