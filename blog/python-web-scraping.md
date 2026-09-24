# Python Web Scraping: The Complete Guide with CRW (2026)

> Python web scraping guide — requests, Beautiful Soup, Scrapy, and the modern API approach with CRW. Code examples included.

**Published:** 2026-03-29  
**Updated:** 2026-03-29  
**Canonical:** https://fastcrw.com/blog/python-web-scraping

---

## Overview

Python is the most popular language for web scraping — and for good reason. Its ecosystem includes battle-tested libraries for every scraping approach, from simple HTTP requests to full browser automation. This guide covers all the major approaches, then shows how CRW's API simplifies everything.

We'll cover: **requests + Beautiful Soup** (the classic), **Scrapy** (the framework), **Playwright** (browser automation), and **CRW** (the modern API approach). By the end, you'll know which tool to reach for in every situation.

## The Traditional Approach: Requests + Beautiful Soup

The most common starting point for Python scraping. `requests` fetches the HTML, and `BeautifulSoup` parses it:

```
import requests
from bs4 import BeautifulSoup

url = "https://example.com/blog"
response = requests.get(url, headers={"User-Agent": "Mozilla/5.0"})
soup = BeautifulSoup(response.text, "html.parser")

# Extract article titles
articles = []
for article in soup.select("article h2 a"):
    articles.append({
        "title": article.get_text(strip=True),
        "url": article["href"],
    })

print(f"Found {len(articles)} articles")
```

### Pros and Cons

- **Pros:** Simple, lightweight, great for static HTML pages, huge community
- **Cons:** Doesn't execute JavaScript, requires manual CSS/XPath selectors, breaks when site structure changes, no built-in rate limiting or retry logic

## The Framework Approach: Scrapy

Scrapy is a full-featured scraping framework with built-in concurrency, rate limiting, and pipeline processing:

```
import scrapy

class BlogSpider(scrapy.Spider):
    name = "blog"
    start_urls = ["https://example.com/blog"]

    def parse(self, response):
        for article in response.css("article"):
            yield {
                "title": article.css("h2 a::text").get(),
                "url": response.urljoin(article.css("h2 a::attr(href)").get()),
                "summary": article.css("p::text").get(),
            }

        # Follow pagination
        next_page = response.css("a.next-page::attr(href)").get()
        if next_page:
            yield response.follow(next_page, self.parse)
```

### Pros and Cons

- **Pros:** Built-in concurrency, middleware system, export pipelines, excellent for large-scale crawls
- **Cons:** Steep learning curve, heavyweight for simple tasks, still no JavaScript rendering (without plugins), requires maintaining spider code as sites change

## Browser Automation: Playwright

For JavaScript-heavy single-page applications, you need a real browser. Playwright automates Chromium, Firefox, or WebKit:

```
from playwright.sync_api import sync_playwright

with sync_playwright() as p:
    browser = p.chromium.launch(headless=True)
    page = browser.new_page()
    page.goto("https://example.com/app")

    # Wait for dynamic content to load
    page.wait_for_selector(".product-card")

    products = page.query_selector_all(".product-card")
    for product in products:
        name = product.query_selector("h3").inner_text()
        price = product.query_selector(".price").inner_text()
        print(f"{name}: {price}")

    browser.close()
```

### Pros and Cons

- **Pros:** Handles JavaScript-rendered content, can interact with pages (click, scroll, fill forms), works with SPAs
- **Cons:** Slow (launches a full browser), high memory usage, complex setup for headless environments, fragile selectors

## The Modern Approach: CRW API

CRW is a Rust-based web scraping API that handles all the hard parts — JavaScript rendering, content extraction, bot detection — and returns clean markdown or structured data. You make an HTTP request; CRW does the rest.

This approach eliminates the need to maintain CSS selectors, handle browser automation, or build retry logic. Let's see how it works in Python.

### Setup

You can use CRW with a simple `requests` call or with the Firecrawl Python SDK (CRW is Firecrawl-compatible):

```
# Option 1: Direct HTTP (no extra dependencies)

# Option 2: Firecrawl Python SDK
# pip install firecrawl-py
from firecrawl import FirecrawlApp
```

### Scrape a Single Page

The `/v1/scrape` endpoint fetches a single URL and returns clean markdown:

```
import requests

CRW_URL = "https://api.fastcrw.com"  # or http://localhost:3000 for self-hosted
API_KEY = "crw_live_YOUR_API_KEY"

def scrape_page(url: str) -> dict:
    response = requests.post(
        f"{CRW_URL}/v1/scrape",
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {API_KEY}",
        },
        json={"url": url, "formats": ["markdown"]},
    )
    data = response.json()
    if not data.get("success"):
        raise Exception(f"Scrape failed: {data.get('error')}")
    return {
        "markdown": data["data"]["markdown"],
        "title": data["data"]["metadata"].get("title", ""),
        "source_url": data["data"]["metadata"].get("sourceURL", url),
    }

# Usage
result = scrape_page("https://docs.python.org/3/tutorial/index.html")
print(f"Title: {result['title']}")
print(f"Content length: {len(result['markdown'])} chars")
```

### Using the Firecrawl Python SDK

Since CRW is Firecrawl-compatible, you can use the official Firecrawl SDK by pointing it at your CRW instance:

```
from firecrawl import FirecrawlApp

# Point SDK at CRW (self-hosted or fastCRW cloud)
app = FirecrawlApp(
    api_key="crw_live_YOUR_API_KEY",
    api_url="https://api.fastcrw.com",  # or http://localhost:3000
)

# Scrape a single page
result = app.scrape_url("https://docs.python.org/3/tutorial/index.html")
print(result["markdown"][:500])
```

### Crawl an Entire Website

The `/v1/crawl` endpoint discovers and scrapes all pages on a site:

```
import time

def crawl_site(url: str, limit: int = 50) -> list[dict]:
    # Start the crawl
    start_response = requests.post(
        f"{CRW_URL}/v1/crawl",
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {API_KEY}",
        },
        json={
            "url": url,
            "limit": limit,
            "scrapeOptions": {"formats": ["markdown"]},
        },
    )
    job_id = start_response.json()["id"]
    print(f"Crawl started: {job_id}")

    # Poll for completion
    while True:
        time.sleep(2)
        status_response = requests.get(
            f"{CRW_URL}/v1/crawl/{job_id}",
            headers={"Authorization": f"Bearer {API_KEY}"},
        )
        status = status_response.json()

        if status["status"] == "completed":
            print(f"Crawl complete: {len(status['data'])} pages")
            return status["data"]
        elif status["status"] == "failed":
            raise Exception("Crawl failed")
        else:
            scraped = len(status.get("data", []))
            print(f"In progress: {scraped} pages scraped...")

# Usage
pages = crawl_site("https://docs.python.org/3/tutorial/", limit=20)
for page in pages:
    title = page.get("metadata", {}).get("title", "Untitled")
    content_len = len(page.get("markdown", ""))
    print(f"  {title} ({content_len} chars)")
```

### Using the SDK for Crawling

```
from firecrawl import FirecrawlApp

app = FirecrawlApp(api_key="crw_live_YOUR_API_KEY", api_url="https://api.fastcrw.com")

# Crawl with the SDK (handles polling automatically)
result = app.crawl_url(
    "https://docs.python.org/3/tutorial/",
    params={"limit": 20, "scrapeOptions": {"formats": ["markdown"]}},
)

for page in result["data"]:
    print(page["metadata"]["title"])
```

### Discover URLs with Map

Use `/v1/map` to discover all URLs on a site without fetching content — useful for planning targeted scrapes:

```
def discover_urls(url: str) -> list[str]:
    response = requests.post(
        f"{CRW_URL}/v1/map",
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {API_KEY}",
        },
        json={"url": url},
    )
    data = response.json()
    return data.get("links", [])

# Discover, filter, then scrape only what you need
all_urls = discover_urls("https://docs.python.org/3/")
tutorial_urls = [u for u in all_urls if "/tutorial/" in u]
print(f"Found {len(tutorial_urls)} tutorial pages out of {len(all_urls)} total")
```

### Extract Structured Data

CRW's `/v1/extract` endpoint uses LLM-powered extraction to pull structured data from pages:

```
def extract_data(url: str, prompt: str, schema: dict | None = None) -> dict:
    body = {"url": url, "prompt": prompt}
    if schema:
        body["schema"] = schema

    response = requests.post(
        f"{CRW_URL}/v1/extract",
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {API_KEY}",
        },
        json=body,
    )
    return response.json()

# Extract product info
result = extract_data(
    url="https://example.com/product",
    prompt="Extract the product name, price, and key features",
    schema={
        "type": "object",
        "properties": {
            "product_name": {"type": "string"},
            "price": {"type": "string"},
            "features": {"type": "array", "items": {"type": "string"}},
        },
    },
)
print(result)
```

## Comparison: Traditional vs. CRW Approach

| Feature | requests + BS4 | Scrapy | Playwright | CRW |
| --- | --- | --- | --- | --- |
| JavaScript rendering | No | No (plugin needed) | Yes | Yes |
| Setup complexity | Low | Medium | High | Low |
| Content cleaning | Manual | Manual | Manual | Automatic |
| Selector maintenance | Required | Required | Required | Not needed |
| Concurrent crawling | DIY | Built-in | DIY | Built-in |
| Output format | Raw HTML | Raw HTML | Raw HTML | Clean markdown |
| Resource usage | Low | Medium | High | Low (API call) |
| Structured extraction | Manual parsing | Manual parsing | Manual parsing | LLM-powered |

## Real-World Example: Building a Documentation Indexer

Here's a complete script that crawls a documentation site and builds a searchable index:

```
import json

CRW_URL = "https://api.fastcrw.com"
API_KEY = "crw_live_YOUR_API_KEY"

def crawl_docs(site_url: str, limit: int = 100) -> list[dict]:
    """Crawl a documentation site and return all pages."""
    start = requests.post(
        f"{CRW_URL}/v1/crawl",
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {API_KEY}",
        },
        json={
            "url": site_url,
            "limit": limit,
            "scrapeOptions": {"formats": ["markdown"]},
        },
    )
    job_id = start.json()["id"]

    while True:
        time.sleep(2)
        status = requests.get(
            f"{CRW_URL}/v1/crawl/{job_id}",
            headers={"Authorization": f"Bearer {API_KEY}"},
        ).json()
        if status["status"] == "completed":
            return status["data"]
        if status["status"] == "failed":
            raise Exception("Crawl failed")

def build_index(pages: list[dict]) -> list[dict]:
    """Build a searchable index from crawled pages."""
    index = []
    for page in pages:
        metadata = page.get("metadata", {})
        markdown = page.get("markdown", "")
        if not markdown:
            continue
        index.append({
            "url": metadata.get("sourceURL", ""),
            "title": metadata.get("title", "Untitled"),
            "description": metadata.get("description", ""),
            "content": markdown,
            "word_count": len(markdown.split()),
        })
    return index

def search_index(index: list[dict], query: str) -> list[dict]:
    """Simple keyword search across the index."""
    query_lower = query.lower()
    results = []
    for doc in index:
        content_lower = doc["content"].lower()
        if query_lower in content_lower:
            # Count occurrences as a basic relevance score
            score = content_lower.count(query_lower)
            results.append({**doc, "score": score})
    return sorted(results, key=lambda x: x["score"], reverse=True)

# Run it
pages = crawl_docs("https://docs.example.com", limit=50)
index = build_index(pages)
print(f"Indexed {len(index)} pages, {sum(d['word_count'] for d in index)} total words")

# Search
results = search_index(index, "authentication")
for r in results[:5]:
    print(f"  [{r['score']}] {r['title']} — {r['url']}")

# Save for later
with open("docs_index.json", "w") as f:
    json.dump(index, f, indent=2)
```

## Error Handling and Best Practices

```
import time

from requests.adapters import HTTPAdapter
from urllib3.util.retry import Retry

def create_session() -> requests.Session:
    """Create a session with automatic retries."""
    session = requests.Session()
    retries = Retry(
        total=3,
        backoff_factor=1,
        status_forcelist=[429, 500, 502, 503, 504],
    )
    session.mount("http://", HTTPAdapter(max_retries=retries))
    session.mount("https://", HTTPAdapter(max_retries=retries))
    return session

def scrape_with_retry(url: str, session: requests.Session | None = None) -> dict | None:
    """Scrape a URL with retry logic and error handling."""
    session = session or create_session()
    try:
        response = session.post(
            f"{CRW_URL}/v1/scrape",
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {API_KEY}",
            },
            json={"url": url, "formats": ["markdown"]},
            timeout=30,
        )
        response.raise_for_status()
        data = response.json()
        if not data.get("success"):
            print(f"Scrape failed for {url}: {data.get('error')}")
            return None
        return data["data"]
    except requests.exceptions.RequestException as e:
        print(f"Request failed for {url}: {e}")
        return None
```

## When to Use Each Approach

- **requests + Beautiful Soup:** Quick one-off scrapes of static HTML pages where you know the exact CSS selectors. Good for learning.
- **Scrapy:** Large-scale crawls where you need fine-grained control over crawl behavior, middleware, and export pipelines.
- **Playwright:** Pages that require browser interaction — login flows, infinite scroll, clicking through tabs.
- **CRW:** Everything else. When you want clean content without maintaining selectors, when you need markdown output for AI/LLM pipelines, or when you want a simple API call instead of a scraping infrastructure.

## Self-Hosted vs. Cloud

|  | Self-Hosted CRW | fastCRW Cloud |
| --- | --- | --- |
| Setup | `docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest` | Sign up at [fastcrw.com](https://fastcrw.com) |
| Cost | Free (your infrastructure) | Pay per request |
| API URL | `http://localhost:3000` | `https://api.fastcrw.com` |
| Proxy rotation | Not included | Built-in |
| Best for | High volume, data privacy | Quick start, no infra overhead |

## Conclusion

Python web scraping has evolved. While requests + Beautiful Soup and Scrapy remain valuable tools, the API approach with CRW eliminates most of the pain points — no selector maintenance, no browser management, automatic content cleaning, and clean markdown output ready for AI pipelines.

For RAG and AI applications, see our [RAG pipeline guide](/blog/rag-pipeline-with-crw). For a comparison with other scraping APIs, check [CRW vs. Firecrawl](/blog/firecrawl-vs-crawl4ai-vs-crw).

Get started: [self-host CRW](https://github.com/fastcrw/crw) or try [fastCRW cloud](https://fastcrw.com).
