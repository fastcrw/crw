# How to Add Web Search to Your AI Agent (Step-by-Step API Guide)

> Learn how to give your AI agent real-time web search capabilities using CRW. Three integration paths: MCP zero-code, REST API, and self-hosted. Includes full Python example.

**Published:** 2026-04-05  
**Updated:** 2026-04-05  
**Canonical:** https://fastcrw.com/blog/web-search-api-for-ai-agents

---

## The Problem: Agents Without Web Access

You ship an AI agent. It sounds confident. It answers every question with authority. Then a user asks about something that happened last week and the agent **confidently lies**.

This is the hallucination problem, and it hits every team building with LLMs. The model's training data has a cutoff. It doesn't know about today's stock price, yesterday's CVE, or the API docs that changed last month. Without access to live information, your agent is guessing — and presenting those guesses as facts.

The pattern shows up everywhere:

- Customer support agents citing outdated product features
- Research assistants inventing paper titles and authors
- Code assistants recommending deprecated APIs
- Market analysis tools reporting stale data as current

The fix is straightforward: **give your agent a search API that returns clean, current content**. When the agent can search the web, scrape real pages, and extract structured data, it stops guessing and starts citing. The difference in output quality is immediate and measurable.

This guide walks you through three integration paths — from zero-code MCP setup to a full custom Python agent — so you can pick the one that fits your stack.

## Architecture: Search, Scrape, Extract

Before writing code, understand the pipeline. Every web-grounded agent follows the same three-step flow:

```
# Agent Web Search Pipeline
#
#  ┌──────────────┐
#  │  Agent asks   │  "What are the best practices for
#  │  a question   │   container security in 2026?"
#  └──────┬───────┘
#         ▼
#  ┌──────────────┐
#  │   Search     │  POST /v1/search
#  │   the web    │  → Returns ranked URLs with snippets
#  └──────┬───────┘
#         ▼
#  ┌──────────────┐
#  │   Scrape     │  POST /v1/scrape
#  │   top pages  │  → Returns clean markdown content
#  └──────┬───────┘
#         ▼
#  ┌──────────────┐
#  │   Extract    │  POST /v1/scrape (with JSON schema)
#  │   structure  │  → Returns typed, structured data
#  └──────┬───────┘
#         ▼
#  ┌──────────────┐
#  │   Agent      │  Reasons over fresh, real content
#  │   responds   │  with citations and sources
#  └──────────────┘
```

**Step 1: Search.** The agent sends a natural language query to the search endpoint. CRW searches the web and returns ranked results — each with a title, URL, description, and relevance score. This replaces Google Custom Search, Bing API, or SerpAPI in your stack.

**Step 2: Scrape.** The agent picks the most relevant URLs and scrapes them. CRW handles JavaScript rendering, cookie banners, paywalls, and anti-bot measures. You get back clean markdown or HTML — no boilerplate, no ads, no navigation noise.

**Step 3: Extract.** For structured use cases (prices, specs, reviews), the agent passes a JSON schema and CRW extracts typed data from the page. No regex, no CSS selectors, no brittle parsing.

The entire pipeline runs through a single API with one authentication token. Now let's implement it.

## Option 1: MCP (Zero-Code Integration)

If you're using [Claude Code, Cursor, Windsurf, or Codex](/blog/best-mcp-servers-web-scraping), the fastest path is MCP — the Model Context Protocol. One command gives your IDE agent full web search and scraping capabilities:

```
npx -y crw-mcp
```

This registers CRW's MCP tools with your agent — the most useful for search workflows:

| Tool | Description | Use Case |
| --- | --- | --- |
| `crw_search` | Search the web | Finding relevant pages for any query |
| `crw_scrape` | Scrape a URL to markdown | Reading full page content |
| `crw_crawl` | Crawl an entire site | Mapping site structure, bulk scraping |
| `crw_map` | Discover URLs on a site | Finding all pages on a domain |

After running `init`, your agent can immediately search and scrape. Here's what it looks like in Claude Code:

```
# In Claude Code, just ask:
"Search for the latest Next.js 15 breaking changes and summarize them"

# Claude Code will automatically:
# 1. Call crw_search("Next.js 15 breaking changes 2026")
# 2. Call crw_scrape on the top results
# 3. Synthesize a summary with citations
```

No API keys to configure (it uses your local CRW instance or fastCRW account). No code to write. The MCP server handles all the plumbing between your agent and the search/scrape pipeline.

For self-hosted setups, point the MCP server at your local instance:

```
npx -y crw-mcp --base-url http://localhost:3000
```

Read the full MCP setup guide: [Best MCP Servers for Web Scraping](/blog/best-mcp-servers-web-scraping).

## Option 2: REST API Integration

For custom agents, you want direct API access. Here's the step-by-step integration.

### Step 1: Get Your API Key

Two options:

- **Cloud:** Sign up at [fastcrw.com](https://fastcrw.com) and grab your API key from the dashboard. The free tier is a one-time lifetime 1000 credits (not a monthly meter).
- **Self-hosted:** Run CRW locally (see Option 3 below). No API key needed — or set one with the `CRW_API_KEY` env variable.

### Step 2: Search the Web

The search endpoint takes a natural language query and returns ranked results from across the web.

**curl:**

```
curl -X POST https://api.fastcrw.com/v1/search \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "best practices container security 2026",
    "limit": 5
  }'
```

**Python:**

```
import requests

response = requests.post(
    "https://api.fastcrw.com/v1/search",
    headers={"Authorization": "Bearer YOUR_API_KEY"},
    json={
        "query": "best practices container security 2026",
        "limit": 5,
    },
)

results = response.json()
for result in results["data"]:
    print(f"{result['title']} — {result['url']}")
    print(f"  Score: {result['score']}")
    print(f"  {result['description']}")
    print()
```

**Node.js:**

```
const response = await fetch("https://api.fastcrw.com/v1/search", {
  method: "POST",
  headers: {
    "Authorization": "Bearer YOUR_API_KEY",
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    query: "best practices container security 2026",
    limit: 5,
  }),
});

const { data } = await response.json();
data.forEach((result) => {
  console.log(`${result.title} — ${result.url}`);
  console.log(`  Score: ${result.score}`);
});
```

The response includes `title`, `url`, `description`, `position`, `score`, and `category` for each result. You can also pass `scrapeOptions` to search *and* scrape in a single request — see the [Search API reference](/blog/search-api-for-ai-agents) for details.

### Step 3: Scrape Result Pages

Once you have URLs from search, scrape them for full content:

**curl:**

```
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://example.com/container-security-guide",
    "formats": ["markdown"]
  }'
```

**Python:**

```
response = requests.post(
    "https://api.fastcrw.com/v1/scrape",
    headers={"Authorization": "Bearer YOUR_API_KEY"},
    json={
        "url": "https://example.com/container-security-guide",
        "formats": ["markdown"],
    },
)

data = response.json()["data"]
print(data["markdown"])  # Clean content, no boilerplate
```

CRW returns clean markdown by default — ads, navigation, footers, and cookie banners are stripped. You can also request `html`, `rawHtml`, `links`, or `screenshot` formats. For a deep dive on scraping to markdown, see [Website to Markdown](/blog/website-to-markdown).

### Step 4: Extract Structured Data

For cases where you need typed, structured output — product prices, article metadata, company info — pass a JSON schema to the scrape endpoint:

```
response = requests.post(
    "https://api.fastcrw.com/v1/scrape",
    headers={"Authorization": "Bearer YOUR_API_KEY"},
    json={
        "url": "https://example.com/pricing",
        "formats": ["json"],
        "jsonSchema": {
            "type": "object",
            "properties": {
                "plans": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string"},
                            "price": {"type": "number"},
                            "currency": {"type": "string"},
                            "features": {
                                "type": "array",
                                "items": {"type": "string"}
                            }
                        }
                    }
                }
            }
        },
    },
)

pricing = response.json()["data"]["json"]
for plan in pricing["plans"]:
    print(f"{plan['name']}: {plan['currency']}{plan['price']}")
    for feat in plan["features"]:
        print(f"  - {feat}")
```

The extract endpoint uses LLMs under the hood to understand page structure and pull out exactly the fields you specified. No CSS selectors. No XPath. No breaking when the site changes its layout.

## Option 3: Self-Hosted

For teams that need full data privacy or want to avoid per-request pricing, CRW runs entirely on your own infrastructure. Same API, just change the base URL.

### Docker (Recommended)

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

That's it. The API is now running at `http://localhost:3000`. All the endpoints work the same as the cloud version.

### Binary Install

```
# macOS / Linux
curl -fsSL https://fastcrw.com/install | bash

# Start the server
crw
```

CRW is a single small static binary compiled from Rust, with a tiny resident footprint — you can run it on a Raspberry Pi, a low-end VPS, or alongside your existing services without worrying about resource consumption.

### Switching Between Cloud and Self-Hosted

Your code doesn't change. Just swap the base URL:

```
# Cloud
BASE_URL = "https://api.fastcrw.com"

# Self-hosted
BASE_URL = "http://localhost:3000/api"

# Everything else stays the same
response = requests.post(
    f"{BASE_URL}/v1/search",
    json={"query": "kubernetes security best practices"},
)
```

## Building a Complete Search Agent (Python)

Here's a working Python agent that takes a question, searches the web, scrapes the top results, and generates a grounded answer. Around 50 lines of real code:

```
import requests
from openai import OpenAI

CRW_URL = "http://localhost:3000/api"  # or https://api.fastcrw.com
CRW_KEY = "your-api-key"  # omit for self-hosted
HEADERS = {"Authorization": f"Bearer {CRW_KEY}"}

openai = OpenAI()

def search_web(query: str, limit: int = 3) -> list[dict]:
    """Search the web and return top results."""
    resp = requests.post(
        f"{CRW_URL}/v1/search",
        headers=HEADERS,
        json={"query": query, "limit": limit},
    )
    return resp.json()["data"]

def scrape_url(url: str) -> str:
    """Scrape a URL and return clean markdown."""
    resp = requests.post(
        f"{CRW_URL}/v1/scrape",
        headers=HEADERS,
        json={"url": url, "formats": ["markdown"]},
    )
    data = resp.json()["data"]
    # Truncate to avoid token limits
    return data.get("markdown", "")[:4000]

def answer_question(question: str) -> str:
    """Search the web and generate a grounded answer."""
    # Step 1: Search
    results = search_web(question)
    print(f"Found {len(results)} results")

    # Step 2: Scrape top results
    sources = []
    for result in results:
        print(f"Scraping: {result['title']}")
        content = scrape_url(result["url"])
        sources.append({
            "title": result["title"],
            "url": result["url"],
            "content": content,
        })

    # Step 3: Build context and ask LLM
    context = "\n\n---\n\n".join(
        f"Source: {s['title']} ({s['url']})\n{s['content']}"
        for s in sources
    )

    response = openai.chat.completions.create(
        model="gpt-4o",
        messages=[
            {
                "role": "system",
                "content": (
                    "Answer the question using ONLY the provided sources. "
                    "Cite sources inline as [Source Title](URL). "
                    "If the sources don't contain enough info, say so."
                ),
            },
            {
                "role": "user",
                "content": f"Question: {question}\n\nSources:\n{context}",
            },
        ],
    )
    return response.choices[0].message.content

# Usage
answer = answer_question("What are the top container security tools in 2026?")
print(answer)
```

This agent takes about 3–5 seconds total: ~1 second for search, ~2 seconds for scraping 3 pages, and ~1 second for LLM generation. Every claim in the output is backed by a real, scrapable source.

## Building a RAG Pipeline with Search

For production systems that need to handle repeated queries efficiently, combine web search with a vector database. The architecture:

```
# RAG + Web Search Pipeline
#
#  ┌──────────────┐    ┌──────────────┐
#  │  User query  │───▶│ Vector DB    │──▶ If relevant docs exist,
#  └──────────────┘    │  lookup      │    return them directly
#                      └──────┬───────┘
#                             │ Cache miss
#                             ▼
#                      ┌──────────────┐
#                      │  CRW Search  │──▶ Find fresh URLs
#                      └──────┬───────┘
#                             ▼
#                      ┌──────────────┐
#                      │  CRW Scrape  │──▶ Get clean content
#                      └──────┬───────┘
#                             ▼
#                      ┌──────────────┐
#                      │  Chunk +     │──▶ Split into passages
#                      │  Embed       │    Generate embeddings
#                      └──────┬───────┘
#                             ▼
#                      ┌──────────────┐
#                      │  Store in    │──▶ Persist for future queries
#                      │  Vector DB   │
#                      └──────────────┘
```

The key insight: use web search as a **cache-miss fallback**. When the vector DB has relevant, fresh content, skip the search. When it doesn't — or when the content is stale — trigger a web search to refresh the corpus.

This gives you the best of both worlds: fast responses for common queries (vector lookup is sub-10ms) and fresh, accurate answers for novel queries (web search fills the gap). For a full implementation walkthrough, see [RAG Pipeline with CRW](/blog/rag-pipeline-with-crw).

Some practical tips for production RAG with search:

- **Set TTLs on your embeddings.** Invalidate cached content after 24–72 hours depending on your freshness requirements.
- **Store source metadata.** Keep the original URL, scrape timestamp, and page title alongside each chunk so you can provide citations and detect staleness.
- **Batch your searches.** If you have multiple sub-questions, run them in parallel. CRW handles concurrent requests without throttling on the self-hosted tier.
- **Use `scrapeOptions` in the search call** to search and scrape in one round-trip instead of two.

## Production Considerations

Moving from a prototype to production means handling failures, managing costs, and scaling reliably.

### Rate Limits

| Tier | Search | Scrape | Concurrent |
| --- | --- | --- | --- |
| Free (cloud) | 100/day | 500/day | 2 |
| Paid (cloud) | 10,000/day | 50,000/day | 50 |
| Self-hosted | Unlimited | Unlimited | Unlimited |

Self-hosting removes all rate limits. For high-volume use cases (10k+ searches/day), self-hosting typically pays for itself within the first month.

### Caching Strategy

```
import hashlib

from functools import lru_cache

# In-memory cache for development
@lru_cache(maxsize=1000)
def cached_search(query: str, limit: int = 5) -> str:
    """Cache search results by query."""
    resp = requests.post(
        f"{CRW_URL}/v1/search",
        headers=HEADERS,
        json={"query": query, "limit": limit},
    )
    return resp.text  # Return as string for lru_cache

# For production, use Redis:
# cache_key = f"search:{hashlib.md5(query.encode()).hexdigest()}"
# cached = redis.get(cache_key)
# if cached: return json.loads(cached)
# result = search(query)
# redis.setex(cache_key, 3600, json.dumps(result))  # 1 hour TTL
```

### Error Handling and Retries

```
import time

def search_with_retry(query: str, max_retries: int = 3) -> dict:
    """Search with exponential backoff."""
    for attempt in range(max_retries):
        try:
            resp = requests.post(
                f"{CRW_URL}/v1/search",
                headers=HEADERS,
                json={"query": query, "limit": 5},
                timeout=10,
            )
            resp.raise_for_status()
            return resp.json()
        except requests.exceptions.RequestException as e:
            if attempt == max_retries - 1:
                raise
            wait = 2 ** attempt  # 1s, 2s, 4s
            print(f"Retry {attempt + 1}/{max_retries} in {wait}s: {e}")
            time.sleep(wait)
```

### Cost Optimization

- **Use `scrapeOptions` in search:** One API call instead of two. Saves a round-trip and often a credit.
- **Limit search results:** Most agents only need 3–5 results. Don't fetch 20 if you'll only read 3.
- **Cache aggressively:** If users ask similar questions, cache search results for 1–4 hours.
- **Truncate scraped content:** LLMs have context limits anyway. Scraping 50,000 words when your model can only handle 8,000 wastes bandwidth and compute.
- **Self-host for volume:** At 1,000+ daily searches, a self-hosted instance on a small VPS costs less than cloud credits.

## Comparison: Build vs Buy

Can you build this pipeline yourself? Absolutely. Here's what it takes.

### DIY: Google Custom Search + BeautifulSoup + Readability

```
# DIY approach — ~60 lines minimum, plus error handling
from googleapiclient.discovery import build
from bs4 import BeautifulSoup
from readability import Document

# 1. Set up Google Custom Search (requires Google Cloud project,
#    Custom Search Engine ID, API key — billing required after 100/day)
service = build("customsearch", "v1", developerKey="GOOGLE_API_KEY")
results = service.cse().list(
    q="container security 2026",
    cx="YOUR_SEARCH_ENGINE_ID",
    num=5,
).execute()

# 2. Scrape each result (handle JS rendering, cookies, anti-bot)
for item in results.get("items", []):
    url = item["link"]
    resp = requests.get(url, headers={"User-Agent": "..."})

    # 3. Extract readable content (fails on JS-rendered pages)
    doc = Document(resp.text)
    soup = BeautifulSoup(doc.summary(), "html.parser")
    text = soup.get_text()

    # 4. Handle edge cases:
    # - JavaScript-rendered pages (need Playwright/Selenium)
    # - Cookie consent banners
    # - Anti-bot protection (Cloudflare, etc.)
    # - Rate limiting
    # - Paywalled content
    # - PDF links
    # - Malformed HTML
    # ... easily another 100+ lines
```

### CRW: 3 Lines

```
response = requests.post(
    "https://api.fastcrw.com/v1/search",
    headers={"Authorization": "Bearer YOUR_KEY"},
    json={"query": "container security 2026", "limit": 5, "scrapeOptions": {"formats": ["markdown"]}},
)
results = response.json()["data"]  # Searched, scraped, cleaned
```

The DIY approach also means maintaining your pipeline when sites change their anti-bot measures, when Google changes their API pricing, and when new edge cases surface. CRW handles all of that behind a stable API.

The trade-off is clear: DIY gives you maximum control but costs engineering time. CRW gives you a stable, fast pipeline with zero maintenance overhead.

## Performance Characteristics

CRW is a local-first engine: a single small static binary with no runtime, no headless-browser pool, and a tiny resident footprint, so search and scrape calls have low, predictable latency on commodity hardware. Rather than ship a marketing number, we publish a full latency distribution and a one-command reproduction on our public benchmark — run it on your own dataset and see the results for yourself.

CRW's search averages 880ms for 5 results. Scrape averages 595ms per page. For an agent that searches and scrapes 3 results, total wall time is under 3 seconds — fast enough for interactive use cases.

Self-hosted performance is even better since you eliminate network latency to the cloud API. On a local instance, search + scrape for 3 results typically completes in under 2 seconds.

For detailed methodology and head-to-head comparisons, see the [CRW vs Tavily Benchmark](/blog/crw-vs-tavily-search-api-benchmark) and the full [Search API benchmarks](/blog/search-api-for-ai-agents).

## When to Use Which Option

Here's a quick decision guide:

| Scenario | Best Option | Why |
| --- | --- | --- |
| IDE agent (Claude Code, Cursor) | MCP | Zero code, instant setup |
| Custom agent, low volume | Cloud REST API | No infra to manage |
| Custom agent, high volume | Self-hosted REST API | No rate limits, lowest cost |
| Enterprise / regulated | Self-hosted | Data never leaves your network |
| Prototype / hackathon | MCP or Cloud API | Fastest time to working demo |
| RAG pipeline | Self-hosted | Batch-friendly, unlimited throughput |

For most teams, the journey is: start with cloud API to validate the use case, then move to self-hosted when volume grows. The migration is a one-line base URL change.

## Getting Started

Here's your quick-start checklist:

- **5 minutes:** Install MCP server (`npx -y crw-mcp`) and try a search in Claude Code or Cursor
- **15 minutes:** Sign up at [fastcrw.com](https://fastcrw.com), get an API key, run the Python search agent from this guide
- **30 minutes:** Self-host with Docker (`docker run -p 3000:3000 ghcr.io/fastcrw/crw`) and wire it into your existing agent
- **1 hour:** Build a complete RAG pipeline with search, using the [RAG Pipeline guide](/blog/rag-pipeline-with-crw)

Every AI agent that answers questions about the real world needs web access. The difference between an agent that hallucinates and one that cites sources is a single API integration. CRW makes that integration as simple as possible — whether you use MCP, the REST API, or self-host the entire thing.

Further reading:

- [Search API for AI Agents](/blog/search-api-for-ai-agents) — Full endpoint reference and advanced parameters
- [Build a Deep Research Agent with CRW](/blog/deep-research-agent-crw) — Multi-step autonomous research with iterative search loops
- [CRW vs Tavily Benchmark](/blog/crw-vs-tavily-search-api-benchmark) — Head-to-head performance comparison
- [Website to Markdown](/blog/website-to-markdown) — Deep dive on scraping clean content for LLMs
- [Best MCP Servers for Web Scraping](/blog/best-mcp-servers-web-scraping) — MCP integration guide and comparisons
- [RAG Pipeline with CRW](/blog/rag-pipeline-with-crw) — Full vector DB + search implementation
