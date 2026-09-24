# fastCRW vs Crawl4AI: Head-to-Head Comparison (2026)

> fastCRW vs Crawl4AI head-to-head comparison (2026): Rust REST service vs Python framework for AI agent and RAG workflows. Deployment, Python integration, LangChain/LlamaIndex, cost at scale, CI/CD, and honest tradeoffs. For a 3-way comparison including Firecrawl, see The Honest Benchmark.

**Published:** 2026-03-03  
**Updated:** 2026-05-27  
**Canonical:** https://fastcrw.com/blog/crw-vs-crawl4ai

---

## Short Answer

Crawl4AI is a Python-native scraping framework with a rich feature set for AI workflows. CRW is an API-first, single-binary alternative built in Rust. If you want a framework you extend in Python — with custom extraction strategies, hooks, and deep Playwright control — Crawl4AI is the better fit. If you want a lightweight REST API you can deploy anywhere, call from any language, and run on minimal infrastructure, CRW is the better fit.

Neither tool is universally "better." They have different design philosophies: Crawl4AI is a *framework* you import into Python; CRW is a *service* you deploy and call over HTTP. Which one belongs in your stack depends on your architecture more than the feature list.

- **Best for Python-native AI workflows:** Crawl4AI
- **Best for lightweight self-hosted REST API:** CRW
- **Best for MCP/AI agent integration:** CRW (built-in)
- **Best for browser automation & screenshots:** Crawl4AI
- **Best for polyglot / microservice architectures:** CRW
- **Best for LangChain / LlamaIndex with minimal setup:** CRW (via FirecrawlLoader)

|  | CRW | Crawl4AI |
| --- | --- | --- |
| Language | Rust | Python |
| Interface | REST API (primary) | Python library / REST (secondary) |
| Latency profile | **Lower, local-first (see [benchmark](/benchmarks))** | Higher (browser-routed) |
| Idle footprint | **Tiny (single binary)** | Heavy (~hundreds of MB) |
| Docker image size | **Small single binary** | ~2 GB (Chromium bundled) |
| MCP server built-in | ✅ Yes | Community add-on |
| Firecrawl-compatible API | ✅ Yes | ❌ Own format |
| LangChain FirecrawlLoader | ✅ Works (set api_url) | ❌ Requires custom loader |
| Self-host deployment | Single binary / one Docker command | Python env + Playwright + Chromium |
| Extensibility in Python | ❌ Not applicable | ✅ Rich hooks/strategies |
| Screenshot support | ✅ (needs a Chrome-class tier) | ✅ Yes |
| REST API design | Firecrawl-compatible, REST-first | FastAPI, Python-library-first |
| License | AGPL-3.0 | Apache-2.0 |

## What Is Crawl4AI?

Crawl4AI is an open-source Python library and optional REST service for web scraping tailored to AI use cases. It provides extraction strategies (LLMExtractionStrategy, JsonCssExtractionStrategy, CosineStrategy), chunk-based output sized for LLM context windows, screenshot capture, deep-crawl orchestration, and Python hooks for customizing behavior at every stage.

At its core, Crawl4AI is designed to be *extended*. You write Python classes that implement extraction logic, filtering, and crawl graph strategies. The REST service wraps this Python core — it is not the primary interface; it is a convenience layer on top of the library. Crawl4AI has strong community traction in the Python AI ecosystem and is a mature choice if your team is Python-native and your scraping logic is complex.

## What Is CRW?

CRW is an open-source web scraping API written in Rust. It implements a Firecrawl-compatible REST interface, ships as a small single binary, and has a tiny idle footprint. It is service-oriented — you deploy it once, then call it over HTTP from any language or service.

AI agent support comes via a built-in MCP server (zero config). The Firecrawl-compatible API means existing Firecrawl integrations — including LangChain's `FirecrawlLoader`, LlamaIndex's `FirecrawlWebReader`, and the Firecrawl Python SDK — work with CRW by changing a single URL parameter.

CRW's primary weakness is extensibility: there are no Python hooks, no custom extraction strategies you write in code. What the API exposes is what you get. For teams that need deep customization, this is a real constraint. For teams that need a fast, reliable scraping endpoint they can call from anywhere, it is the right tradeoff.

[fastCRW](https://fastcrw.com) is the hosted cloud version of CRW — same engine, same API, no infrastructure to manage.

## Architecture: Framework vs. Service

This is the most fundamental difference between the two tools, and it shapes almost every other comparison.

Crawl4AI is a *framework* you import into a Python process. You instantiate an `AsyncWebCrawler`, pass an `ExtractionStrategy`, configure hooks, and run it. Your scraping logic lives in Python alongside your application logic. That is powerful if your team is Python-native and you need fine-grained control over every aspect of extraction.

CRW is a *service*. You deploy it as a separate process — a Docker container, a systemd service, or a sidecar — and call its REST API from wherever your app runs: Python, TypeScript, Go, Rust, or a shell script. Your application language does not matter. This makes CRW easier to integrate into polyglot architectures, microservice deployments, and any workflow where you do not control the calling environment.

Their failure modes also differ. If Crawl4AI crashes, it is inside your Python process. If CRW's service crashes, it is isolated. For production scraping at scale, service isolation is often preferable.

## Deployment Complexity

Getting Crawl4AI running requires Python 3.10+, Playwright, and a Chromium or Firefox installation. The official Docker image is around 2 GB because it bundles a full browser runtime. On a fresh VPS you are looking at 300–500 MB RAM at idle before you have scraped a single page. Managing Playwright browser versions across environments adds ongoing operational overhead.

CRW deploys with one command:

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

The image is a small single binary with a tiny idle footprint. The entire stack runs on a $5/month server. No Python environment, no Playwright, no Chromium to manage. If you are adding scraping as a sidecar to an existing application, this difference in operational overhead is significant.

For teams running Kubernetes, the footprint difference is even more pronounced. A Crawl4AI pod requires a minimum request of 512 MB–1 GB RAM; a CRW pod runs comfortably with a 32 MB request.

## Performance

On standard HTML pages, CRW has a lower-latency profile than Crawl4AI in our public benchmark. The difference comes primarily from how each tool processes HTML: CRW uses `lol-html`, a streaming Rust HTML parser that processes content without building a full DOM; Crawl4AI typically routes through a Playwright-rendered browser even for static HTML. See the full latency distribution and one-command repro on our [public benchmark](/benchmarks).

The performance gap narrows — and largely disappears — for JavaScript-heavy single-page applications that require a real browser. In those cases both tools spin up a browser runtime and performance converges. CRW's SPA support is also less mature than Crawl4AI's; for complex login flows, form submission, or highly dynamic content, Crawl4AI's deep Playwright integration is more robust.

For standard HTML content (documentation sites, blogs, product pages, news articles), CRW's streaming parser is substantially faster and uses far less memory per request.

## AI Agent Integration: MCP

CRW ships with a Model Context Protocol (MCP) server out of the box. Add two lines to your Claude Desktop or Cursor config and your AI agent can call `scrape`, `crawl`, and `map` directly. No extra package, no extra config.

```
{
  "mcpServers": {
    "crw": { "command": "crw", "args": ["mcp"] }
  }
}
```

From a Claude conversation, that looks like: "Scrape the API docs at docs.example.com and summarize the authentication section." The MCP server handles the HTTP call and returns structured content back to the agent.

Crawl4AI has community MCP integrations, but they require additional setup on top of the core library. For teams primarily using AI agents as the client, CRW's built-in MCP is a meaningful convenience.

## Python Integration: Calling CRW from LangChain and LlamaIndex

CRW's Firecrawl-compatible API means Python LLM frameworks with Firecrawl integrations work directly with CRW — you just change the API URL. This is one of CRW's most practical advantages for Python developers: you get CRW's lightweight footprint and REST-first design while using the same LangChain or LlamaIndex code you would write for Firecrawl.

### LangChain: FirecrawlLoader with CRW

LangChain's `FirecrawlLoader` accepts an `api_url` parameter. Point it at your CRW instance (or fastCRW) instead of Firecrawl's cloud:

```
from langchain_community.document_loaders import FirecrawlLoader

# Self-hosted CRW
loader = FirecrawlLoader(
    api_key="your-crw-api-key",
    url="https://docs.example.com",
    mode="crawl",
    api_url="https://api.fastcrw.com"  # or http://localhost:3000 for self-hosted
)
docs = loader.load()

# Or using fastCRW hosted service
loader = FirecrawlLoader(
    api_key="your-fastcrw-key",
    url="https://docs.example.com",
    mode="crawl",
    api_url="https://api.fastcrw.com"
)
docs = loader.load()

# Use in a chain
from langchain_openai import ChatOpenAI
from langchain_core.prompts import ChatPromptTemplate

llm = ChatOpenAI(model="gpt-4o-mini")
prompt = ChatPromptTemplate.from_template(
    "Summarize this documentation:

{content}"
)
chain = prompt | llm
result = chain.invoke({"content": docs[0].page_content})
```

### LlamaIndex: FirecrawlWebReader with CRW

LlamaIndex's `FirecrawlWebReader` similarly accepts a custom API URL:

```
from llama_index.readers.web import FirecrawlWebReader

reader = FirecrawlWebReader(
    api_key="your-crw-api-key",
    api_url="https://api.fastcrw.com",  # or http://localhost:3000 for self-hosted
    mode="scrape",
)
documents = reader.load_data(urls=["https://example.com/docs"])

# Build a VectorStoreIndex for RAG
from llama_index.core import VectorStoreIndex

index = VectorStoreIndex.from_documents(documents)
query_engine = index.as_query_engine()
response = query_engine.query("What authentication methods are supported?")
```

Both integrations work because CRW implements the same request/response shapes as Firecrawl's API. No monkey-patching, no custom loaders — just a URL change. You can also use the [Firecrawl Python SDK](https://github.com/mendableai/firecrawl/tree/main/apps/python-sdk) directly by passing `api_url="https://api.fastcrw.com"` (or `"http://localhost:3000"` for self-hosted) to the `FirecrawlApp` constructor.

## Running CRW from Python

You do not need a special SDK to call CRW from Python. Any HTTP client works. Here are three common patterns for scraping and retrieving clean markdown:

### requests (synchronous)

```
import requests

response = requests.post(
    "https://api.fastcrw.com/v1/scrape",  # or http://localhost:3000 for self-hosted
    headers={"Authorization": "Bearer crw_live_YOUR_API_KEY"},
    json={"url": "https://example.com", "formats": ["markdown"]}
)
markdown = response.json()["data"]["markdown"]
print(markdown)
```

### httpx (async)

```
import httpx

async def scrape(url: str) -> str:
    async with httpx.AsyncClient() as client:
        response = await client.post(
            "https://api.fastcrw.com/v1/scrape",  # or http://localhost:3000 for self-hosted
            headers={"Authorization": "Bearer crw_live_YOUR_API_KEY"},
            json={"url": url, "formats": ["markdown"]},
            timeout=30.0,
        )
        return response.json()["data"]["markdown"]

# Scrape multiple URLs concurrently
async def scrape_many(urls: list[str]) -> list[str]:
    async with httpx.AsyncClient() as client:
        tasks = [
            client.post(
                "https://api.fastcrw.com/v1/scrape",  # or http://localhost:3000 for self-hosted
                headers={"Authorization": "Bearer crw_live_YOUR_API_KEY"},
                json={"url": url, "formats": ["markdown"]},
                timeout=30.0,
            )
            for url in urls
        ]
        responses = await asyncio.gather(*tasks)
        return [r.json()["data"]["markdown"] for r in responses]

results = asyncio.run(scrape_many([
    "https://example.com/page-1",
    "https://example.com/page-2",
    "https://example.com/page-3",
]))
```

### aiohttp

```
import aiohttp

async def scrape_with_aiohttp(url: str) -> str:
    async with aiohttp.ClientSession() as session:
        async with session.post(
            "https://api.fastcrw.com/v1/scrape",  # or http://localhost:3000 for self-hosted
            headers={"Authorization": "Bearer crw_live_YOUR_API_KEY"},
            json={"url": url, "formats": ["markdown"]},
        ) as response:
            data = await response.json()
            return data["data"]["markdown"]

markdown = asyncio.run(scrape_with_aiohttp("https://example.com"))
```

All three patterns work identically whether you are calling a self-hosted CRW instance or fastCRW's hosted service — only the base URL and API key change.

## Extraction Quality Comparison

Both CRW and Crawl4AI support structured LLM-based extraction, but with different ergonomics. Here is a side-by-side for extracting structured product data from an e-commerce page.

### CRW: json format via REST

```
POST /v1/scrape
{
  "url": "https://shop.example.com/product/widget-pro",
  "formats": ["json"],
  "jsonSchema": {
    "type": "object",
    "properties": {
      "name":        { "type": "string" },
      "price":       { "type": "number" },
      "currency":    { "type": "string" },
      "in_stock":    { "type": "boolean" },
      "description": { "type": "string" },
      "rating":      { "type": "number" }
    },
    "required": ["name", "price", "in_stock"]
  }
}
```

Response (structured data at `data.json`):

```
{
  "data": {
    "json": {
      "name": "Widget Pro",
      "price": 49.99,
      "currency": "USD",
      "in_stock": true,
      "description": "Professional-grade widget for power users.",
      "rating": 4.7
    }
  }
}
```

### Crawl4AI: LLMExtractionStrategy

```
from crawl4ai import AsyncWebCrawler
from crawl4ai.extraction_strategy import LLMExtractionStrategy
from pydantic import BaseModel

class Product(BaseModel):
    name: str
    price: float
    currency: str
    in_stock: bool
    description: str
    rating: float

strategy = LLMExtractionStrategy(
    provider="openai/gpt-4o-mini",
    api_token="your-openai-key",
    schema=Product.model_json_schema(),
    extraction_type="schema",
    instruction="Extract product details from this page.",
)

async with AsyncWebCrawler() as crawler:
    result = await crawler.arun(
        url="https://shop.example.com/product/widget-pro",
        extraction_strategy=strategy,
    )
    product = Product.model_validate_json(result.extracted_content)
```

### Tradeoffs

CRW's approach is simpler to call over REST — any language, no Python dependencies. Crawl4AI's approach gives you Pydantic validation, Python type safety, and the ability to write custom post-processing logic inline. For Python teams building complex extraction pipelines, Crawl4AI's in-process approach can be more ergonomic. For polyglot teams or microservice architectures, CRW's REST-based extraction is easier to integrate.

One honest limitation of CRW: extraction quality depends entirely on the LLM you configure, and there are no built-in fallback strategies or chunking controls. Crawl4AI gives you more knobs for handling large pages, controlling chunk overlap, and combining multiple extraction strategies.

## Cost Comparison at Scale

RAM footprint directly determines how many concurrent scraping workers you can run on a given instance. CRW's tiny idle footprint lets it pack many workers onto small VMs; Crawl4AI's browser-per-worker model needs much larger instances. The table below uses rough DigitalOcean Droplet pricing and assumes each CRW worker uses a small amount of memory under load and each Crawl4AI worker carries one Playwright browser instance.

| Concurrent workers | CRW RAM needed | CRW server (DO est.) | Crawl4AI RAM needed | Crawl4AI server (DO est.) |
| --- | --- | --- | --- | --- |
| 10 | ~150 MB | $6/mo (1 GB Droplet) | ~3.5 GB | $24/mo (4 GB Droplet) |
| 50 | ~750 MB | $12/mo (2 GB Droplet) | ~17.5 GB | $96/mo (16 GB Droplet, 2× nodes) |
| 100 | ~1.5 GB | $18/mo (2 GB Droplet + headroom) | ~35 GB | $192/mo (32 GB Droplet or 3–4× nodes) |

These are rough estimates. Real-world numbers depend on page complexity, concurrency patterns, and whether Crawl4AI is running in browser-per-request or shared-browser mode. But the order-of-magnitude difference is real: CRW's Rust parser does not need a browser process per worker, so the cost curve is fundamentally different. For a team scraping 10,000 pages/day, the infrastructure cost difference can easily exceed $100–200/month.

## Docker Image Size in CI/CD

The single-binary vs 2 GB size difference is not just about production memory — it materially affects CI/CD pipeline speed. On a typical GitHub Actions runner with a warm cache, pulling the small CRW image is near-instant. Pulling Crawl4AI's 2 GB image on a cold runner can take far longer, and even with Docker layer caching, layer validation adds overhead. For ephemeral CI environments that provision fresh runners per job, this is a real cost in both time and money.

Here is a GitHub Actions workflow that starts CRW as a service container for integration tests:

```
jobs:
  integration-test:
    runs-on: ubuntu-latest
    services:
      crw:
        image: ghcr.io/fastcrw/crw:latest
        ports:
          - 3000:3000
    steps:
      - uses: actions/checkout@v4

      - name: Wait for CRW to be ready
        run: |
          for i in 1 2 3 4 5 6 7 8 9 10; do
            curl -sf http://localhost:3000/health && break
            sleep 1
          done

      - name: Run scraping integration tests
        env:
          CRW_API_URL: http://localhost:3000
          CRW_API_KEY: test-key
        run: npm test
```

With CRW's small single-binary image, the service starts quickly on any CI runner. With a 2 GB image, you end up investing in cache warm-up strategies just to keep pipeline times acceptable. For teams running hundreds of CI jobs per day, that overhead adds up.

## Crawl4AI's REST Mode vs CRW

Crawl4AI does have a REST mode — a FastAPI server you can run with `docker run unclecode/crawl4ai`. This is worth addressing directly, because it is a fair counterargument to "CRW is better for REST-based workflows."

- **Crawl4AI REST mode exists and works.** You can call it from any language, and it exposes most of the library's functionality. For teams already committed to Crawl4AI, the REST mode is a reasonable polyglot interface.
- **The Docker image is still 2 GB.** REST mode does not change the underlying image size — you are still pulling Chromium and Playwright. The operational overhead is the same whether you use Crawl4AI as a library or a REST service.
- **The API is not Firecrawl-compatible.** Crawl4AI uses its own request/response format. LangChain's `FirecrawlLoader`, LlamaIndex's `FirecrawlWebReader`, and the Firecrawl SDK do not work with Crawl4AI REST out of the box. You would need a custom integration layer.
- **REST is secondary to the Python library.** Crawl4AI's documentation, examples, and community content primarily cover the Python API. The REST interface is less documented and some Python-only features (custom hooks, strategy composition) are not fully exposed over HTTP.
- **CRW is REST-first by design.** Every CRW feature — scrape, crawl, map, extract, MCP — is accessible over HTTP with a clean, versioned, documented API. The REST interface is not a wrapper around a library; it *is* the interface.

If you are choosing between the two specifically for a REST-based workflow, CRW's lighter footprint, Firecrawl compatibility, and REST-first design make it the more natural fit. If you need Crawl4AI's Python-specific features — custom extraction strategies, browser hooks, in-process data pipelines — the Python library is the right tool regardless of REST mode's existence.

## Where Crawl4AI Is the Better Fit

- **Python-native extensibility:** Custom hooks, extraction strategies, and event-driven control are core to Crawl4AI's design. Writing a `JsonCssExtractionStrategy` or a custom `ChunkingStrategy` in Python is something CRW simply cannot match.
- **Screenshots and media:** both capture full-page screenshots; CRW needs a Chrome-class renderer tier for it (LightPanda cannot capture). Crawl4AI additionally handles media downloads, which CRW does not.
- **Complex browser automation:** Full Playwright control for login flows, cookie handling, form submission, and single-page app interactions where CRW's SPA support is less mature.
- **Deep crawl orchestration:** Crawl4AI's crawl graph and strategy system is more configurable for complex multi-step crawl jobs with conditional logic.
- **Python AI ecosystem integration depth:** If you want tight in-process integration with LangChain or LlamaIndex — shared memory, direct object passing, no HTTP overhead — Crawl4AI's library model is a better fit.

## Where CRW Is the Better Fit

- **Operational simplicity:** One small binary, one Docker command, tiny idle footprint. No Python environment, no browser runtime to manage.
- **Language agnostic:** REST API means any language can call it — Python, TypeScript, Go, Rust, bash. No library install required in the calling service.
- **Firecrawl compatibility:** Drop-in replacement for Firecrawl's API. LangChain, LlamaIndex, and the Firecrawl SDK work without modification.
- **MCP integration:** Built-in, zero-config MCP server for AI agents.
- **Cost at scale:** Lower memory means more concurrent scrapes per dollar on self-hosted infrastructure.
- **CI/CD speed:** Small single-binary image pulls in seconds; no cache warm-up strategy needed.
- **Performance on static HTML:** Streaming Rust parser avoids browser-render overhead for standard HTML content, with lower latency in our [public benchmark](/benchmarks).

## Who Should Use Which

- **Use Crawl4AI if:** you are building a Python-native scraping workflow that needs custom extraction strategies, screenshots, deep Playwright control, or tight in-process integration with your Python application.
- **Use CRW if:** you want a lightweight, language-agnostic scraping API that you can self-host cheaply, call from any language, and connect to AI agents via MCP — with minimal operational overhead.
- **Use both if:** your architecture has a Python data pipeline (Crawl4AI) and a separate microservice or non-Python service that needs scraping (CRW). They serve different roles and can coexist.
- **Use fastCRW if:** you want CRW's engine hosted for you without managing infrastructure — with proxy networks, auto-scaling, and a free tier.

Also see: [Best Self-Hosted Web Scraping Tools for AI Agents and RAG](/blog/best-self-hosted-scrapers) for a broader comparison including Firecrawl.

## Getting Started with CRW

### Open-Source Path — Self-Host for Free

CRW is AGPL-3.0 licensed. Run it on your own server at no cost:

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

Or install the binary directly — CRW ships as a single statically-linked binary with no runtime dependencies. Review the install script before running it:

```
# Linux / macOS — download and inspect first
curl -fsSL https://fastcrw.com/install -o install.sh
cat install.sh          # review before running
sh install.sh

# Verify and run
crw --version
crw serve
```

[GitHub repository](https://github.com/fastcrw/crw) · [Documentation](https://us.github.io/crw)

### Hosted Path — Use fastCRW

Don't want to manage infrastructure? [fastCRW](https://fastcrw.com) runs CRW's engine for you — with proxy networks, auto-scaling, and a free tier of 1000 credits. No credit card required to start.

## Want a Broader Three-Way Comparison?

This page is the focused 2-way head-to-head. If you also want to weigh **Firecrawl** in the mix — including the public benchmark numbers, pricing, and per-tool tradeoffs across all three — read [Firecrawl vs Crawl4AI vs fastCRW: The Honest Benchmark (2026)](/blog/firecrawl-vs-crawl4ai-vs-crw). The reproducible `diagnose_3way.py` script lives on [/benchmarks](/benchmarks).

## FAQ

### Is CRW a drop-in replacement for Crawl4AI?

No — the two tools have different API designs. Crawl4AI uses a Python library interface with its own endpoint format, while CRW exposes a Firecrawl-compatible REST API. Switching means updating your API calls, but the core concepts (scrape, crawl, extract) map directly. The deeper difference is architectural: CRW is a service you call over HTTP, and Crawl4AI is a library you import into a Python process.

### Which is better for RAG pipelines: CRW or Crawl4AI?

Both work well for RAG, and the choice depends on your stack. If your pipeline is Python-native and you want to call the scraper directly from LangChain or LlamaIndex with in-process integration, Crawl4AI fits more naturally. If you want a language-agnostic scraping service any pipeline component can call over HTTP, CRW is the better fit.

### Does CRW have Python support?

CRW's API is language-agnostic — you can call it from Python with any HTTP client such as requests, httpx, or aiohttp. The Firecrawl Python SDK also works unchanged because the API shapes are identical, and LangChain's FirecrawlLoader and LlamaIndex's FirecrawlWebReader both work by setting the api_url parameter to your CRW instance. There is no Python-native library to install.

### Can I use Crawl4AI and CRW together?

Yes, and for some architectures that is the right answer. Use Crawl4AI for complex Python extraction workflows that need custom strategies, browser hooks, or tight in-process pipeline integration. Use CRW for lightweight REST scraping in polyglot services, AI-agent integrations via MCP, or anywhere you want a low-overhead HTTP endpoint. They solve different problems and can coexist in the same system.

### How does Crawl4AI's 2 GB image compare to CRW's?

Crawl4AI's image bundles Python, Playwright, and a full Chromium browser, which is what drives its roughly 2 GB size. CRW ships instead as a single binary, a ~10 MB download, in one container with no external runtime dependencies — versus a Firecrawl-style self-host that needs around five containers. For teams where container pull time matters in CI/CD or auto-scaling, this is a real operational difference.

### How do CRW and Crawl4AI compare on scrape accuracy and speed?

On Firecrawl's public 1,000-URL scrape-content-dataset-v1 (819 labeled URLs, harness diagnose_3way.py, run 2026-05-08), CRW reached the highest truth-recall of the three tools at 63.74% (522/819), ahead of Crawl4AI's 59.95% (491) and Firecrawl's 56.04% (459). Median latency was effectively tied — CRW 1914 ms versus Crawl4AI 1916 ms. fastCRW's recall mode recovers 34 URLs that neither Crawl4AI nor Firecrawl can reach — 70% more unique recoveries than the other two combined.
