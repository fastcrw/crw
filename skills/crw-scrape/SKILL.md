---
name: crw-scrape
description: |
  Scrape a single known URL into clean markdown / HTML / links / structured
  JSON with fastCRW. Use when you already have the URL and want the page
  content — "scrape", "grab", "fetch", "pull", "read this page", "get the
  content of". Handles JavaScript-rendered SPAs automatically. Step 2 of the
  crw workflow ladder.
license: AGPL-3.0
metadata:
  author: us
  version: "0.3.0"
  homepage: https://fastcrw.com
  repository: https://github.com/fastcrw/crw
allowed-tools: Bash(crw:*) Bash(curl:*) Read
---

# crw-scrape — single-page extraction

## When to use

- You have one (or a handful of) **known URLs** and want their content.
- Step 2 in the [crw ladder](../crw/SKILL.md): if you don't have a URL yet, go
  to [crw-search](../crw-search/SKILL.md) (step 1) first. For many pages under a
  site, use [crw-crawl](../crw-crawl/SKILL.md) (step 4). For a local PDF, use
  [crw-parse](../crw-parse/SKILL.md) (step 5).
- JS-heavy page? You usually don't need anything special — crw auto-detects and
  renders. This is a crw advantage: no separate "interact"/browser step.

## Quick start

**CLI** (binary on PATH):
```bash
crw scrape "https://example.com"                       # → markdown to stdout
crw scrape "https://example.com" --format json -o page.json
crw scrape "https://example.com" --js --css "article.main"
crw scrape "https://example.com" --format links -o .crw/links.txt
```

**MCP** (inside an agent harness):
```
crw_scrape(url="https://example.com", formats=["markdown"], onlyMainContent=true)
```

**REST** (drop-in for Firecrawl SDKs — just swap the base URL):
```bash
curl -X POST "$CRW_API_URL/v1/scrape" -H "Authorization: Bearer $CRW_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com","formats":["markdown"],"onlyMainContent":true}'
```

## Options

| Need | CLI flag | MCP / REST field |
|------|----------|------------------|
| Output format | `--format markdown\|html\|rawhtml\|text\|links\|json` | `formats: [...]` |
| Strip nav/footer/sidebar | (on by default; `--raw` to disable) | `onlyMainContent: true` |
| Force JS rendering | `--js` | `renderJs: true` (null = auto) |
| Wait after load | — | `waitFor: 2000` (ms) |
| Keep only selectors | `--css "article"` / `--xpath …` | `includeTags: ["article"]` |
| Drop selectors | — | `excludeTags: ["nav","footer"]` |
| Pick renderer | — | `renderer: "auto\|lightpanda\|chrome\|chrome_proxy\|playwright"` (`auto` is default) |
| Save to file | `-o FILE` | (write the response yourself) |
| Structured JSON | `--extract '<schema>'` | `extract: {schema: {...}}` — see [crw-extract](../crw-extract/SKILL.md) |
| Use a proxy | `--proxy URL` `--stealth` | `proxy`, `proxyRotation`, `stealth` |

## Tips

- **Quote URLs** — `?` and `&` are shell-special. Always wrap in quotes.
- **Multiple URLs = run them concurrently.** Fire several `crw scrape … &` and
  `wait`, or issue parallel MCP calls.
- **Blank page / loading skeleton?** Add `--js` / `renderJs: true`, optionally a
  `waitFor`. crw's auto-detect covers most SPAs without it.
- **Don't dump huge pages into context.** Write to `.crw/`, then `grep`/`head`.
  MCP truncates to ~15 000 chars (`maxLength: 0` to opt out).
- **Want a typed object, not prose?** `--format json` returns the raw full-page
  object (metadata + content), not schema-extracted data. For structured
  extraction against a schema use `--extract '<schema>'` — this calls an LLM
  and **requires a configured LLM provider**. See the dedicated
  [crw-extract](../crw-extract/SKILL.md) skill.
- **Source is a file, not a URL?** Use [crw-parse](../crw-parse/SKILL.md) instead.

## See also

- [crw-search](../crw-search/SKILL.md) — find the URL first
- [crw-map](../crw-map/SKILL.md) — discover all URLs on a site
- [crw-crawl](../crw-crawl/SKILL.md) — scrape many pages at once
- [crw-dynamic-search](../crw-dynamic-search/SKILL.md) — filter scrape output in a subprocess to save context
