<p align="right"><a href="https://github.com/fastcrw/crw/blob/main/README.zh-CN.md">中文文档 (README.zh-CN.md)</a></p>

<div class="page-intro">
  <div class="page-kicker">Get Started</div>
  <h1>CRW Docs</h1>
  <p class="page-subtitle">Open-source web scraping API for agents and RAG. Scrape, crawl, search, map, and extract data from any site with one Rust binary, self-hosted or on fastCRW Cloud.</p>
  <div class="page-capabilities">
    <div class="page-capability"><strong>Fastest first win:</strong> one URL, one markdown response</div>
    <div class="page-capability"><strong>Works for:</strong> agents, ETL, RAG, structured extraction</div>
    <div class="page-capability"><strong>Deploy:</strong> cloud first, self-host when ready</div>
  </div>
  <div class="page-actions">
    <a class="page-btn primary" href="#quick-start">Make your first request</a>
    <a class="page-btn secondary" href="#self-hosting">Self-host CRW</a>
  </div>
</div>

## Setting up an AI coding agent?

Paste this into Claude Code, Cursor, Codex, Windsurf, or any other coding agent's chat:

:::note
Install fastCRW for web access: run <code>npx crw-mcp@latest install</code> to add the MCP server and skill, then use the <code>crw_scrape</code>, <code>crw_search</code>, <code>crw_map</code>, and <code>crw_crawl</code> tools (or the native <code>/v1</code> REST API) to fetch, search, and crawl the web with the <code>CRW_API_KEY</code> in my environment.
:::

Full reference written for autonomous agents: [Agent Onboarding Guide](/agent-onboarding).

## Pick your endpoint

:::cards
::card{icon="code" title="Scrape" href="#scraping" description="One known URL in, clean markdown or JSON out."}
::card{icon="search" title="Search" href="#search" description="No URL yet? Search the web and get ranked results with content."}
::card{icon="map" title="Map" href="#map" description="Discover every URL under a domain before you scrape or crawl."}
::card{icon="layers" title="Crawl" href="#crawling" description="Scrape every page under a URL in one background job."}
::card{icon="key" title="Extract" href="#extract" description="Pull structured JSON fields from a page with an LLM."}
::card{icon="plug" title="MCP" href="#mcp" description="Give Claude, Cursor, Codex, and other agents live web access."}
:::

> **New to CRW? Use `/v1`.** The `/v1` routes are the native fastCRW API for new integrations. Use `/firecrawl/v2` when migrating existing Firecrawl v2 SDK code or when you need compatibility-only routes such as batch scrape or PDF parse.

<div class="playground-panel">
  <div class="playground-kicker">30-second example</div>
  <div class="playground-title">The shortest path to a successful response</div>
  <div class="playground-copy">If this request works, you already understand the core CRW model: known URL in, clean content out. Everything else in the docs builds on that.</div>
</div>

**Try it without a key:** run this exact request in the <a href="https://fastcrw.com/playground?url=https%3A%2F%2Fexample.com&mode=scrape" target="_blank" rel="noopener">playground</a> (rate-limited preview, no signup).

## Does this work for me?

| I want to… | Use |
|---|---|
| Scrape a known URL → markdown / JSON | **scrape** |
| Discover all pages under a domain | **map** |
| Crawl and scrape many pages in one job | **crawl** |
| Find relevant URLs via web search | **search** |
| Pull structured data with an LLM | **extract** |
| Drive a real browser, click, fill forms | **crw-browse** (companion service) |
| Give my AI agent live web access | **MCP** |

All five core verbs share the same API key and base URL. You can mix them in a single workflow.

```bash
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://example.com",
    "formats": ["markdown"]
  }'
```

```json
{
  "success": true,
  "data": {
    "markdown": "# Example Domain\n\nThis domain is for use in illustrative examples in documents.",
    "metadata": {
      "title": "Example Domain",
      "sourceURL": "https://example.com",
      "statusCode": 200,
      "renderedWith": "http",
      "elapsedMs": 218
    }
  }
}
```

## Start here

:::cards
::card{icon="code" title="Scrape a page" href="#scraping" description="Use one URL and get markdown, HTML, links, or JSON back."}
::card{icon="search" title="Search the web" href="#search" description="Find URLs first, then scrape only the results you care about."}
::card{icon="cursor" title="crw-browse (companion)" href="/docs/mcp#browser-automation-crw-browse" description="Separate companion service for multi-step browser automation — clicks, form fills, stateful sessions. Not part of the core API verbs."}
::card{icon="plug" title="Add MCP tools" href="#mcp" description="Give Claude, Cursor, Codex, and other hosts live web access."}
:::

## Choose your path

:::cards
::card{icon="rocket" title="Cloud API" href="#quick-start" description="The fastest first run: get a key, copy one request, and move."}
::card{icon="plug" title="MCP" href="#mcp" description="Best when your agent runtime already expects MCP tools."}
::card{icon="box" title="Self-host" href="#self-hosting" description="Best when you want your own infrastructure, auth, and deployment controls."}
:::

## Why teams switch to CRW

CRW is meant to feel easy on day one without closing off the more serious use cases:

- one native `/v1` API surface for single-page scrape, discovery, bounded crawl, search, and extraction,
- a `/firecrawl/v2` Firecrawl compatibility layer for migration work,
- low-ops self-hosting when you need infra control,
- and a built-in MCP server for agent workflows.

## Benchmarks

Public 3-way run on [Firecrawl scrape-content-dataset-v1](https://huggingface.co/datasets/firecrawl/scrape-content-dataset-v1), full 1000 URL, canonical `diagnose_3way.py` harness (concurrency 5 / timeout 120s):

| Metric | CRW | crawl4ai | Firecrawl |
|---|---|---|---|
| **Truth-recall (522/819 labeled URLs)** <sup>recall mode</sup> | **63.74%** | 59.95% | 56.04% |
| **p50 latency** | **1914ms** | 1916ms | 2305ms |
| Thrown errors (3000 requests) | 0 | 0 | 0 |
| Dependencies | single binary | Python + Playwright | Node + Redis + PG + RabbitMQ |

CRW leads on every axis — top truth-recall and fastest median — with **0 thrown errors** across all 3,000 requests, and it uniquely recovers **34 URLs the other two miss** (70% more than crawl4ai and Firecrawl combined). The 63.74% denominator is **819 labeled/matchable URLs**, not 3,000 requests, not 1,000. **Two modes, one binary, one config toggle:** *recall mode* maximizes truth-recall; *fast mode* (LightPanda-only) trades recall for lower latency. Full result: [`bench/server-runs/RESULT_3WAY_1000_FULL.md`](https://github.com/fastcrw/crw/blob/main/bench/server-runs/RESULT_3WAY_1000_FULL.md).

## What to read next

- [Quick Start](#quick-start) for the fastest first request
- [API Overview](#rest-api) for the endpoint map
- [Scrape](#scraping) for the canonical single-page flow
- [Authentication](#authentication) for key handling and self-host auth
