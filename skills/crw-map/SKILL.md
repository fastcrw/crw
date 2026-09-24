---
name: crw-map
description: |
  Discover all URLs on a website without fetching content — fast, low-cost
  URL inventory via sitemap.xml + link extraction BFS. Use when you need to
  know which pages exist before deciding what to scrape or crawl: "list all
  pages", "find URLs on this site", "discover links", "what pages does this
  site have", "map the site". Step 3 of the crw workflow ladder.
license: AGPL-3.0
metadata:
  author: us
  version: "0.3.0"
  homepage: https://fastcrw.com
  repository: https://github.com/fastcrw/crw
allowed-tools: Bash(crw:*) Bash(curl:*) Read
---

# crw-map — URL discovery without content

## When to use

- You need to know **which URLs exist** on a site before committing to a
  crawl — map first, then crawl only the section you need.
- Step 3 in the [crw ladder](../crw/SKILL.md): if you want content, combine
  with [crw-scrape](../crw-scrape/SKILL.md) (single pages) or
  [crw-crawl](../crw-crawl/SKILL.md) (bulk). Map is URL-only — no page
  content is returned.
- Estimating crawl cost: `crw map docs.example.com | wc -l` tells you how
  many pages a subsequent crawl would touch before you commit.
- Finding a specific path: filter the URL list with `grep` instead of
  crawling the whole site.

## Quick start

**CLI** (binary on PATH):
```bash
crw map "https://docs.example.com"                       # URLs to stdout
crw map "https://docs.example.com" -d 3 --format json   # JSON object, depth 3
crw map "https://example.com" --sitemap-only             # sitemap.xml only
crw map "https://example.com" --no-sitemap               # link crawl only
crw map "https://example.com" --format json > .crw/urls.json
```

**MCP** (inside an agent harness):
```
crw_map(url="https://docs.example.com")
crw_map(url="https://docs.example.com", maxDepth=3, limit=200)
crw_map(url="https://example.com", useSitemap=false)       # link crawl only
crw_map(url="https://example.com", crawlFallback=false)    # sitemap only
```

**REST** (drop-in for Firecrawl SDKs — just swap the base URL):
```bash
curl -X POST "$CRW_API_URL/v1/map" -H "Authorization: Bearer $CRW_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://docs.example.com","maxDepth":2,"limit":200}'
# Response: {"success":true,"data":{"links":[...],"sitemaps":[...]}}
# links and sitemaps both live under data; sitemap files are not pages, so
# they are kept out of links
# jq tip for REST: jq '.data.links[]' / jq '.data.sitemaps[]'
```

## Options

| Need | CLI flag | MCP / REST field |
|------|----------|------------------|
| Discovery depth | `-d/--depth N` (default 2) | `maxDepth` (default 2) |
| Result format | `--format text\|json` | — (always JSON) |
| Sitemap only (no link crawl) | `--sitemap-only` | `crawlFallback: false` |
| Link crawl only (no sitemap) | `--no-sitemap` | `useSitemap: false` |
| Cap URL count | — | `limit` (default 100; `0` = unbounded) |
| JS rendering | `--js` | — |
| Proxy | `--proxy URL` | — |
| Stealth mode | `--stealth` | — |
| Rate limit | `--rate-limit N` (default 5.0 req/s) | — |
| Concurrency | `--concurrency N` (default 10) | — |
| Per-page timeout | `--timeout MS` (default 15000) | — |

MCP truncates to 100 URLs and 100 sitemaps by default, bounded independently
(`truncated: true`, plus `totalDiscovered` when the link list was cut and
`totalSitemaps` when the sitemap list was). Pass `limit: 0` to opt out.

## The map → scrape / crawl pattern

```bash
# 1. Map to see what's there
crw map "https://docs.example.com" --format json > .crw/urls.json

# 2a. Grep for the section you need
grep '"authentication"' .crw/urls.json

# 2b. Scrape a single page
crw scrape "https://docs.example.com/api/authentication"

# 2c. Or crawl the whole /api section
crw crawl "https://docs.example.com/api" -d 2 -l 50
```

With MCP in a single agent turn:
```
crw_map(url="https://docs.example.com", limit=0)
# inspect links[], pick the /changelog/* subset
crw_crawl(url="https://docs.example.com/changelog", maxDepth=1, maxPages=20)
```

## Tips

- **Map before crawl, always.** A 3-second map call can save a 10-minute
  crawl abort. If the map returns 5 000 URLs and you only need `/blog/*`,
  scope the crawl to that sub-path.
- **Sitemap + crawl fallback (default) is the most complete.** sitemap.xml
  gives canonical URLs; the BFS link scan catches pages not in the sitemap.
  Use `--sitemap-only` only when you trust the sitemap is complete.
- **Depth 2 covers most docs sites.** Increase to 3-4 for deeply nested
  wikis; 1 is enough to enumerate top-level sections.
- **Filter in shell, not in context.** Pipe to `grep`, `jq '.links[]'` (CLI
  JSON output), or `wc -l` rather than loading the full list into model context.
  For REST responses use `jq '.data.links[]'` (links are nested under `data`).
- **No content returned.** Map is intentionally URL-only. If you want page
  content, follow up with `crw scrape` or `crw crawl`.

## See also

- [crw-scrape](../crw-scrape/SKILL.md) — scrape individual URLs from the map
- [crw-crawl](../crw-crawl/SKILL.md) — bulk content extraction after mapping
- [crw](../crw/SKILL.md) — hub skill with the full workflow ladder
