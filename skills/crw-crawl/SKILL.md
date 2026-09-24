---
name: crw-crawl
description: |
  Crawl an entire website or section and extract content from every page.
  Use when you need content from many pages under a common URL prefix:
  "crawl the whole site", "get all docs pages", "scrape every blog post",
  "download the full docs for RAG", "extract all pages under /api". Async
  BFS — starts a job and polls for results. Step 4 of the crw workflow ladder.
license: AGPL-3.0
metadata:
  author: us
  version: "0.3.0"
  homepage: https://fastcrw.com
  repository: https://github.com/fastcrw/crw
allowed-tools: Bash(crw:*) Bash(curl:*) Read
---

# crw-crawl — bulk page extraction

## When to use

- You need content from **many pages** under a site or section, not just one.
- Step 4 in the [crw ladder](../crw/SKILL.md): if you only need a handful of
  known URLs, use [crw-scrape](../crw-scrape/SKILL.md) in a loop instead —
  it's simpler and gives you content immediately. Use crawl when the set of
  URLs is unknown or large.
- Always **map first** ([crw-map](../crw-map/SKILL.md)) to estimate page
  count before committing. A misconfigured crawl on a 50 000-page site is
  expensive; a map call is cheap.
- Start conservative: `depth 1, limit 10`. Scale up once you verify scope.

## Quick start

**CLI** (synchronous streaming output):
```bash
crw crawl "https://docs.example.com" -d 2 -l 50         # markdown to stdout
crw crawl "https://docs.example.com/api" -d 1 -l 20 --format json
crw crawl "https://example.com" --js --rate-limit 1.0 --concurrency 3
```

**MCP** (async — returns a job ID, poll for results):
```
# Start the crawl
crw_crawl(url="https://docs.example.com", maxDepth=2, maxPages=50)
→ { "id": "a1b2c3d4-..." }

# Poll until status == "completed"
crw_check_crawl_status(id="a1b2c3d4-...")
→ { "status": "scraping|completed|failed", "data": [...] }
```

**REST** (async — POST to start, GET to poll, DELETE to cancel):
```bash
# Start
curl -X POST "$CRW_API_URL/v1/crawl" -H "Authorization: Bearer $CRW_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://docs.example.com","maxDepth":2,"maxPages":50}'
# → {"id":"a1b2c3d4-..."}

# Poll
curl "$CRW_API_URL/v1/crawl/a1b2c3d4-..." \
  -H "Authorization: Bearer $CRW_API_KEY"
# → {"status":"completed","data":[...]}

# Cancel
curl -X DELETE "$CRW_API_URL/v1/crawl/a1b2c3d4-..." \
  -H "Authorization: Bearer $CRW_API_KEY"
```

## Options

| Need | CLI flag | MCP / REST field |
|------|----------|------------------|
| Max depth | `-d/--depth N` (default 2) | `maxDepth` (default 2) |
| Max pages | `-l/--limit N` (default 10) | `maxPages` |
| Output format | `--format markdown\|json\|html\|rawhtml\|text\|links` | — |
| Structured JSON per page | — | `jsonSchema: {...}` |
| JS rendering | `--js` | `renderJs: true` (null = auto) |
| Wait after load | — | `waitFor: 2000` (ms) |
| Renderer override | — | `renderer: "lightpanda\|chrome\|playwright"` |
| Rate limit | `--rate-limit N` (default 2.0 req/s) | — |
| Concurrency | `--concurrency N` (default 5) | — |
| Per-page timeout | `--timeout MS` (default 30 000) | — |
| Proxy | `--proxy URL` | — |
| Stealth mode | `--stealth` | — |
| Strip nav/footer | (on by default; `--raw` to disable) | — |

## Polling loop (MCP / REST)

The MCP and REST crawl is async. Poll `crw_check_crawl_status` (MCP) or
`GET /v1/crawl/{id}` (REST) every few seconds. The job expires after 1 hour.

```
loop:
  status = crw_check_crawl_status(id=job_id)
  if status.status == "completed":  break
  if status.status == "failed":     raise error
  wait(3s)

pages = status.data   # list of {url, markdown, html, links, metadata, ...}
```

MCP truncates each page's content to ~15 000 chars by default. Pass
`maxLength: 0` to opt out.

## Saving crawl output to local files

Never stream a whole crawl into model context. Write pages to `.crw/` and
read incrementally.

**CLI** (streams pages as they arrive — redirect or tee):
```bash
crw crawl "https://docs.example.com" -d 2 -l 100 \
  --format json > .crw/crawl-raw.jsonl

# One markdown file per page from the JSON lines
grep '^{' .crw/crawl-raw.jsonl | jq -r '"\(.metadata.sourceURL)\n\(.markdown)"' \
  | split - .crw/pages/page-
```

**MCP / REST** (after polling completes):
```bash
# REST: save the full result
curl "$CRW_API_URL/v1/crawl/$JOB_ID" -H "Authorization: Bearer $CRW_API_KEY" \
  | jq -c '.data[]' > .crw/pages.jsonl

# Write one .md per page
jq -r '.markdown' .crw/pages.jsonl | split -l 1 - .crw/pages/page-
```

Then `grep`, `head`, or pass individual files to the model — never the whole
blob.

## Recommended workflow

```
1. crw map  "https://docs.example.com" --format json > .crw/urls.json
             → see how many pages exist (check last line: "Discovered N URLs")

2. crw crawl "https://docs.example.com/api" -d 1 -l 20
             → start narrow, verify output quality

3. Scale up: -l 100, -d 2, or scope to a sub-path if needed

4. Write to .crw/, read with grep/jq
```

## Tips

- **Map first.** `crw map docs.example.com | wc -l` in 3 seconds beats a
  cancelled 10-minute crawl.
- **Start at depth 1, limit 10.** Confirm you're in the right section before
  widening scope. Most docs sets are fully reachable at depth 2-3.
- **JS auto-detects.** crw's renderer fallback handles most SPAs without
  `--js`. Add it only if you see blank pages or loading skeletons.
- **Rate-limit aggressively for production sites.** Default 2 req/s is
  polite; drop to 0.5 on fragile targets. `--concurrency 2` + `--rate-limit
  0.5` is a safe baseline for external sites.
- **`jsonSchema` turns every page into a typed object.** Pass a JSON schema
  via MCP/REST to extract structured data from every crawled page — useful
  for price monitoring, job listings, or any repeating schema.

## See also

- [crw-map](../crw-map/SKILL.md) — discover URLs before crawling
- [crw-scrape](../crw-scrape/SKILL.md) — single-page extraction (faster for known URLs)
- [crw](../crw/SKILL.md) — hub skill with the full workflow ladder
