# CRW v0.0.10: Rate Limiting, Crawl Cancel, and Machine-Readable Error Codes

> CRW v0.0.10 adds configurable rate limiting, a crawl cancel endpoint, machine-readable error codes on every error response, fenced code blocks, and cleaner markdown output for RAG pipelines.

**Published:** 2026-04-26  
**Updated:** 2026-04-26  
**Canonical:** https://fastcrw.com/blog/crw-v0-0-10-release

---

CRW v0.0.10 focuses on API hardening and markdown output quality. If you're running CRW in production — especially behind a multi-tenant API or as part of an automated pipeline — this release makes CRW more predictable, more controllable, and easier to integrate.

## API Rate Limiting

Production APIs need rate limits. Without them, a single runaway client can saturate your server's outbound bandwidth and degrade service for everyone else. CRW v0.0.10 adds a configurable token-bucket rate limiter.

```
# config.toml
rate_limit_rps = 10  # requests per second per client
```

When a client exceeds the limit, CRW returns a 429 response with structured error details:

```
{
  "success": false,
  "error": "Rate limit exceeded. Try again in 100ms.",
  "error_code": "rate_limited"
}
```

The default is 10 requests per second, which is generous for most scraping workflows. For high-throughput pipelines, increase it. For shared multi-tenant deployments, decrease it to prevent any single user from monopolizing the scraper.

The rate limiter uses a token-bucket algorithm — it allows short bursts above the limit (up to the bucket size) while enforcing the average rate over time. This means a client that sends 15 requests at once will get the first 10 through immediately and the remaining 5 after tokens refill.

## Crawl Cancel Endpoint

Long-running crawl jobs can go wrong: the target site starts responding slowly, the scope is broader than expected, or the job is simply no longer needed. Before v0.0.10, your only option was to wait for the crawl to finish or restart CRW.

v0.0.10 adds `DELETE /v1/crawl/{id}`:

```
curl -X DELETE https://api.fastcrw.com/v1/crawl/abc123 \
  -H "Authorization: Bearer crw_live_YOUR_API_KEY"
```

```
{
  "success": true
}
```

The cancel is immediate — it triggers an `AbortHandle` that stops all pending page fetches for that crawl job. Pages already fetched are preserved in the results; only pending fetches are cancelled. You can still retrieve partial results via `GET /v1/crawl/{id}` after cancellation.

## Machine-Readable Error Codes

Every error response now includes an `error_code` field with a stable, machine-readable identifier:

```
// Invalid URL
{ "error_code": "invalid_url", "error": "URL scheme must be http or https" }

// Target returned 403
{ "error_code": "http_error", "error": "Target returned 403 Forbidden" }

// Rate limited
{ "error_code": "rate_limited", "error": "Rate limit exceeded" }

// Crawl job not found
{ "error_code": "not_found", "error": "No crawl job with id 'abc123'" }

// Wrong HTTP method
{ "error_code": "method_not_allowed", "error": "POST required" }
```

This is a small change with outsized impact on API consumers. Instead of parsing error message strings (which can change between versions), clients can switch on `error_code` to handle specific error cases programmatically. Monitoring dashboards can aggregate errors by code. Retry logic can distinguish transient errors (`rate_limited`, `http_error`) from permanent ones (`invalid_url`).

All routes now also have proper 405 responses — sending a GET to a POST-only endpoint returns structured JSON instead of an empty body.

## Markdown Quality Improvements

### Fenced Code Blocks

HTML uses two conventions for code blocks: fenced (triple backtick) and indented (4 spaces). LLMs handle fenced blocks much better — they can identify the language, the block boundaries are unambiguous, and they match how developers write markdown. CRW v0.0.10 post-processes all indented code blocks into fenced blocks:

Before:

```
    def hello():
        print("world")
```

After:

```
def hello():
    print("world")
```

This is especially important for documentation sites that use static site generators outputting indented code blocks.

### Anchor Link Cleanup

Many documentation sites add pilcrow signs and empty anchor links next to headings for linking purposes. These show up as noise in markdown output:

```
## Authentication [](#authentication) [&para;](#authentication)
```

v0.0.10 strips these anchor markers and section signs, producing clean heading text.

### ARIA Role Cleanup

Elements with ARIA roles `contentinfo`, `navigation`, `banner`, and `complementary` are now removed during cleaning. These are semantic accessibility markers that identify non-content regions — they're the ARIA equivalent of `` and ``.

### Tiny Chunk Merging

Topic chunking sometimes produces chunks that contain only a heading with no body text (e.g., a heading followed immediately by another heading). These produce poor embeddings because there's not enough content to capture semantic meaning. v0.0.10 merges heading-only chunks (under 50 characters) with the next chunk, ensuring every chunk has enough content for meaningful embedding.

## Other Changes

- **Map response envelope** — `/v1/map` now returns `{ success, data: { links } }` instead of `{ success, links }`, matching the response format of other endpoints
- **`renderedWith: "http"`** — HTTP-only fetches now report `rendered_with: "http"` in metadata instead of `null`
- **Sphinx footer cleanup** — `footer` added as an exact-token noise pattern, catching ` ` in Sphinx documentation sites

## Upgrade

```
# Docker
docker pull ghcr.io/fastcrw/crw:0.0.10

# Cargo
cargo install crw-server
```

Backward-compatible. All new behaviors (rate limiting, error codes) are additive — existing integrations work without changes. Rate limiting defaults to 10 rps; set `rate_limit_rps = 0` to disable.
