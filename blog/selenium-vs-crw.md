# Selenium vs CRW: Legacy Browser Scraping vs Modern API

> Selenium vs CRW — why teams are switching to API-based scraping, where Selenium still fits, and an honest comparison for AI pipelines.

**Published:** 2026-03-30  
**Updated:** 2026-03-30  
**Canonical:** https://fastcrw.com/blog/selenium-vs-crw

---

## Short Answer

If you need cross-browser testing automation or must interact with complex web applications — **Selenium** still works, though Playwright has largely superseded it for new projects. If you need fast, reliable web scraping for data extraction and AI pipelines — **CRW** is the better fit. Selenium was never designed for scraping; CRW was built specifically for it.

- **Better fit for cross-browser test automation:** Selenium (or Playwright for new projects)
- **Better fit for interacting with legacy web apps:** Selenium (broad driver ecosystem)
- **Better fit for web scraping and data extraction:** CRW (API-first, built for this purpose)
- **Better fit for AI agent web access:** CRW (MCP built-in, markdown output)

|  | Selenium | CRW |
| --- | --- | --- |
| Primary purpose | Browser test automation | Web scraping API |
| Approach | Full browser (WebDriver protocol) | Streaming HTML parser |
| Language support | Java, Python, C#, JS, Ruby, Kotlin | Any (REST API) |
| Latency per page | Full-browser, multi-second | **Sub-second, local-first (see [benchmark](/benchmarks))** |
| Idle footprint | Browser process (~hundreds of MB) | **Tiny (single binary)** |
| Docker image size | ~1–2 GB (browser bundled) | **Small single binary** |
| Markdown output | No (you parse the DOM) | ✅ Built-in |
| Structured extraction | Manual (selectors + code) | ✅ JSON schema via API |
| MCP server | No | ✅ Built-in |
| Crawling support | Manual (you code it) | ✅ Built-in crawl endpoint |
| Driver management | Required (chromedriver, geckodriver) | None |
| Headless mode | Yes (browser-dependent) | N/A (no browser) |
| Anti-bot handling | Stealth plugins, undetected-chromedriver | Partial |
| License | Apache 2.0 | AGPL-3.0 |

## What Is Selenium?

Selenium is a browser automation framework originally created for testing web applications. It uses the WebDriver protocol to control real browsers — Chrome, Firefox, Safari, Edge — through language-specific bindings. It's been around since 2004, making it one of the oldest tools in the web automation space.

Over the years, developers adopted Selenium for web scraping because it was the most accessible way to interact with JavaScript-rendered pages. Before headless Chrome existed, Selenium with PhantomJS was the go-to solution for scraping dynamic content. That era created a generation of scraping infrastructure built on Selenium — infrastructure that many teams are now reconsidering.

## What Is CRW?

CRW is an open-source web scraping API written in Rust. It doesn't use a browser at all — it uses lol-html (Cloudflare's streaming HTML parser) to process pages directly from HTTP responses. This means no WebDriver, no chromedriver, no browser process. The result is sub-second, local-first latency (vs Selenium's multi-second full-browser navigation), a tiny idle footprint instead of a resident browser, and a small single binary instead of a multi-gigabyte image. See the full latency distribution and one-command repro on our [public benchmark](/benchmarks).

CRW exposes a Firecrawl-compatible REST API with built-in markdown output, structured JSON extraction, multi-page crawling, and an MCP server for AI agents. For JS-heavy pages, it falls back to LightPanda — a lightweight browser alternative — rather than Chromium.

## The Selenium Pain Points

If you've maintained a Selenium-based scraper for any length of time, you know these problems intimately:

### 1. Driver version management

Selenium requires a browser driver (chromedriver, geckodriver) that must match the installed browser version exactly. Chrome updates automatically. Your chromedriver doesn't. The result: your scraper breaks every few weeks when Chrome auto-updates and the driver version falls out of sync.

```
# The error every Selenium user knows
selenium.common.exceptions.SessionNotCreatedException:
Message: session not created: This version of ChromeDriver only supports
Chrome version 119. Current browser version is 121.0.6167.85
```

Solutions exist — `webdriver-manager`, Selenium Manager — but they add complexity and can fail in CI/CD environments where browser versions are pinned differently than in development.

CRW has no driver to manage. There's no browser version to track. The API works the same regardless of what's installed on the host machine.

### 2. Resource consumption

Every Selenium session launches a full browser process. In headless mode, Chrome still allocates 200–500 MB of RAM per instance. Running 10 concurrent scraping sessions means 2–5 GB of RAM just for browser processes — before your application code, database, or any other services.

On cloud infrastructure, this directly translates to cost. You need larger VMs, more container memory limits, and more aggressive scaling rules. Teams running Selenium at scale often find that their scraping infrastructure costs more than their application infrastructure.

CRW has a tiny idle footprint and stays modest even under concurrent load. The entire scraping service runs comfortably on a $5 VPS. See our [post on running CRW on a $5 VPS](/blog/crw-on-5-dollar-vps) for details.

### 3. Flaky selectors and timing issues

Selenium scraping code is full of `time.sleep()` calls, explicit waits, and fragile CSS selectors. Pages render asynchronously, elements appear at different times, and what works on your fast development machine fails on a slower CI runner.

```
# Typical Selenium scraping code — fragile and timing-dependent
from selenium.webdriver.support.ui import WebDriverWait
from selenium.webdriver.support import expected_conditions as EC

driver.get("https://example.com/products")
time.sleep(3)  # Hope the page has loaded by now

try:
    WebDriverWait(driver, 10).until(
        EC.presence_of_element_located((By.CSS_SELECTOR, ".product-list"))
    )
except TimeoutException:
    # Sometimes the class is different on mobile layout
    WebDriverWait(driver, 10).until(
        EC.presence_of_element_located((By.CSS_SELECTOR, ".products-grid"))
    )
```

CRW doesn't have timing issues because it's not rendering a page — it's parsing HTML. The response comes back when parsing is complete. No waits, no sleeps, no race conditions.

### 4. Difficult deployment

Deploying Selenium in production means installing a browser, a driver, and all their system-level dependencies (fonts, graphics libraries, dbus) in your container or VM. Selenium Docker images are 1–2 GB because they bundle a full operating system layer for the browser.

```
# Typical Selenium Dockerfile — heavy
FROM selenium/standalone-chrome:latest
# Image size: ~1.3 GB
# Includes: Chrome, chromedriver, X11 libs, fonts, VNC server
```

Compare with CRW:

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
# Includes: single Rust binary, nothing else
```

### 5. Poor fit for data pipelines

Selenium returns a live DOM that you query with selectors. For data pipelines, you need to: open a browser, navigate to a page, wait for it to load, query elements with selectors, extract text, clean up formatting, handle pagination, close the browser, and convert everything to a structured format. Each step can fail.

CRW returns clean markdown or structured JSON from a single API call. The output is immediately usable in data pipelines, RAG systems, and LLM workflows without any post-processing.

## Code Comparison: Same Task, Different Approaches

### Selenium (Python)

```
from selenium import webdriver
from selenium.webdriver.chrome.options import Options
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait
from selenium.webdriver.support import expected_conditions as EC

options = Options()
options.add_argument("--headless")
options.add_argument("--no-sandbox")
options.add_argument("--disable-dev-shm-usage")

driver = webdriver.Chrome(options=options)

try:
    driver.get("https://example.com/blog/article")

    # Wait for content to load
    WebDriverWait(driver, 10).until(
        EC.presence_of_element_located((By.TAG_NAME, "article"))
    )

    title = driver.find_element(By.CSS_SELECTOR, "h1").text
    content = driver.find_element(By.TAG_NAME, "article").text
    author = driver.find_element(By.CSS_SELECTOR, ".author-name").text

    article = {"title": title, "author": author, "content": content}
    print(article)
finally:
    driver.quit()

# Time: ~4 seconds
# RAM: ~400 MB (Chrome + chromedriver + Python)
# Lines of boilerplate: 15+
```

### CRW (Python SDK)

```
from firecrawl import FirecrawlApp

app = FirecrawlApp(
    api_key="your-key",
    api_url="http://localhost:3000",
)

result = app.scrape_url(
    "https://example.com/blog/article",
    params={
        "formats": ["json", "markdown"],
        "jsonSchema": {
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "author": {"type": "string"},
                "content": {"type": "string"},
            },
            "required": ["title", "content"],
        },
    },
)

print(result["json"])      # Structured data
print(result["markdown"])  # Clean markdown for LLM

# Time: sub-second (local-first; see /benchmarks)
# RAM: tiny single-binary footprint (CRW server)
# Lines of boilerplate: 0
```

## Crawling an Entire Site

Crawling — following links and scraping multiple pages — is where Selenium's limitations become most painful.

### Selenium approach

```
from selenium import webdriver
from selenium.webdriver.common.by import By

driver = webdriver.Chrome()
visited = set()
to_visit = ["https://example.com"]
results = []

while to_visit and len(visited) < 50:
    url = to_visit.pop(0)
    if url in visited:
        continue
    visited.add(url)

    try:
        driver.get(url)
        time.sleep(2)  # Wait for page load

        title = driver.find_element(By.TAG_NAME, "h1").text
        content = driver.find_element(By.TAG_NAME, "body").text
        results.append({"url": url, "title": title, "content": content})

        # Find links
        links = driver.find_elements(By.TAG_NAME, "a")
        for link in links:
            href = link.get_attribute("href")
            if href and href.startswith("https://example.com"):
                to_visit.append(href)
    except Exception:
        continue

driver.quit()
# Time: ~100+ seconds for 50 pages (sequential)
# RAM: 400 MB constant (single browser session)
```

This is sequential — one page at a time. Adding concurrency with Selenium means managing multiple browser instances, which multiplies the memory problem. You also need to handle URL deduplication, depth limiting, and error recovery manually.

### CRW approach

```
from firecrawl import FirecrawlApp

app = FirecrawlApp(api_key="your-key", api_url="http://localhost:3000")

result = app.crawl_url(
    "https://example.com",
    params={
        "limit": 50,
        "scrapeOptions": {"formats": ["markdown"]},
    },
)

for page in result["data"]:
    print(f"{page['metadata']['url']}: {len(page['markdown'])} chars")

# Time: ~20 seconds for 50 pages (concurrent internally)
# RAM: ~30 MB under load
```

CRW handles concurrency, link following, deduplication, and the page `limit` internally. The output is clean markdown for every page, ready for downstream processing.

## Where Selenium Still Makes Sense

Being honest: there are scenarios where Selenium is still the right tool.

### 1. Cross-browser testing

If your primary use case is testing web applications across Chrome, Firefox, Safari, and Edge — Selenium's WebDriver protocol is the W3C standard. That said, Playwright has largely replaced Selenium for new testing projects, with better APIs and faster execution.

### 2. Legacy infrastructure

If you have years of Selenium code in production, a mature Selenium Grid deployment, and a team that knows Selenium deeply — migration has real cost. The code works. It's slow and resource-heavy, but it works. Migration to CRW makes sense when you're already reworking your scraping pipeline or when the infrastructure cost becomes unjustifiable.

### 3. Interactive automation beyond scraping

If you need to fill forms, click through multi-step workflows, or automate browser-based tasks (not just extract data), Selenium provides that interaction layer. CRW extracts data from pages; it doesn't interact with them.

### 4. Sites requiring a real browser fingerprint

Some heavily protected sites detect non-browser clients and block them. Selenium with `undetected-chromedriver` can sometimes pass these checks. CRW's anti-bot handling is functional but less sophisticated for the most heavily protected sites.

## Migration Path: Selenium to CRW

For teams ready to move from Selenium to CRW, the migration is typically straightforward because the scraping logic gets simpler, not more complex.

### Step 1: Start CRW

```
docker run -p 3000:3000 -e CRW_API_KEY=my-secret ghcr.io/fastcrw/crw:latest
```

### Step 2: Replace Selenium calls with CRW API calls

```
# Before (Selenium)
driver = webdriver.Chrome()
driver.get("https://example.com")
title = driver.find_element(By.TAG_NAME, "h1").text
content = driver.find_element(By.TAG_NAME, "article").text
driver.quit()

# After (CRW)
from firecrawl import FirecrawlApp
app = FirecrawlApp(api_key="my-secret", api_url="http://localhost:3000")
result = app.scrape_url("https://example.com", params={"formats": ["markdown"]})
```

### Step 3: Remove browser dependencies

Once Selenium calls are replaced, you can remove Chrome, chromedriver, and all browser-related system dependencies from your Docker images and CI/CD pipelines. For most teams, this reduces image sizes by 1+ GB and eliminates the driver version management problem entirely.

### What you gain

- Sub-second, local-first scraping instead of multi-second full-browser navigation (see our [public benchmark](/benchmarks))
- A tiny single-binary footprint instead of a resident browser process
- No driver version management
- No browser dependencies in your Docker images
- Clean markdown output for AI/LLM pipelines
- Built-in MCP server for AI agent integration

### What you lose

- Browser interaction (click, type, scroll) — not available via CRW
- Screenshot capture on a LightPanda-only deployment — CRW captures over CDP, so it needs a Chrome or Playwright tier
- Real browser fingerprint for anti-bot bypass
- Full JavaScript execution for very complex SPAs

## Who Should Use Which

- **Use Selenium if:** you need cross-browser testing automation, interactive browser workflows, or you have mature Selenium infrastructure that isn't worth migrating yet. Consider Playwright instead of Selenium for new browser automation projects.
- **Use CRW (self-hosted) if:** you need fast, reliable web scraping for data extraction, AI pipelines, or RAG — especially on constrained infrastructure. The API-first approach eliminates browser overhead and returns clean, structured output.
- **Use fastCRW (cloud) if:** you want CRW's scraping engine without managing infrastructure — proxy networks, auto-scaling, and maintenance handled for you.

## Bottom Line

Selenium is a browser automation tool that people use for scraping. CRW is a scraping tool purpose-built for scraping. When you use the right tool for the job, you get lower latency, a far smaller resource footprint, and simpler code — see the numbers on our [public benchmark](/benchmarks).

The web scraping landscape has evolved. In 2015, running a headless browser was the only reliable way to scrape dynamic content. In 2026, streaming HTML parsers handle the vast majority of pages faster and cheaper. Selenium still has its place — but that place is browser testing, not data extraction.

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
