# Playwright vs Puppeteer vs CRW: AI Scraping Compared

> Playwright vs Puppeteer vs CRW for AI web scraping — compare browser automation and API-first approaches with benchmarks.

**Published:** 2026-04-02  
**Updated:** 2026-04-02  
**Canonical:** https://fastcrw.com/blog/playwright-vs-puppeteer-vs-crw

---

## Short Answer

If you need full browser interaction — clicking buttons, filling forms, navigating SPAs — **Playwright** is the most capable option. If you're already in the Google ecosystem and want a lighter browser tool — **Puppeteer** works. If you need fast, structured data extraction from web pages for AI pipelines without the overhead of running browsers — **CRW** is the better fit.

- **Better fit for complex browser interactions:** Playwright (multi-browser, auto-wait, codegen)
- **Better fit for simple Chromium-only automation:** Puppeteer (lighter API, Chrome-native)
- **Better fit for AI/RAG data pipelines:** CRW (API-first, markdown output, MCP built-in)
- **Better fit for self-hosted scraping on constrained infra:** CRW (single small static binary, no headless-browser memory baseline)

|  | Playwright | Puppeteer | CRW |
| --- | --- | --- | --- |
| Approach | Browser automation | Browser automation | API-first scraping |
| Language support | JS, Python, Java, C# | JS only | Any (REST API) |
| Browser engines | Chromium, Firefox, WebKit | Chromium only | No browser needed* |
| Idle memory baseline | Full browser process | Full browser process | **No browser baseline** |
| Latency per page | Browser render each time | Browser render each time | **Low (no browser in path)** |
| Footprint | ~1.5 GB image | ~1 GB image | **Single small static binary** |
| Markdown output | Manual (you parse) | Manual (you parse) | ✅ Built-in |
| MCP server | Community packages | No | ✅ Built-in |
| Structured extraction | Manual (you code it) | Manual (you code it) | ✅ JSON schema via API |
| JavaScript rendering | ✅ Full browser | ✅ Full browser | Via LightPanda |
| Anti-bot bypass | Stealth plugins | Stealth plugins | Partial |
| License | Apache 2.0 | Apache 2.0 | AGPL-3.0 |

** CRW uses lol-html (Cloudflare's streaming parser) for most pages. LightPanda is used for JS-heavy pages when needed.*

## What Is Playwright?

Playwright is Microsoft's browser automation framework. It controls Chromium, Firefox, and WebKit through a single API, supports multiple languages (JavaScript, Python, Java, C#), and includes features like auto-waiting, network interception, and test code generation. Originally designed for end-to-end testing, it's widely adopted for web scraping because it can handle any page a real browser can render.

For scraping, Playwright's strength is universality: if a human can see it in a browser, Playwright can extract it. The tradeoff is resource cost — every Playwright instance runs a full browser process with hundreds of megabytes of RAM, and each page load takes seconds rather than milliseconds.

## What Is Puppeteer?

Puppeteer is Google's Node.js library for controlling Chromium (and experimentally Firefox) via the Chrome DevTools Protocol. It predates Playwright — in fact, Playwright's original authors came from the Puppeteer team at Google before moving to Microsoft.

Puppeteer is simpler than Playwright: it targets Chromium only, has a JavaScript-only API, and lacks some of Playwright's advanced features like multi-browser support and built-in auto-waiting. For Chromium-specific scraping tasks, it's a lighter alternative. But the core tradeoff is the same: you're running a full browser, which means high memory usage and slow page loads.

## What Is CRW?

CRW is an open-source web scraping API written in Rust. Instead of running a browser, it uses **lol-html** — Cloudflare's streaming HTML rewriter — to parse pages directly at the HTTP level. This means no Chromium, no browser process, no GPU memory. The result is dramatically lower resource usage (no headless-browser memory baseline) and lower, more predictable latency because there is no browser render in the request path. On a labeled public benchmark, CRW reached 63.74% truth-recall (522 of 819 labeled URLs) with 91.8% scrape success (of reachable URLs) and 0 errors (full distribution + one-command repro on /benchmarks).

CRW exposes a Firecrawl-compatible REST API, so it works with existing Firecrawl SDKs and integrations. It outputs clean markdown, supports structured JSON extraction via LLM schemas, and includes a built-in MCP server for AI agent integration. For pages that genuinely require JavaScript execution, CRW falls back to LightPanda — a lightweight headless browser that avoids Chromium's overhead.

## The Core Architecture Difference

The fundamental distinction isn't between Playwright and Puppeteer — they're both browser automation tools with similar tradeoffs. The real split is between **browser-based scraping** and **API-first scraping**.

### Browser-based (Playwright/Puppeteer)

- Launches a real browser process for every scraping session
- Executes all JavaScript, renders CSS, loads images
- Can interact with the page: click, type, scroll, wait for elements
- Consumes 150–400 MB RAM per browser instance
- Each page load takes 2–5 seconds including render time
- You write code to extract data from the rendered DOM

### API-first (CRW)

- No browser process — parses HTML at the HTTP response level
- Streaming parser processes HTML as it arrives (no full DOM construction)
- Returns clean markdown, HTML, links, or structured JSON via REST API
- No headless-browser memory baseline; handles many concurrent requests without memory pressure
- Lower, more predictable response time than browser-based approaches (no browser render in the request path)
- Data extraction is declarative — pass a JSON schema, get structured output

## When Browser Automation Wins

Browser automation is the right choice when you genuinely need what a browser provides. Here are the concrete scenarios:

### 1. Single-page applications with complex client-side routing

If the page you're scraping is a React/Vue/Angular SPA where content loads entirely via JavaScript and the HTML response is just an empty ``, a browser is the most reliable way to get the rendered content. CRW handles many SPAs via LightPanda, but for very complex routing and state management, Playwright is more mature.

### 2. Authenticated flows requiring login interaction

If you need to log in — typing a username, clicking a button, handling MFA redirects — Playwright gives you programmatic control over the full interaction. CRW doesn't simulate user interactions; it scrapes content available at a URL.

### 3. Pages behind anti-bot systems

Some sites use advanced bot detection (Cloudflare Turnstile, DataDome, PerimeterX) that requires a real browser fingerprint to pass. Playwright with stealth plugins can sometimes bypass these. CRW's anti-bot handling is functional but less sophisticated for heavily protected sites.

### 4. Visual testing or screenshot capture

If your workflow requires taking screenshots of rendered pages, browser automation is the only option. CRW does not currently support screenshot capture.

## When API-First Scraping Wins

For the majority of web scraping use cases — especially in AI and data pipeline contexts — browser automation is overkill. Here's where CRW's approach is a better fit:

### 1. AI agent pipelines and RAG

AI agents need web content as clean text, not as a rendered DOM. CRW outputs markdown directly, which is what LLMs consume. With Playwright, you'd scrape the page, then write custom logic to extract text, strip navigation, remove ads, and convert to a format the LLM can use. CRW handles all of that automatically.

```
# With CRW — one API call, clean markdown output
curl -X POST http://localhost:3000/v1/scrape   -H "Content-Type: application/json"   -d '{"url": "https://docs.example.com/api-reference", "formats": ["markdown"]}'
```

### 2. High-volume scraping

If you're scraping hundreds or thousands of pages, browser automation becomes a resource bottleneck. Each browser instance carries a heavy per-process memory cost, so at high concurrency you need many gigabytes of RAM just for browser processes. CRW handles the same load as a single small static binary with no per-request browser, a tiny fraction of that footprint.

### 3. Scraping on constrained infrastructure

Running Playwright on a $5 VPS is painful — the browser alone may consume all available RAM. CRW's single small static binary and absence of a headless-browser baseline mean it runs comfortably on the smallest VPS tiers. See our [post on running CRW on a $5 VPS](/blog/crw-on-5-dollar-vps) for a walkthrough.

### 4. Content sites, docs, articles, and product pages

The vast majority of content on the web — news articles, documentation, blog posts, product listings — is server-rendered HTML. These pages don't need JavaScript execution to extract content. Using a browser for these pages is like driving a truck to the corner store. CRW parses them in milliseconds.

### 5. Structured data extraction

CRW supports JSON schema-based extraction via its API. Pass a schema describing the data you want, and CRW returns structured JSON. With Playwright, you'd write custom selectors and parsing logic for every page structure.

```
// Structured extraction with CRW
const result = await app.scrapeUrl("https://example.com/product", {
  formats: ["json"],
  jsonSchema: {
    type: "object",
    properties: {
      name: { type: "string" },
      price: { type: "number" },
      in_stock: { type: "boolean" },
    },
    required: ["name", "price"],
  },
});

console.log(result.json?.name);  // "Widget Pro"
console.log(result.json?.price); // 29.99
```

## Playwright vs Puppeteer: Head-to-Head

If you've decided that browser automation is the right approach for your use case, here's how Playwright and Puppeteer compare directly:

|  | Playwright | Puppeteer |
| --- | --- | --- |
| Multi-browser | ✅ Chromium, Firefox, WebKit | Chromium only (Firefox experimental) |
| Auto-waiting | ✅ Built-in | Manual (waitForSelector) |
| Parallel contexts | ✅ Browser contexts (lightweight) | Incognito contexts |
| Code generation | ✅ codegen CLI tool | No |
| Network interception | ✅ Route API | ✅ Page.setRequestInterception |
| Language support | JS, Python, Java, C# | JS only |
| Stealth/anti-detection | playwright-extra + stealth | puppeteer-extra + stealth |
| Maintenance | Active (Microsoft) | Active (Google) |

**Recommendation:** If you need browser automation, Playwright is the better choice for new projects. It has broader browser support, better auto-waiting, a more modern API, and stronger multi-language support. Puppeteer is fine if you're already using it and don't need multi-browser testing, but there's little reason to choose it for new work.

## Code Comparison: Scraping a Product Page

Let's compare what it takes to extract product data from the same page using all three tools.

### Playwright (Node.js)

```
import { chromium } from "playwright";

const browser = await chromium.launch();
const page = await browser.newPage();
await page.goto("https://example.com/product");

const product = {
  name: await page.textContent("h1.product-title"),
  price: parseFloat(
    (await page.textContent(".price"))?.replace("$", "") ?? "0"
  ),
  description: await page.textContent(".product-description"),
  inStock: (await page.textContent(".stock-status"))?.includes("In Stock"),
};

await browser.close();
console.log(product);
// ~3 seconds, ~300 MB RAM for the browser process
```

### Puppeteer

```
import puppeteer from "puppeteer";

const browser = await puppeteer.launch();
const page = await browser.newPage();
await page.goto("https://example.com/product", { waitUntil: "networkidle2" });

const product = await page.evaluate(() => ({
  name: document.querySelector("h1.product-title")?.textContent,
  price: parseFloat(
    document.querySelector(".price")?.textContent?.replace("$", "") ?? "0"
  ),
  description: document.querySelector(".product-description")?.textContent,
  inStock: document
    .querySelector(".stock-status")
    ?.textContent?.includes("In Stock"),
}));

await browser.close();
console.log(product);
// ~3 seconds, ~250 MB RAM for the browser process
```

### CRW

```
import FirecrawlApp from "@mendable/firecrawl-js";

const app = new FirecrawlApp({
  apiKey: "your-key",
  apiUrl: "http://localhost:3000", // self-hosted CRW
});

const result = await app.scrapeUrl("https://example.com/product", {
  formats: ["json"],
  jsonSchema: {
    type: "object",
    properties: {
      name: { type: "string" },
      price: { type: "number" },
      description: { type: "string" },
      inStock: { type: "boolean" },
    },
    required: ["name", "price"],
  },
});

console.log(result.json);
// Single API call, no browser process — CRW server has no headless-browser baseline
```

The CRW approach is declarative — you describe what you want, not how to find it. This is a significant advantage for AI pipelines where the extraction logic shouldn't be tightly coupled to CSS selectors that break when the page redesigns.

## MCP Integration: AI Agents and Web Context

For teams building AI agents that need web access, the MCP (Model Context Protocol) integration is a key differentiator. CRW ships with a built-in MCP server — configure it in Claude Desktop or Cursor and your agent can scrape web pages directly:

```
{
  "mcpServers": {
    "crw": {
      "command": "docker",
      "args": ["run", "--rm", "-i", "ghcr.io/fastcrw/crw:latest", "crw-mcp"]
    }
  }
}
```

Playwright and Puppeteer don't have native MCP support. You'd need to wrap them in a custom MCP server, handle browser lifecycle management, and deal with the memory overhead of running browsers alongside your AI agent. CRW's built-in MCP makes web access a zero-configuration addition to any AI workflow.

## Performance Under Load

The performance gap between browser-based and API-first scraping widens dramatically under concurrent load:

| Concurrent requests | Playwright memory shape | CRW memory shape | Relative wall-clock |
| --- | --- | --- | --- |
| 1 | One full browser process | No browser baseline | CRW lower (no render) |
| 10 | Grows ~linearly per browser | Grows with connection buffers only | CRW gap widens |
| 50 | Each session adds a browser's memory | Stays a small fraction of browser-based | CRW gap widest |

At high concurrency, browser automation needs a large-memory machine just for the browser processes. CRW handles the same load on the cheapest VPS tier. This isn't theoretical — it's the practical difference between running scraping as a sidecar service and needing dedicated scraping infrastructure. See our [benchmark post](/blog/benchmark-crw) and the full latency distribution with a one-command repro on [/benchmarks](/benchmarks).

## When to Use Each: Decision Framework

Here's a practical decision tree:

### Use Playwright when:

- You need to interact with the page (click, type, scroll, navigate)
- The page is a complex SPA with client-side routing that CRW can't handle
- You need to bypass advanced anti-bot systems that require a real browser fingerprint
- You need screenshots or visual testing alongside scraping
- You're already using Playwright for E2E testing and want to reuse that infrastructure

### Use Puppeteer when:

- You need browser automation but only target Chromium
- You have an existing Puppeteer codebase and migration to Playwright isn't worth the effort
- You want a simpler API for straightforward Chromium-only tasks

### Use CRW when:

- You're building AI agent pipelines that need web content as markdown
- You're building RAG systems that ingest web pages
- You need to scrape at scale without massive infrastructure
- You want structured data extraction via JSON schema rather than CSS selectors
- You're running on constrained infrastructure (small VPS, edge deployments)
- You want an MCP-compatible scraping tool for AI agents
- The pages you're scraping are content-heavy (articles, docs, product pages) rather than interaction-heavy

## The Hybrid Approach

In practice, many teams use both approaches. CRW handles the 90% of pages that are content-oriented (articles, docs, listings), while Playwright handles the 10% that genuinely require browser interaction (login flows, complex SPAs, anti-bot bypasses).

Because CRW exposes a Firecrawl-compatible REST API, it's easy to build a routing layer that sends requests to CRW by default and falls back to a Playwright-based scraper for specific domains or patterns. This gives you the performance and efficiency of API-first scraping for the common case, with browser automation available when you need it.

## Try CRW

### Open-Source Path — Self-Host for Free

CRW is AGPL-3.0 licensed. Run it on your own infrastructure at zero cost:

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

[View the source on GitHub](https://github.com/fastcrw/crw) · [Read the docs](https://us.github.io/crw)

### Hosted Path — Use fastCRW

Don't want to manage servers? [fastCRW](https://fastcrw.com) is the managed cloud version — same Firecrawl-compatible API, same low-latency engine, with infrastructure and scaling handled for you. Start with a one-time lifetime 1000 credits (not a monthly meter), no credit card required.

## Further Reading

- [CRW vs Firecrawl: A Practical Comparison](/blog/firecrawl-vs-crawl4ai-vs-crw)
- [CRW Benchmark: 500 URLs, Real Results](/blog/benchmark-crw)
- [Why Low Memory Matters for Web Scraping](/blog/low-memory-scraping)
- [Running CRW on a $5 VPS](/blog/crw-on-5-dollar-vps)
