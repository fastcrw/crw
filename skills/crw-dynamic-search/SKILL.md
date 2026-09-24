---
name: crw-dynamic-search
description: |
  Programmatic web search and scrape with context isolation. Use for any research
  task where you need to search the web, filter results, and extract specific
  information — without flooding your context window with raw HTML and boilerplate.
  This is the single biggest token-saver in the crw skill set. Triggered by "search
  for", "look up", "find", "research", "what's the latest on", or any query that
  requires current web information. Also use when asked to "search and filter", "find
  the important parts", or any task where you suspect the raw output will be large
  (multi-page scrapes, news aggregation, competitive research).
license: AGPL-3.0
metadata:
  author: us
  version: "0.3.0"
  homepage: https://fastcrw.com
  repository: https://github.com/fastcrw/crw
allowed-tools: Bash(crw:*) Bash(python3:*) Bash(uv run:*) Bash(jq:*)
---

# crw-dynamic-search — Programmatic Tool Calling for Web Research

Search the web and scrape pages so that **raw web data never enters your context
window**. Only your curated `print()` output comes back — pure signal, no noise.

## Why this matters

A typical `crw search --json` returns 10 results × 300-600 chars of description
each = ~5K characters. That sounds manageable — until you add `scrapeOptions` to
fetch full page markdown, which can be 20-50K chars per result. **A 10-result
search with full content ≈ 200-500K characters.** If that floods your context, you
burn tokens reading cookie banners, navigation menus, and boilerplate — and your
reasoning quality degrades under the noise.

By processing results inside a Python subprocess, only your `print()` output enters
context — typically **1-3K characters of pure signal.** That's a 100-200x reduction.

## Background: the PTC sandbox pattern

[Anthropic's Programmatic Tool Calling](https://docs.anthropic.com/en/docs/agents-and-tools/tool-use/programmatic-tool-calling)
lets a model write code that orchestrates tool calls inside a sandbox. Intermediate
results live in the sandbox; only `print()` output crosses into the context window.

**This skill applies the same principle using local Python execution.** The Python
process is your sandbox. Variables in memory hold raw data. Only what you `print()`
crosses into context. You write the filtering logic — you decide what matters for
each query.

## Core Rule

**NEVER** pipe `crw search --json` or `crw scrape --format json` bare into context.
Always process through Python so you control what enters.

```bash
# WRONG — raw results flood context, possibly 200K+ characters
crw search "quantum computing 2025" --json

# RIGHT — only your print() enters context
crw search "quantum computing 2025" --json 2>/dev/null | python3 -c "
import json, sys
data = json.load(sys.stdin)
for r in data:
    print(f'[{r[\"position\"]}] {r[\"title\"]}')
    print(f'  {r[\"url\"]}')
    print(f'  {r[\"description\"][:150]}')
"
```

## JSON Schemas

You need these to write correct filtering code.

### `crw search --json` output

The CLI outputs a **JSON array** of result objects (not a wrapper object):

```json
[
  {
    "title": "string",
    "url": "string",
    "description": "string (~200-600 chars from the search backend snippet)",
    "snippet": "string (alias of description — always same value)",
    "position": 1,
    "score": 0.85,
    "category": "general | news | images | null"
  }
]
```

Key notes for crw vs Tavily:
- **`score` is unreliable.** The search backend aggregates results from many
  engines; scores are engine-dependent and often `null`. **Triage by `position`
  (rank order) and keyword density in `description`, not by score.**
- `description` and `snippet` are always the same value — pick either. `snippet`
  exists as an alias for Firecrawl-compat pipelines.
- `category` is the search backend's category: `"general"` for web, `"news"`, `"images"`.
- `published_date` appears on news results (ISO 8601 string or null).

### `crw scrape --format json` output (ScrapeData)

The CLI outputs a single object (serialized `ScrapeData`):

```json
{
  "markdown": "string | null",
  "html": "string | null",
  "links": ["url1", "url2"],
  "renderDecision": { "kind": "autoDefault", "chosen": "http" },
  "creditCost": 1,
  "contentType": "text/html",
  "metadata": {
    "title": "string | null",
    "description": "string | null",
    "ogTitle": "string | null",
    "ogDescription": "string | null",
    "ogImage": "string | null",
    "sourceURL": "string",
    "language": "string | null",
    "statusCode": 200,
    "renderedWith": "string | null",
    "elapsedMs": 1234
  }
}
```

For most filtering tasks you want `markdown` (the main content) and
`metadata.title`. `links` is a flat array of hrefs found on the page.

### MCP `crw_search` output (when using MCP, not CLI)

The results sit in one of two places depending on which backend served the call:
directly in `data` on the hosted API (`api.fastcrw.com`), or nested as
`data.results` on a self-hosted engine. Read `data.results` when present and fall
back to `data`. The nested form:

```json
{
  "success": true,
  "data": {
    "results": [
      {
        "url": "string",
        "title": "string",
        "description": "string",
        "snippet": "string",
        "position": 1,
        "score": 0.85,
        "category": "string | null",
        "publishedDate": "string | null"
      }
    ]
  }
}
```

When `scrapeOptions` is passed, each result also carries `markdown`, `html`,
`links`, and `metadata` populated from the full page fetch.

## Execution modes

### Pipe mode — for simple filters (3-5 lines)

```bash
crw search "Python 3.13 release date" --json 2>/dev/null | python3 -c "
import json, sys
data = json.load(sys.stdin)
for r in data[:3]:
    print(r['title'])
    print(r['description'][:300])
    print()
"
```

### Heredoc mode — for anything more complex (default)

Single Bash call, clean multi-line Python, no escaping, no temp files. The
single-quoted `<< 'PYEOF'` heredoc is the workhorse — nothing inside is
interpolated by the shell.

```bash
python3 << 'PYEOF'
import json, subprocess

raw = subprocess.check_output(
    ['crw', 'search', 'your query', '--json', '--limit', '10'],
    stderr=subprocess.DEVNULL
)
data = json.loads(raw)
for r in data:
    print(f'[{r["position"]}] {r["title"]}')
    print(f'  {r["url"]}')
    print(f'  {r["description"][:200]}')
    print()
PYEOF
```

**Save DATA to `/tmp/`, not CODE.** Saving `/tmp/crw_results.json` for use in the
next turn = good. Writing a one-shot `/tmp/filter.py` = wasteful; use a heredoc.

### Script mode — only for reusable pipelines

Only write a real file when the same script will be called across 3+ turns or
invoked repeatedly. Otherwise, use a heredoc.

## Multi-turn iteration

Complex research needs **explore then extract** — see what's available before
deciding what to drill into. The key: save raw JSON to `/tmp/` once, process in
separate steps.

### Turn 1: Search and triage

```bash
python3 << 'PYEOF'
import json, subprocess

raw = subprocess.check_output(
    ['crw', 'search', 'solid-state battery commercialization 2025',
     '--json', '--limit', '10'],
    stderr=subprocess.DEVNULL
)
data = json.loads(raw)

# Save raw — stays on disk, never enters context
with open('/tmp/crw_results.json', 'w') as f:
    json.dump(data, f)

# Print only what you need to pick next steps
# Sort by position (rank), not score — score is unreliable from the search backend
print(f'{len(data)} results saved to /tmp/crw_results.json\n')
for r in data:
    print(f'[{r["position"]}] {r["title"][:90]}')
    print(f'    {r["url"]}')
    print(f'    {r["description"][:150]}')
    print()
PYEOF
```

Context receives: ~600-800 tokens of titles + snippets. Any full page markdown is
in `/tmp/crw_results.json`, untouched.

### Turn 2: Extract from chosen results

You saw the triage. Now write targeted extraction for the results that matter:

```bash
python3 << 'PYEOF'
import json, subprocess

data = json.load(open('/tmp/crw_results.json'))

# Indices you chose from the triage in turn 1
for r in [data[0], data[2], data[4]]:
    # Scrape the full page for results that looked relevant
    try:
        raw = subprocess.check_output(
            ['crw', 'scrape', r['url'], '--format', 'json'],
            stderr=subprocess.DEVNULL, timeout=30
        )
        page = json.loads(raw)
    except Exception:
        continue

    md = page.get('markdown') or ''
    if not md:
        continue

    print(f'## {r["title"]}')
    print(f'URL: {r["url"]}\n')

    # Write filtering logic that matches the query — this is the key step
    # Example: keep paragraphs about commercialization timelines
    for para in md.split('\n\n'):
        para = para.strip()
        if len(para) > 80 and any(kw in para.lower() for kw in
                ['toyota', 'quantumscape', 'samsung', 'production',
                 'commercializ', '2025', '2026', 'gigafactory']):
            print(para)
            print()
    print('---\n')
PYEOF
```

Context receives: ~600-800 tokens of targeted content. You made the decision.

### Turn 3: Follow leads

Turn 2 often surfaces new URLs or specific sub-topics. Keep iterating:

```bash
python3 << 'PYEOF'
import json, subprocess

# A URL you found referenced in the content you read in turn 2
raw = subprocess.check_output(
    ['crw', 'search', 'QuantumScape QSE-5 production timeline Q4 2025',
     '--json', '--limit', '5'],
    stderr=subprocess.DEVNULL
)
data = json.loads(raw)

for r in data[:3]:
    print(f'## {r["title"]}')
    print(f'URL: {r["url"]}')
    print(r['description'])
    print()
PYEOF
```

## When to use single-turn vs multi-turn

**Single turn** (pipe or one heredoc): when you know what you're looking for. Specific
factual queries, known keywords, lookup tasks.

**Multi-turn** (save + explore + extract): when you need to see what's available
before deciding what to extract. Open-ended research, competitive analysis, queries
where you don't know the right keywords yet.

## Writing your filtering code

The Python you write IS the filtering logic. There are no fixed templates. Principles:

**Triage by position, not score.** The search backend's scores are engine-dependent
and often absent. Result order (`position: 1, 2, 3...`) is a more reliable signal —
the aggregator's RRF ranking already baked in multi-engine consensus.

**Be specific.** A financial query should filter for numbers and financial terms.
A technical query should look for code blocks and version strings. Match your
filtering to the domain.

**Skip structural noise.** Lines shorter than ~50 chars are usually nav elements,
breadcrumbs, or button labels. Skip them. Keep headings and their following
paragraphs.

**Print structured output** so it's easy to reason over:

```python
print(f'## {title}')
print(f'URL: {url}\n')
print(relevant_content)
print('---\n')
```

**Handle errors.** Pages 404, scrapes timeout, the search backend returns partial results.
Always wrap scrape calls in try/except:

```python
try:
    raw = subprocess.check_output(['crw', 'scrape', url, '--format', 'json'],
                                   stderr=subprocess.DEVNULL, timeout=30)
except Exception:
    continue
```

**Token budget.** Your `print()` output is what enters context. Target 150-600
tokens per source. If you're printing 5000+ chars from one page, you're not
filtering enough. Exception: dense data tables or spec pages where every row
counts.

## Full example: multi-angle research

```bash
python3 << 'PYEOF'
import json, subprocess

# Fan out: hit the same topic from multiple angles
queries = [
    ('general', 'EU AI Act compliance requirements 2025'),
    ('specific', 'EU AI Act high-risk AI systems Article 6 obligations'),
]

all_results = []
for label, q in queries:
    raw = subprocess.check_output(
        ['crw', 'search', q, '--json', '--limit', '8'],
        stderr=subprocess.DEVNULL
    )
    results = json.loads(raw)
    for r in results:
        r['_query'] = label
    all_results.extend(results)

# Deduplicate by URL
seen = set()
unique = []
for r in all_results:
    if r['url'] not in seen:
        seen.add(r['url'])
        unique.append(r)

# Save everything
with open('/tmp/eu_ai_results.json', 'w') as f:
    json.dump(unique, f)

# Print triage sorted by position within each query batch
print(f'{len(unique)} unique results from {len(queries)} queries\n')
for r in unique[:12]:
    print(f'[{r["_query"]}][pos {r["position"]}] {r["title"][:80]}')
    print(f'  {r["url"]}')
    print(f'  {r["description"][:120]}')
    print()
PYEOF
```

## jq fallback

When `python3` is unavailable, use `jq` for basic filtering:

```bash
# Print titles and URLs only
crw search "query" --json 2>/dev/null | jq '.[] | {title, url, description: .description[:200]}'

# Filter by keyword in description
crw search "query" --json 2>/dev/null | jq '[.[] | select(.description | ascii_downcase | contains("keyword"))]'
```

jq can't do multi-step search-then-scrape, subprocess calls, or complex filtering.
Use it only for simple single-pass lookups when Python isn't available.

## CLI quick reference

```bash
crw search "query"                                     # text output (default)
crw search "query" --json                              # JSON array
crw search "query" --json --fields title,url,snippet   # projected fields only
crw search "query" --json --limit 5                    # cap results
crw search "query" --category news --time-range week   # news, last 7 days
crw scrape "https://example.com"                       # markdown
crw scrape "https://example.com" --format json         # full ScrapeData JSON
crw scrape "https://example.com" --format json -o /tmp/page.json
```

Available `--fields` for `crw search --json`:
`title`, `url`, `description`, `snippet`, `position`, `score`, `category`

## See also

- [crw-search](../crw-search/SKILL.md) — full search options (time-range, categories, language)
- [crw-scrape](../crw-scrape/SKILL.md) — scrape a known URL
- [crw-best-practices](../crw-best-practices/SKILL.md) — choosing the right verb, post-filtering strategies
