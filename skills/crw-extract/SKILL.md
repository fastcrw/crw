---
name: crw-extract
description: |
  Extract a typed JSON object from one or more web pages against a JSON Schema
  with fastCRW. Use when you need structured data — "get the price and stock
  status", "extract all job listings as JSON", "pull structured fields from this
  page". Step 6 of the crw workflow ladder.
license: AGPL-3.0
metadata:
  author: us
  version: "0.3.0"
  homepage: https://fastcrw.com
  repository: https://github.com/fastcrw/crw
allowed-tools: Bash(crw:*) Bash(curl:*) Read
---

# crw-extract — typed JSON from pages

## When to use

- You need a **structured JSON object** from a page, not prose.
- Step 6 in the [crw ladder](../crw/SKILL.md). First scrape succeeds (step 2),
  but you need machine-readable fields. If the source is a local PDF, use
  [crw-parse](../crw-parse/SKILL.md) (step 5) with `formats:["json"]` instead.
- Schema-driven = deterministic output shape. No schema = exploratory; use a
  `prompt` to describe what you want.

## How extraction works in crw

**crw has no `/v1/extract` endpoint** (Firecrawl's dedicated extract API). Instead,
extraction runs in two ways, both backed by the same LLM pipeline:

| Path | When to use | Sync? |
|------|------------|-------|
| Per-page: `formats:["json"]` + `jsonSchema` on scrape | Single URL, or inline during a crawl | Synchronous |
| Async multi-URL: `POST /v2/extract` → poll `GET /v2/extract/{id}` | Many URLs, fire-and-forget | Async |

`/v2/extract` is marked deprecated in the server (it recommends `/v2/scrape`
with `formats:["json"]`), but it works and is useful for multi-URL batches.

**Requires a server-side LLM.** Set `[extraction.llm]` in the server config
(provider, api_key, model). Without it, requests return an error (HTTP 4xx) —
e.g. 422 "no LLM configured". Use `crw setup` to configure the LLM for the CLI.

## Quick start

**CLI** — per-page extraction via `--extract`:

```bash
# Inline schema
crw scrape "https://example.com/product" \
  --extract '{"type":"object","properties":{"price":{"type":"number"},"inStock":{"type":"boolean"}}}'

# Schema from file
crw scrape "https://example.com/job" --extract @schema.json -o result.json
```

**MCP** — pass `formats:["json"]` with `jsonSchema` on a scrape:

```
crw_scrape(
  url="https://example.com/product",
  formats=["json"],
  extract={"schema": {"type":"object","properties":{"price":{"type":"number"}}}}
)
```

Note: the MCP `crw_scrape` accepts `extract.schema` (Firecrawl style). The
REST API also accepts `jsonSchema` as a top-level alias.

**REST** — per-page (synchronous):

```bash
curl -X POST "$CRW_API_URL/v1/scrape" \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://example.com/product",
    "formats": ["json"],
    "jsonSchema": {
      "type": "object",
      "properties": {
        "price": {"type": "number"},
        "inStock": {"type": "boolean"}
      }
    }
  }'
```

**REST** — async multi-URL (deprecated endpoint, still functional):

```bash
# Start job
curl -X POST "$CRW_API_URL/v2/extract" \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "urls": ["https://example.com/p1", "https://example.com/p2"],
    "schema": {"type":"object","properties":{"price":{"type":"number"}}}
  }'
# → {"success":true,"id":"<uuid>","warnings":["...use /v2/scrape..."], ...}

# Poll until completed
curl "$CRW_API_URL/v2/extract/<uuid>" -H "Authorization: Bearer $CRW_API_KEY"
# → {"success":true,"status":"completed|scraping|failed","data":{...}}
```

## Options

| Need | CLI | MCP / REST |
|------|-----|------------|
| JSON schema | `--extract '<schema>'` or `@file.json` | `jsonSchema` / `extract.schema` |
| Free-text prompt (no schema) | — | `prompt` on `/v2/extract` |
| Save output | `-o FILE` | write the response yourself |
| Multi-URL async | not available | `POST /v2/extract` with `urls:[...]` |
| LLM override | `--llm-provider`, `--llm-key`, `--llm-model` | server config only |

## Tips

- **Schema = deterministic, prompt = exploratory.** A schema pins the output
  shape; a free-text `prompt` is useful for exploration but less reliable.
  Start with a schema when you know the fields you want.
- **Don't over-specify.** Narrow schemas ("give me exactly these three fields")
  extract more reliably than wide ones with fifty optional fields.
- **Crawl + extract in one pass.** `crw_crawl` accepts a `jsonSchema` parameter
  — each page in the crawl gets extracted against the schema, saving a second
  round-trip.
- **Check `data.json` in the scrape response.** Per-page extraction lands in
  `data.json`, not `data.markdown`. The `markdown` field is also populated for
  reference.

## See also

- [crw-scrape](../crw-scrape/SKILL.md) — scrape a page without schema extraction
- [crw-parse](../crw-parse/SKILL.md) — extract structured data from a local PDF
- [crw-best-practices](../crw-best-practices/SKILL.md) — SDK usage patterns
- [crw](../crw/SKILL.md) — ladder overview
