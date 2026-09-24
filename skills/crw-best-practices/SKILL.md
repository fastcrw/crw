---
name: crw-best-practices
description: |
  Reference skill for building production-ready crw integrations. Covers verb
  selection, call surfaces (CLI/MCP/REST), post-filtering strategies, context-window
  hygiene, Hybrid RAG patterns, common pitfalls, and crw-specific operational
  considerations (search backend limits, renderer pool, proxy rotation). Load this when
  writing application code that embeds crw, designing a multi-step agent workflow,
  or debugging an integration that isn't behaving as expected.
license: AGPL-3.0
metadata:
  author: us
  version: "0.3.0"
  homepage: https://fastcrw.com
  repository: https://github.com/fastcrw/crw
---

# crw Best Practices

Reference documentation for developers and AI agents using crw (fastCRW) in
production. Covers decision-making, integration patterns, and crw-specific
operational details.

## 1. Choosing the right verb

Stop at the cheapest rung that answers the need. Don't reach for a heavier verb
than the task requires.

| Need | Verb | Notes |
|------|------|-------|
| You have a question/topic, not a URL | **search** | Own search backend, no API key required. Returns titles + URLs + snippets. Add `scrapeOptions` to get markdown inline. |
| You have one (or a few) known URLs | **scrape** | Returns markdown, HTML, links, or structured JSON. JS auto-detected. |
| You need to discover which URLs exist on a site | **map** | Fast URL discovery via sitemap + BFS. No content fetched. Use before committing to a crawl. |
| You need content from many pages under a site | **crawl** | Async BFS job. Poll with `crw_check_crawl_status`. Always `map` first to estimate size. |
| The source is a local file (PDF) | **parse** | `crw_parse_file` (MCP) or `crw scrape path/to/file.pdf` (CLI). No network call. |
| You need a typed JSON object from a page | **extract** | `--extract '<schema>'` (CLI) or `extract: {schema: {...}}` (MCP/REST). Runs an LLM; costs tokens. |
| You want to detect what changed on a page | **watch / diff** | `POST /v1/change-tracking/diff`. Stateless diff primitive; no stored state needed. |

**Common chains:**
- `search` → pick URLs → `scrape` the best ones
- `search --json` (or `crw_search`) → filter in Python subprocess → `crw scrape` chosen URLs
- `map` → estimate page count → `crawl` a bounded section → stream results
- `map "https://docs.example.com"` → find URLs → filter for `/docs/api/auth` → `scrape` that one URL

## 2. Three call surfaces

crw runs identically in three modes. Pick the one available in your environment.

### CLI (`crw`)

Best for scripting, one-shot queries, and agent bash calls. Binary must be on PATH.

```bash
crw search "query" --json --limit 5
crw scrape "https://example.com" --format json
crw map "https://docs.example.com"
crw scrape "report.pdf"                              # local PDF auto-detected
```

**Use CLI when:** the binary is on PATH and you're in a Bash context. Especially
good for the dynamic-search pattern (pipe into Python subprocess).

### MCP tools (`crw_scrape`, `crw_crawl`, `crw_check_crawl_status`, `crw_map`, `crw_extract`, `crw_check_extract_status`, `crw_cancel_extract`, `crw_search`, `crw_parse_file`)

Best inside an MCP-capable agent harness. The MCP server runs the engine either
in-process (embedded mode, ~14 MB RAM, no server) or as a proxy to a REST endpoint.

```
crw_scrape(url="https://example.com", formats=["markdown"], onlyMainContent=true)
crw_search(query="query", limit=5)
crw_map(url="https://docs.example.com", limit=200)
```

**MCP output bounds (defaults):** content truncated to ~15,000 chars per call;
`crw_map` returns ≤ 100 URLs. Both carry `truncated: true` when clipped. Pass
`maxLength: 0` / `limit: 0` to opt out.

**Use MCP when:** you're inside Claude Code, Cursor, Windsurf, or any harness that
manages MCP connections. Lower per-call overhead than REST for agent loops.

### REST API (`/v1/scrape`, `/v1/search`, etc.)

Best for application code, cross-language clients (Go, Java, Ruby), or when you
need a shared microservice. Firecrawl-compatible — SDK swap is one `api_url` change.

```python
# Python SDK (pip install crw)
from crw import CrwClient
client = CrwClient(api_url="https://api.fastcrw.com", api_key="crw_live_...")
result = client.scrape("https://example.com", formats=["markdown"])
results = client.search("AI news", limit=10)

# Drop-in for Firecrawl SDK
from firecrawl import FirecrawlApp
app = FirecrawlApp(api_url="https://api.fastcrw.com", api_key="crw_live_...")
```

**Use REST when:** writing application code, needing async crawl jobs with polling,
or integrating with frameworks like LangChain / CrewAI / LlamaIndex.

## 3. Post-filtering strategy stack

Raw web results carry noise. Apply these in order, stopping when you have enough
signal.

### Layer 1: Rank/order-based triage (free)

The search backend's raw score is unreliable (engine-dependent, often null).
**Position is the reliable signal** — it reflects the aggregator's Reciprocal
Rank Fusion over N engines. Default: trust the top 3-5 results unless they're
obviously off-topic.

```python
# Rely on position, not score
top = [r for r in results if r['position'] <= 5]
```

### Layer 2: Regex / keyword density filter (cheap)

Before fetching full pages, filter descriptions for relevance. Drop results whose
description doesn't contain any query-adjacent term.

```python
keywords = {'commercializ', 'battery', 'production', '2025', '2026'}
relevant = [r for r in results
            if any(kw in r['description'].lower() for kw in keywords)]
```

After scraping full markdown, apply paragraph-level filtering:

```python
for para in markdown.split('\n\n'):
    if len(para) > 60 and any(kw in para.lower() for kw in keywords):
        print(para)
```

### Layer 3: LLM verify (expensive — use sparingly)

When layers 1-2 aren't precise enough, send a small batch of candidate snippets to
a cheap model for binary relevance classification.

```python
import anthropic

def is_relevant(snippet: str, query: str) -> dict:
    """Returns {is_match: bool, confidence: float, reasoning: str}"""
    client = anthropic.Anthropic()
    msg = client.messages.create(
        model="claude-haiku-4-5",   # cheap model for classification
        max_tokens=128,
        messages=[{
            "role": "user",
            "content": (
                f"Query: {query}\n\n"
                f"Snippet: {snippet[:500]}\n\n"
                "Does this snippet directly answer or provide evidence for the query? "
                "Reply with JSON only: {\"is_match\": true/false, \"confidence\": 0-1, "
                "\"reasoning\": \"one sentence\"}"
            )
        }]
    )
    import json
    return json.loads(msg.content[0].text)
```

**Gate:** only call LLM-verify on snippets that passed layers 1-2. Don't send all
10 results through an LLM — pick the 3-5 most promising first.

## 4. Context-window hygiene

The single most important practice. See [crw-dynamic-search](../crw-dynamic-search/SKILL.md)
for the full pattern. Summary:

- **Never pipe `crw search --json` or `crw scrape --format json` bare into context.**
  Always filter in a Python subprocess — only your `print()` output enters context.
- **Write large results to `.crw/` or `/tmp/`, not stdout.** Use `crw scrape -o
  .crw/page.json` then read selectively with `grep` or a Python heredoc.
- **MCP truncation is your first line of defense** (default ~15K chars). But don't
  rely on it alone — a 15K char page is still 3,500+ tokens.
- **Target 150-600 tokens per source** in your filtered output. If you're printing
  more from a single page, you're probably including boilerplate.

## 5. Self-hosted Hybrid RAG pattern

crw is optimized for the **retrieve → filter → embed** pipeline. Typical setup:

```
crw search "query" → top-N results (titles + snippets)
→ scrape top 3-5 full pages → filter to relevant paragraphs
→ embed filtered paragraphs → merge with local vector store
→ retrieve top-K chunks → feed to generation model
```

**Why crw for RAG:**
- Search costs $0 per query (no per-call API fees)
- Recurring crawls use VPS cost, not per-page credits
- `crw_crawl` + `jsonSchema` can extract typed objects per page directly —
  skip the embed step for structured data

**Python RAG skeleton:**

```python
from crw import CrwClient

client = CrwClient()  # embedded mode, no server

def retrieve_and_chunk(query: str, top_n: int = 5) -> list[str]:
    results = client.search(query, limit=top_n)
    chunks = []
    for r in results:
        # Scrape full page if the snippet isn't sufficient
        page = client.scrape(r['url'], formats=['markdown'])
        md = page.get('markdown', '') or ''
        # Split into paragraphs, keep non-trivial ones
        for para in md.split('\n\n'):
            para = para.strip()
            if len(para) > 100:
                chunks.append(para)
    return chunks
```

For a local vector store (Chroma, Qdrant, pgvector): embed these chunks, upsert
with URL + position as metadata, then merge vector-store retrieval results with
fresh `crw search` results at query time (hybrid retrieval).

## 6. Common pitfalls

| Problem | Impact | Solution |
|---------|--------|----------|
| Piping raw JSON into context | 50K-500K chars enters context; token waste, reasoning degradation | Always filter in a Python subprocess — see [crw-dynamic-search](../crw-dynamic-search/SKILL.md) |
| Trusting `score` for triage | The search backend's scores are engine-dependent, often `null`; wrong results picked | Triage by `position` (rank order) + keyword density in `description` |
| Crawling without mapping first | Committing to a 500-page crawl when you needed 20 pages | Always `crw map` first to estimate site size; cap with `maxPages` |
| JS rendering on every scrape | Unnecessary browser spawn on plain-HTML pages; slow | crw auto-detects SPAs — don't add `--js` / `renderJs: true` unless the page is blank |
| Blocking on crawl job poll | Agent hangs waiting for async crawl | Set a poll interval (5-10s), set `maxPages` to bound job size, check `status: "completed"` |
| Ignoring `truncated: true` | Missing content from MCP calls; silent data loss | Check for `truncated: true` in MCP responses; pass `maxLength: 0` if you need full content |
| Writing one-shot scripts to `/tmp/` | Wasteful; file left behind | Use heredocs for one-shot filtering; only write data (JSON results) to `/tmp/` |
| Scraping `robots.txt`-blocked pages | 403/empty response; wasted call | crw respects `robots.txt` by default; use `--stealth` + proxy for legitimate access to blocked pages |

## 7. crw-specific operational awareness

Unlike credit-based APIs (Firecrawl, Tavily), crw's costs are infra-denominated.
The right mental model: **you're paying for VPS time and renderer pool capacity, not
per-page fees.**

### Search backend rate limits and politeness

- Public instances rate-limit or block JSON requests — **always use a local
  instance** (`crw setup --local` boots one via Docker).
- The self-hosted search backend has no built-in per-client rate limit, but the
  upstream engines (Google, Bing, DDG) do. Burst too hard and engines start
  returning 429s or CAPTCHAs to your instance.
- Practical safe rate: 2-4 searches/second burst, < 1/second sustained. Space
  parallel searches with a short sleep or process them in series.
- `--category news` and `--time-range week` bypass the general engine pool —
  lighter on upstream rate limits.

### Renderer pool sizing

crw runs a renderer ladder per request (HTTP → LightPanda → Chrome by default; additional tiers such as playwright and chrome_proxy are available via config).
- **HTTP tier** is instant and stateless (no pool cost).
- **LightPanda** is lightweight (~50 MB) but single-process per binary instance.
  Under load, requests queue behind the LightPanda instance.
- **Chrome** (optional, `docker compose --profile heavy`) is the stealth fallback.
  Each Chrome instance is ~200 MB RAM. Scale by running multiple Chrome instances
  or pointing at a remote CDP endpoint via `[renderer.chrome] ws_url` in your
  server config (the `CRW_CDP_URL` env var is honored by `crw scrape --js` in
  CLI mode only, not by server/MCP mode).
- If you see consistent p90 timeouts, you're likely hitting the renderer queue.
  Add more Chrome instances or switch to fast mode (LightPanda-only, lower recall
  but faster tail).

### Proxy rotation

Self-hosted crw supports per-request BYOP (bring-your-own-proxy) via `--proxy URL`
(CLI) or `proxy` / `proxyRotation` (MCP/REST). Rotation modes: `round_robin`,
`random`, `sticky_per_host`.

- **LightPanda can't proxy** — when a proxy is active, LightPanda is skipped
  (fail-closed). Only the HTTP and Chrome tiers route through the proxy.
- If using proxies for scraping targets that block cloud IPs, set
  `proxyRotation: "sticky_per_host"` so sessions from the same domain always hit
  the same exit IP (avoids anti-bot CAPTCHA triggers from IP-hopping mid-session).
- Proxy rotation applies to `scrape`, `crawl`, and `map` — not `search` (which
  goes to your local search backend, not directly to search engines).

### Managed vs self-hosted call-surface differences

| Feature | Self-hosted | Managed (`api.fastcrw.com`) |
|---------|-------------|------------------------------|
| Search | Requires a local search-backend sidecar | Included (managed backend) |
| Proxy pool | BYOP via config | Managed proxy network |
| Rate limiting | Token-bucket (configurable) | Per-plan limits; `X-RateLimit-*` headers |
| Credits | N/A | 1000 one-time lifetime free credits |
| AGPL obligation | Applies if you expose to third parties | Carve-out included |

## 8. Links

- Hub skill: [crw](../crw/SKILL.md)
- Token-saving subprocess pattern: [crw-dynamic-search](../crw-dynamic-search/SKILL.md)
- REST API reference: https://docs.fastcrw.com/#rest-api
- Self-host guide: https://docs.fastcrw.com/#self-hosting
- Firecrawl compatibility matrix: `COMPATIBILITY-firecrawl.md` in the repo
- Benchmarks: https://fastcrw.com/benchmarks
