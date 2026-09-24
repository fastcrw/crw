# BeautifulSoup vs Scrapy vs CRW: Python Web Scraping Compared

> BeautifulSoup vs Scrapy vs CRW for Python web scraping — compare library, framework, and API approaches with code examples and benchmarks.

**Published:** 2026-04-28  
**Updated:** 2026-05-23  
**Canonical:** https://fastcrw.com/blog/beautifulsoup-vs-scrapy-vs-crw

---

## Short Answer

If you need quick, one-off scraping scripts with full control over parsing — **BeautifulSoup** is the simplest starting point. If you need a production crawling framework with built-in concurrency, pipelines, and middleware — **Scrapy** is the right tool. If you need a ready-to-use scraping API that returns clean markdown and structured data without writing parsers — **CRW** is the better fit.

- **Better fit for quick scripts and learning:** BeautifulSoup (minimal boilerplate, intuitive API)
- **Better fit for production crawling pipelines:** Scrapy (built-in concurrency, item pipelines, middleware)
- **Better fit for AI/RAG data pipelines:** CRW (clean markdown output, structured extraction, MCP)
- **Better fit for teams without Python expertise:** CRW (language-agnostic REST API)

|  | BeautifulSoup | Scrapy | CRW |
| --- | --- | --- | --- |
| Type | Parsing library | Crawling framework | Scraping API |
| Language | Python | Python | Any (REST API) |
| Learning curve | Low | Medium-High | Low |
| Concurrency | Manual (threading/async) | Built-in (Twisted) | Built-in (Rust async) |
| HTTP handling | None (needs requests/httpx) | Built-in | Built-in |
| JavaScript rendering | No | Via Splash/Playwright | Via LightPanda |
| Markdown output | Manual conversion | Manual conversion | ✅ Built-in |
| Structured extraction | Manual (CSS/XPath) | Manual (CSS/XPath) | ✅ JSON schema via API |
| MCP server | No | No | ✅ Built-in |
| Deployment | Script on any server | Scrapyd, Scrapy Cloud | Single Docker container |
| Footprint at idle | Python process | Twisted reactor | **Small single static binary** |
| License | MIT | BSD | AGPL-3.0 |

## What Is BeautifulSoup?

BeautifulSoup (BS4) is a Python library for parsing HTML and XML documents. It's not a scraper — it's a parser. You pair it with an HTTP library like `requests` or `httpx` to fetch pages, then use BeautifulSoup to navigate and extract data from the HTML tree.

Its strength is simplicity. The API is intuitive — `soup.find()`, `soup.select()`, `soup.get_text()` — and it handles malformed HTML gracefully. For quick scripts, data exploration, and learning web scraping, it's hard to beat. The tradeoff is that everything beyond parsing is your responsibility: HTTP requests, concurrency, rate limiting, error handling, retries, data storage.

## What Is Scrapy?

Scrapy is a full-featured web crawling framework for Python. Unlike BeautifulSoup, Scrapy handles the entire scraping workflow: making HTTP requests, following links, parsing responses, processing extracted data through pipelines, and exporting results. It runs on Twisted (an async networking framework), giving it built-in concurrency without threading.

Scrapy's strength is production-grade crawling. It includes middleware for user-agent rotation, proxy support, request deduplication, rate limiting, and retry logic. The tradeoff is complexity — Scrapy has a significant learning curve, and simple scraping tasks require more boilerplate than a BeautifulSoup script.

## What Is CRW?

CRW is an open-source web scraping API written in Rust. It takes a fundamentally different approach from both BeautifulSoup and Scrapy: instead of writing parsing code, you make API calls and get structured results back. Send a URL, get clean markdown, HTML, links, or structured JSON. No CSS selectors, no XPath, no parser code.

CRW uses lol-html (Cloudflare's streaming HTML parser) for fast, low-memory processing. It exposes a Firecrawl-compatible REST API, works with the Firecrawl Python SDK, and includes a built-in MCP server for AI agents. Because it's a REST API, it's language-agnostic — you can call it from Python, JavaScript, Go, or curl.

## Code Comparison: Extracting Article Data

Let's compare what it takes to extract the title, author, date, and content from a blog post using all three tools.

### BeautifulSoup

```
import requests
from bs4 import BeautifulSoup

response = requests.get("https://example.com/blog/my-article")
soup = BeautifulSoup(response.text, "html.parser")

article = {
    "title": soup.find("h1").get_text(strip=True),
    "author": soup.find("span", class_="author").get_text(strip=True),
    "date": soup.find("time")["datetime"],
    "content": soup.find("article").get_text(strip=True),
}

print(article)
```

Simple and readable. But this breaks if the HTML structure changes — different class names, different element hierarchy. You also need to handle HTTP errors, timeouts, and encoding yourself.

### Scrapy

```
import scrapy

class ArticleSpider(scrapy.Spider):
    name = "article"
    start_urls = ["https://example.com/blog/my-article"]

    def parse(self, response):
        yield {
            "title": response.css("h1::text").get(),
            "author": response.css("span.author::text").get(),
            "date": response.css("time::attr(datetime)").get(),
            "content": response.css("article::text").getall(),
        }

# Run with: scrapy runspider article_spider.py -o articles.json
```

More structure, but you get Scrapy's full pipeline for free: retries, rate limiting, concurrent requests, output formatting. The overhead only pays off when you're crawling many pages or need production reliability.

### CRW (Python SDK)

```
from firecrawl import FirecrawlApp

app = FirecrawlApp(
    api_key="your-key",
    api_url="http://localhost:3000",  # self-hosted CRW
)

# Option 1: Get clean markdown (great for AI/LLM consumption)
result = app.scrape_url(
    "https://example.com/blog/my-article",
    params={"formats": ["markdown"]},
)
print(result["markdown"])

# Option 2: Get structured data via JSON schema
result = app.scrape_url(
    "https://example.com/blog/my-article",
    params={
        "formats": ["json"],
        "jsonSchema": {
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "author": {"type": "string"},
                "date": {"type": "string"},
                "content": {"type": "string"},
            },
            "required": ["title", "content"],
        },
    },
)
print(result["json"])
```

No CSS selectors. No parsing code. The markdown output is immediately usable for AI pipelines. The structured extraction works even if the page redesigns — the schema describes what you want, not where to find it in the DOM.

## Crawling Multiple Pages

Scraping a single page is straightforward with any tool. The real differences emerge when you need to crawl an entire site.

### BeautifulSoup: You Build Everything

```
import requests
from bs4 import BeautifulSoup
from urllib.parse import urljoin

visited = set()
to_visit = ["https://example.com"]

while to_visit:
    url = to_visit.pop(0)
    if url in visited:
        continue
    visited.add(url)

    try:
        response = requests.get(url, timeout=10)
        soup = BeautifulSoup(response.text, "html.parser")

        # Extract data
        title = soup.find("h1")
        if title:
            print(f"{url}: {title.get_text(strip=True)}")

        # Find links to follow
        for link in soup.find_all("a", href=True):
            next_url = urljoin(url, link["href"])
            if next_url.startswith("https://example.com") and next_url not in visited:
                to_visit.append(next_url)

        time.sleep(1)  # Rate limiting
    except Exception as e:
        print(f"Error: {url} - {e}")
```

This works, but it's missing concurrency, proper error handling, retry logic, URL deduplication, robots.txt compliance, and depth limiting. Adding all of that turns a 20-line script into a 200-line project.

### Scrapy: Framework Handles It

```
import scrapy

class SiteSpider(scrapy.Spider):
    name = "site"
    start_urls = ["https://example.com"]
    allowed_domains = ["example.com"]

    custom_settings = {
        "DEPTH_LIMIT": 3,
        "CONCURRENT_REQUESTS": 16,
        "DOWNLOAD_DELAY": 0.5,
    }

    def parse(self, response):
        yield {
            "url": response.url,
            "title": response.css("h1::text").get(),
            "content": response.css("article::text").getall(),
        }

        for link in response.css("a::attr(href)").getall():
            yield response.follow(link, self.parse)
```

Scrapy handles concurrency, deduplication, rate limiting, and depth control out of the box. This is where it shines — production crawling with minimal code.

### CRW: One API Call

```
from firecrawl import FirecrawlApp

app = FirecrawlApp(
    api_key="your-key",
    api_url="http://localhost:3000",
)

# Crawl the entire site — CRW handles concurrency, deduplication, link discovery
result = app.crawl_url(
    "https://example.com",
    params={
        "limit": 100,
        "scrapeOptions": {"formats": ["markdown"]},
    },
)

for page in result["data"]:
    print(f"{page['metadata']['url']}: {len(page['markdown'])} chars")
```

CRW's crawl endpoint handles the entire crawling workflow: link discovery, deduplication, concurrency, and content extraction. The result is an array of pages with clean markdown — ready for ingestion into a vector database or RAG pipeline.

## AI Pipeline Integration

This is where the three approaches diverge most significantly. Building an AI data pipeline — feeding web content to LLMs, building RAG systems, or powering AI agents — has different requirements than traditional scraping.

### What AI pipelines need from a scraper

- **Clean text output:** LLMs consume text, not HTML. You need content stripped of navigation, ads, and boilerplate.
- **Structured data:** For RAG, you often need typed fields (title, date, author, sections) rather than raw text.
- **Low latency:** AI agents that fetch web context in real-time need sub-second responses.
- **MCP compatibility:** AI agents using Model Context Protocol need tools they can call directly.

### BeautifulSoup for AI pipelines

Possible but labor-intensive. You need to write custom code to strip boilerplate, convert HTML to markdown, and handle every edge case in content extraction. Libraries like `markdownify` or `html2text` help, but each site's HTML structure is different.

### Scrapy for AI pipelines

Scrapy's item pipeline architecture can work for AI data ingestion, but you're still writing custom parsing logic per site. There are community projects for Scrapy-to-RAG integration, but it's not built in.

### CRW for AI pipelines

This is CRW's primary design goal. The markdown output is clean and ready for LLM consumption. Structured extraction via JSON schema means you define what you want semantically, not structurally. The built-in MCP server means AI agents can scrape the web without custom integration code.

```
# Feed web content directly into an AI pipeline
from firecrawl import FirecrawlApp
from openai import OpenAI

crw = FirecrawlApp(api_key="key", api_url="http://localhost:3000")
llm = OpenAI()

# Scrape documentation page
page = crw.scrape_url("https://docs.example.com/api", params={"formats": ["markdown"]})

# Feed directly to LLM — no HTML parsing, no cleanup
response = llm.chat.completions.create(
    model="gpt-4o",
    messages=[
        {"role": "system", "content": "Answer based on the documentation provided."},
        {"role": "user", "content": f"Documentation:\n{page['markdown']}\n\nQuestion: How do I authenticate?"},
    ],
)
print(response.choices[0].message.content)
```

## Deployment and Operations

How you deploy and run your scraper matters as much as which tool you choose.

### BeautifulSoup

It's a library, so deployment is "deploy your Python application." This could be a cron job, a Lambda function, a Docker container running your script. You manage everything: dependencies, concurrency, monitoring, error handling. For one-off scripts, this flexibility is an advantage. For production services, it's overhead.

### Scrapy

Scrapy has dedicated deployment options: Scrapyd (a deployment daemon), Scrapy Cloud (managed hosting by Zyte), or standard Docker containers. Scrapy Cloud is the easiest path to production, but it's a paid service. Self-hosting Scrapy requires managing Twisted's event loop, which can be tricky in containerized environments.

### CRW

One Docker command:

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

It's a single small static binary in a lean container that runs comfortably on a $5 VPS. There's nothing to configure, no dependencies to manage, no runtime to set up. For teams that want scraping as infrastructure rather than scraping as code, this operational simplicity is significant. See our [post on low-memory scraping](/blog/low-memory-scraping) for why this matters at scale.

## Error Handling and Reliability

### BeautifulSoup

No built-in error handling for HTTP requests (that's your HTTP library's job), no retry logic, no timeout management. You write all of it. For scripts that run once, this is fine. For production scrapers, you'll end up reimplementing what Scrapy gives you for free.

### Scrapy

Excellent built-in reliability: automatic retries with configurable retry counts and HTTP status codes, download timeouts, concurrent request limits, and detailed logging. Scrapy's middleware architecture lets you add custom error handling without touching core logic. This is Scrapy's biggest advantage for production use.

### CRW

CRW handles retries, timeouts, and error responses at the API level. You get structured error responses with HTTP status codes. Because it's a REST API, your application's error handling is standard HTTP error handling — something every developer already knows. For crawl operations, CRW tracks per-URL status and reports failures without stopping the entire crawl.

## When to Use Each: Decision Framework

### Use BeautifulSoup when:

- You're writing a quick, one-off scraping script
- You need full control over HTTP requests and parsing logic
- You're learning web scraping and want to understand the fundamentals
- The task is simple: fetch one page, extract specific elements
- You need to parse local HTML files (no HTTP involved)

### Use Scrapy when:

- You're building a production crawling pipeline that needs to run reliably
- You need to crawl thousands of pages with built-in concurrency
- You want middleware for proxy rotation, user-agent cycling, and retry logic
- You need item pipelines for data processing and storage
- Your team has Python expertise and wants full framework control

### Use CRW when:

- You're building AI/RAG pipelines that need clean markdown from web pages
- You want structured data extraction without writing CSS selectors or XPath
- You need a scraping API that any language can call (not just Python)
- You want AI agent integration via MCP
- You need lightweight deployment on constrained infrastructure
- You don't want to write and maintain scraping code — you want scraping as a service

## The Complementary Approach

These tools aren't mutually exclusive. A practical setup might use CRW as the default scraping backend (handling 90% of pages via its API), with Scrapy handling complex crawling jobs that need custom middleware, and BeautifulSoup for quick ad-hoc parsing tasks during development.

Because CRW exposes a standard REST API, it integrates naturally with any Python workflow — including Scrapy pipelines. You can even use CRW as a data source inside a Scrapy spider, getting the best of both worlds: Scrapy's crawl orchestration with CRW's clean content extraction.

## Try CRW

### Open-Source Path — Self-Host for Free

CRW is AGPL-3.0 licensed. Run it on your own infrastructure at zero cost:

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

[View the source on GitHub](https://github.com/fastcrw/crw) · [Read the docs](https://us.github.io/crw)

### Hosted Path — Use fastCRW

Don't want to manage servers? [fastCRW](https://fastcrw.com) is the managed cloud version — same Firecrawl-compatible API, same low-latency engine, with infrastructure and scaling handled for you. Start with 1000 free credits, no credit card required.

## Further Reading

- [CRW vs Firecrawl: A Practical Comparison](/blog/firecrawl-vs-crawl4ai-vs-crw)
- [CRW Benchmark: 1,000 URLs, Real Results](/blog/benchmark-crw)
- [Why Low Memory Matters for Web Scraping](/blog/low-memory-scraping)
- [Running CRW on a $5 VPS](/blog/crw-on-5-dollar-vps)

## FAQ

### Is BeautifulSoup or Scrapy faster for web scraping?

Scrapy is faster for crawling many pages because it has built-in concurrency via the Twisted reactor, while BeautifulSoup is a parser with no HTTP handling and runs single-threaded unless you add threading or async yourself. For a single page the difference is negligible. If raw throughput matters, CRW's Rust async engine handles concurrency at the API level so you do not manage it at all.

### Can BeautifulSoup or Scrapy render JavaScript?

BeautifulSoup cannot render JavaScript at all — it only parses the HTML you hand it. Scrapy needs an add-on such as Splash or Playwright to execute JavaScript. CRW renders JavaScript-heavy pages through its LightPanda renderer by default, with no extra setup.

### When should I use a scraping API instead of writing a Python scraper?

Use a scraping API like CRW when you want clean markdown or structured JSON without writing CSS selectors or XPath, when your team is not Python-centric (the REST API works from any language), or when you are feeding an AI/RAG pipeline that needs LLM-ready text. Stick with BeautifulSoup or Scrapy when you need full control over parsing logic or custom crawl middleware.

### How accurate is CRW at extracting page content?

On Firecrawl's public 1,000-URL scrape-content-dataset-v1 (819 labeled URLs, harness diagnose_3way.py, run 2026-05-08), fastCRW reached 63.74% truth-recall — the highest of the three tools tested, ahead of Crawl4AI at 59.95% and Firecrawl at 56.04%. It also posted 91.8% scrape-success of reachable URLs with 0 thrown errors across 3,000 requests. BeautifulSoup and Scrapy have no comparable accuracy figure because content cleanup is your own parsing code.

### Is CRW free, and how does it compare to Scrapy Cloud?

CRW's engine is open source under AGPL-3.0, so self-hosting it costs nothing beyond your own server — it runs as a single small static binary in one Docker container, comfortably on a $5 VPS. The managed fastCRW cloud has a Free plan with 1000 one-time lifetime credits and paid tiers from $13/mo (Hobby, launch pricing through 2026-06-01). Scrapy itself is free (BSD), but Scrapy Cloud, Zyte's managed hosting, is a paid service.

### Can I use CRW together with Scrapy and BeautifulSoup?

Yes — these tools are not mutually exclusive. A practical setup uses CRW as the default scraping backend for most pages, Scrapy for complex crawls that need custom middleware, and BeautifulSoup for quick ad-hoc parsing during development. Because CRW exposes a standard REST API, you can even call it as a data source inside a Scrapy spider.
