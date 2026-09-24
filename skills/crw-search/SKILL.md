---
name: crw-search
description: |
  Search the web with fastCRW and get titles, URLs, and descriptions.
  Use when you have a question or topic but not a URL — "search for",
  "find pages about", "look up", "what is", "who is", "latest news on",
  "find docs for". Own search backend: self-hosted, no API key, no per-query
  cost, high recall via meta-search aggregation. Step 1 of the crw
  workflow ladder.
license: AGPL-3.0
metadata:
  author: us
  version: "0.3.0"
  homepage: https://fastcrw.com
  repository: https://github.com/fastcrw/crw
allowed-tools: Bash(crw:*) Bash(curl:*) Read
---

# crw-search — web search via crw's own search backend

## When to use

- You have a **question or topic**, not a URL. Get candidate URLs first, then
  scrape the one that looks right.
- Step 1 in the [crw ladder](../crw/SKILL.md): most searches should end at
  step 2 ([crw-scrape](../crw-scrape/SKILL.md)) — pick the best result URL
  and scrape it for full content.
- **Token-heavy context?** Pipe through a subprocess filter instead of dumping
  raw JSON. See [crw-dynamic-search](../crw-dynamic-search/SKILL.md).
- The search backend is self-hosted and free — no API key, no per-query billing,
  no usage cap. Queries never leave your infrastructure in embedded/local mode.

## Quick start

**CLI** (binary on PATH):
```bash
crw search "rust async http client"                              # text output
crw search "site:docs.rs tokio" --json --fields title,url,snippet --limit 5
crw search "CVE-2024-1234" --category news --time-range week
crw search "climate policy 2025" --json -o .crw/results.json
crw search "rust crates" --language en --limit 20
```

**MCP** (inside an agent harness):
```
crw_search(query="rust async http client", limit=5, lang="en")
crw_search(query="latest CVE nginx", tbs="qdr:w", categories="news")
crw_search(query="openai pricing", scrapeOptions={"formats": ["markdown"]})
```

**REST** (drop-in for Firecrawl SDKs — just swap the base URL):
```bash
curl -X POST "$CRW_API_URL/v1/search" -H "Authorization: Bearer $CRW_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"query":"rust async http","limit":5,"lang":"en"}'
```

## Options

| Need | CLI flag | MCP / REST field |
|------|----------|------------------|
| Result count | `-l/--limit N` (default 10) | `limit` (default 5) |
| JSON output | `--json` or `--format json` | — (always JSON) |
| Field projection | `--fields title,url,snippet` | — |
| Output to file | `-o FILE` | — |
| Filter by category | `--category news\|images\|videos\|general\|…` | `categories` |
| Language | `--language en` | `lang` |
| Time filter | `--time-range day\|week\|month\|year` | `tbs: qdr:h\|qdr:d\|qdr:w\|qdr:m\|qdr:y` |
| Safe search | `--safesearch 0\|1\|2` | — |
| Custom search backend | `--searxng-url URL` / `$CRW_SEARXNG_URL` | — |
| Group by source | — | `sources: ["web","news","images"]` |
| Scrape results inline | — (use crw scrape separately) | `scrapeOptions: {formats:["markdown"]}` |

**`--fields` available values:** `title`, `url`, `description`, `snippet`,
`position`, `score`, `category`. `snippet` is an alias for `description`.

## A note on result scores

The search backend is a **meta-search aggregator** — it merges results from
multiple engines (Google, Bing, DuckDuckGo, etc.) and the `score` field
reflects internal engine weighting, not a universal relevance measure. Do not
rely on `score` for ranking or filtering. Use **`position`** (1-based rank) or
**result order** instead — position 1 is the most relevant result surfaced.

## Tips

- **No results / 403?** The search backend needs JSON output enabled in its
  config. Run `crw setup --local` to spin up a pre-configured sidecar
  automatically. Public instances usually block JSON with 403/429.
- **`--fields` saves context.** `--json --fields title,url,snippet --limit 5`
  is one call; piping to `jq` is two. Prefer the flag.
- **Inline scraping via MCP.** Pass `scrapeOptions: {formats: ["markdown"]}`
  to get page content alongside search results in one round-trip. There is no
  `--scrape` CLI flag — use the MCP/REST path for this.
- **Time-sensitive queries.** Use `--time-range week` (CLI) or `tbs: "qdr:w"`
  (MCP/REST) for news, CVEs, releases, or any freshness-sensitive topic.
- **After search, scrape the winner.** `crw search "…"` returns candidates;
  `crw scrape "<url>"` gets the full content. Don't try to read content from
  search snippets alone.
- **Write large result sets to `.crw/`.** Never stream a 20-result JSON blob
  to stdout into context. Use `-o .crw/results.json` then `jq`/`grep`.

## See also

- [crw-scrape](../crw-scrape/SKILL.md) — scrape the URL you found
- [crw-dynamic-search](../crw-dynamic-search/SKILL.md) — filter output in a
  subprocess to save context (use this on token-heavy tasks)
- [crw](../crw/SKILL.md) — hub skill with the full workflow ladder
