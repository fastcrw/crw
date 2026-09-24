# JavaScript Web Scraping in 2026 — 4 Approaches Tested (Cheerio, Puppeteer, Playwright, fastCRW)

> JavaScript web scraping compared: Cheerio (fastest parsing), Puppeteer, Playwright, fastCRW API. Code examples in Node.js + TypeScript with cost, RAM, and reliability tradeoffs. Pick the right tool for your scraper.

**Published:** 2026-04-21  
**Updated:** 2026-04-21  
**Canonical:** https://fastcrw.com/blog/javascript-web-scraping

---

## Overview

JavaScript is a natural choice for web scraping — you're scraping the same platform you build for. Node.js gives you fast async I/O, and the ecosystem includes mature browser automation libraries. This guide covers the major approaches and shows how CRW's API simplifies the entire workflow.

We'll cover: **Cheerio** (server-side HTML parsing), **Puppeteer** (Chrome automation), **Playwright** (cross-browser automation), and **CRW** (the API approach). All examples work with both JavaScript and TypeScript.

## The Classic: Cheerio

Cheerio provides jQuery-like syntax for parsing HTML on the server. Pair it with `fetch` or `axios` for a lightweight scraping setup:

```
import * as cheerio from "cheerio";

const url = "https://example.com/blog";
const response = await fetch(url);
const html = await response.text();
const $ = cheerio.load(html);

const articles: Array<{ title: string; url: string }> = [];

$("article h2 a").each((_, el) => {
  articles.push({
    title: $(el).text().trim(),
    url: $(el).attr("href") ?? "",
  });
});

console.log(`Found ${articles.length} articles`);
```

### Pros and Cons

- **Pros:** Fast, lightweight, familiar jQuery syntax, great for static HTML
- **Cons:** No JavaScript execution, requires manual selectors, breaks when HTML structure changes

## Browser Automation: Puppeteer

Puppeteer controls a headless Chrome instance — essential for JavaScript-rendered pages:

```
import puppeteer from "puppeteer";

const browser = await puppeteer.launch({ headless: true });
const page = await browser.newPage();

await page.goto("https://example.com/app", {
  waitUntil: "networkidle2",
});

// Wait for dynamic content
await page.waitForSelector(".product-card");

const products = await page.evaluate(() => {
  return Array.from(document.querySelectorAll(".product-card")).map((card) => ({
    name: card.querySelector("h3")?.textContent?.trim() ?? "",
    price: card.querySelector(".price")?.textContent?.trim() ?? "",
  }));
});

console.log(products);
await browser.close();
```

### Pros and Cons

- **Pros:** Full JavaScript rendering, can interact with pages, screenshot support
- **Cons:** Heavy (~300MB Chrome download), slow startup, high memory usage, Chrome-only

## Cross-Browser: Playwright

Playwright offers similar capabilities to Puppeteer but supports Chromium, Firefox, and WebKit:

```
import { chromium } from "playwright";

const browser = await chromium.launch({ headless: true });
const page = await browser.newPage();

await page.goto("https://example.com/app");
await page.waitForSelector(".product-card");

const products = await page.$$eval(".product-card", (cards) =>
  cards.map((card) => ({
    name: card.querySelector("h3")?.textContent?.trim() ?? "",
    price: card.querySelector(".price")?.textContent?.trim() ?? "",
  })),
);

console.log(products);
await browser.close();
```

### Pros and Cons

- **Pros:** Cross-browser testing, auto-wait for elements, better API than Puppeteer
- **Cons:** Same weight and speed issues as Puppeteer, complex setup in CI/Docker

## The Modern Approach: CRW API

CRW handles scraping, JavaScript rendering, and content extraction server-side. You make an API call and get clean markdown back. No browser to manage, no selectors to maintain.

### Direct HTTP with fetch

```
const CRW_URL = "https://api.fastcrw.com"; // or http://localhost:3000
const API_KEY = "crw_live_YOUR_API_KEY";

async function scrapePage(url: string) {
  const response = await fetch(`${CRW_URL}/v1/scrape`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${API_KEY}`,
    },
    body: JSON.stringify({ url, formats: ["markdown"] }),
  });

  const data = await response.json();
  if (!data.success) throw new Error(data.error ?? "Scrape failed");

  return {
    markdown: data.data.markdown,
    title: data.data.metadata?.title ?? "",
    sourceURL: data.data.metadata?.sourceURL ?? url,
  };
}

// Usage
const result = await scrapePage("https://nodejs.org/en/learn");
console.log(`Title: ${result.title}`);
console.log(`Content: ${result.markdown.slice(0, 200)}...`);
```

### Using the Firecrawl JS SDK

CRW is Firecrawl-compatible, so the official Firecrawl JavaScript SDK works out of the box:

```
import FirecrawlApp from "@mendable/firecrawl-js";

const app = new FirecrawlApp({
  apiKey: "crw_live_YOUR_API_KEY",
  apiUrl: "https://api.fastcrw.com", // or http://localhost:3000
});

// Scrape a single page
const scrapeResult = await app.scrapeUrl("https://nodejs.org/en/learn");
console.log(scrapeResult.markdown?.slice(0, 200));
```

### Crawl an Entire Website

```
async function crawlSite(url: string, limit = 50) {
  // Start the crawl
  const startResponse = await fetch(`${CRW_URL}/v1/crawl`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${API_KEY}`,
    },
    body: JSON.stringify({
      url,
      limit,
      scrapeOptions: { formats: ["markdown"] },
    }),
  });
  const { id } = await startResponse.json();

  // Poll for completion
  while (true) {
    await new Promise((resolve) => setTimeout(resolve, 2000));
    const statusResponse = await fetch(`${CRW_URL}/v1/crawl/${id}`, {
      headers: { "Authorization": `Bearer ${API_KEY}` },
    });
    const status = await statusResponse.json();

    if (status.status === "completed") {
      console.log(`Crawled ${status.data.length} pages`);
      return status.data;
    }
    if (status.status === "failed") throw new Error("Crawl failed");
    console.log(`Progress: ${status.data?.length ?? 0} pages...`);
  }
}

// Using the SDK (handles polling automatically)
const crawlResult = await app.crawlUrl("https://nodejs.org/en/learn", {
  limit: 20,
  scrapeOptions: { formats: ["markdown"] },
});
console.log(`Crawled ${crawlResult.data?.length} pages`);
```

### Discover URLs with Map

```
async function discoverUrls(url: string): Promise<string[]> {
  const response = await fetch(`${CRW_URL}/v1/map`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${API_KEY}`,
    },
    body: JSON.stringify({ url }),
  });
  const data = await response.json();
  return data.links ?? [];
}

// Discover pages, filter, then scrape selectively
const allUrls = await discoverUrls("https://nodejs.org");
const apiUrls = allUrls.filter((u) => u.includes("/api/"));
console.log(`Found ${apiUrls.length} API docs out of ${allUrls.length} total URLs`);
```

### Extract Structured Data

```
async function extractData(
  url: string,
  prompt: string,
  schema?: Record<string, unknown>,
) {
  const body: Record<string, unknown> = { url, prompt };
  if (schema) body.schema = schema;

  const response = await fetch(`${CRW_URL}/v1/extract`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${API_KEY}`,
    },
    body: JSON.stringify(body),
  });
  return response.json();
}

// Extract structured product data
const extracted = await extractData(
  "https://example.com/product",
  "Extract the product name, price, and rating",
  {
    type: "object",
    properties: {
      product_name: { type: "string" },
      price: { type: "string" },
      rating: { type: "number" },
    },
  },
);
console.log(extracted);
```

## Comparison: JS Scraping Approaches

| Feature | Cheerio | Puppeteer | Playwright | CRW |
| --- | --- | --- | --- | --- |
| JS rendering | No | Yes | Yes | Yes |
| Install size | ~2 MB | ~300 MB | ~300 MB | 0 (API call) |
| Content cleaning | Manual | Manual | Manual | Automatic |
| Selector maintenance | Required | Required | Required | Not needed |
| Output | Raw HTML | Raw HTML | Raw HTML | Clean markdown |
| Concurrent scraping | DIY | DIY | DIY | Built-in |
| Docker friendly | Yes | Complex | Complex | Yes (API call) |

## Real-World Example: Building a Content Monitor

Here's a complete TypeScript script that monitors web pages for changes:

```
import { readFile, writeFile } from "node:fs/promises";

const CRW_URL = "https://api.fastcrw.com";
const API_KEY = "crw_live_YOUR_API_KEY";

interface PageSnapshot {
  url: string;
  hash: string;
  title: string;
  scrapedAt: string;
}

async function scrape(url: string): Promise<{ markdown: string; title: string }> {
  const res = await fetch(`${CRW_URL}/v1/scrape`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${API_KEY}`,
    },
    body: JSON.stringify({ url, formats: ["markdown"] }),
  });
  const data = await res.json();
  if (!data.success) throw new Error(`Failed: ${url}`);
  return {
    markdown: data.data.markdown,
    title: data.data.metadata?.title ?? url,
  };
}

async function checkForChanges(urls: string[]) {
  const snapshotFile = "snapshots.json";
  let previousSnapshots: PageSnapshot[] = [];
  try {
    const raw = await readFile(snapshotFile, "utf-8");
    previousSnapshots = JSON.parse(raw);
  } catch {
    // First run — no previous snapshots
  }
  const previousMap = new Map(previousSnapshots.map((s) => [s.url, s]));

  const currentSnapshots: PageSnapshot[] = [];
  const changes: Array<{ url: string; title: string; type: "new" | "changed" }> = [];

  for (const url of urls) {
    const { markdown, title } = await scrape(url);
    const hash = createHash("sha256").update(markdown).digest("hex");

    const previous = previousMap.get(url);
    if (!previous) {
      changes.push({ url, title, type: "new" });
    } else if (previous.hash !== hash) {
      changes.push({ url, title, type: "changed" });
    }

    currentSnapshots.push({
      url,
      hash,
      title,
      scrapedAt: new Date().toISOString(),
    });
  }

  await writeFile(snapshotFile, JSON.stringify(currentSnapshots, null, 2));
  return changes;
}

// Monitor these pages
const urls = [
  "https://docs.example.com/pricing",
  "https://docs.example.com/api",
  "https://docs.example.com/changelog",
];

const changes = await checkForChanges(urls);
if (changes.length === 0) {
  console.log("No changes detected.");
} else {
  console.log(`${changes.length} change(s) detected:`);
  for (const c of changes) {
    console.log(`  [${c.type}] ${c.title} — ${c.url}`);
  }
}
```

## Error Handling and Production Patterns

```
async function scrapeWithRetry(
  url: string,
  maxRetries = 3,
): Promise<string | null> {
  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    try {
      const res = await fetch(`${CRW_URL}/v1/scrape`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "Authorization": `Bearer ${API_KEY}`,
        },
        body: JSON.stringify({ url, formats: ["markdown"] }),
        signal: AbortSignal.timeout(30_000),
      });

      if (!res.ok) throw new Error(`HTTP ${res.status}`);

      const data = await res.json();
      if (!data.success) throw new Error(data.error ?? "Scrape failed");
      return data.data.markdown;
    } catch (err) {
      console.warn(`Attempt ${attempt}/${maxRetries} failed for ${url}:`, err);
      if (attempt < maxRetries) {
        await new Promise((r) => setTimeout(r, 1000 * attempt));
      }
    }
  }
  return null;
}

// Concurrent scraping with concurrency limit
async function scrapeMany(urls: string[], concurrency = 5) {
  const results: Array<{ url: string; markdown: string }> = [];
  const queue = [...urls];

  async function worker() {
    while (queue.length > 0) {
      const url = queue.shift()!;
      const markdown = await scrapeWithRetry(url);
      if (markdown) results.push({ url, markdown });
    }
  }

  await Promise.all(
    Array.from({ length: concurrency }, () => worker()),
  );
  return results;
}
```

## When to Use Each Approach

- **Cheerio:** Quick parsing of static HTML when you know the selectors. Fast scripts where you don't need JS rendering.
- **Puppeteer:** When you specifically need Chrome DevTools Protocol features — performance tracing, coverage analysis, or Chrome-specific APIs.
- **Playwright:** When you need browser interaction — login flows, form submissions, infinite scroll — or cross-browser testing.
- **CRW:** When you want clean content with minimal code. Best for markdown output, AI/LLM pipelines, content monitoring, and any scenario where you'd rather make an API call than manage a browser.

## Self-Hosted vs. Cloud

|  | Self-Hosted CRW | fastCRW Cloud |
| --- | --- | --- |
| Setup | `docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest` | Sign up at [fastcrw.com](https://fastcrw.com) |
| Cost | Free (your infra) | Pay per request |
| API URL | `http://localhost:3000` | `https://api.fastcrw.com` |
| Proxy rotation | Not included | Built-in |
| Best for | High volume, privacy | Quick start, no infra |

## Conclusion

JavaScript web scraping has come a long way from jQuery-parsing raw HTML. With CRW, you get clean markdown output, built-in crawling, and structured data extraction — all through simple API calls that work with both `fetch` and the Firecrawl JS SDK.

For AI and RAG applications, see our [RAG pipeline guide](/blog/rag-pipeline-with-crw). To convert websites to markdown, check [our conversion guide](/blog/website-to-markdown).

Get started: [self-host CRW](https://github.com/fastcrw/crw) or try [fastCRW cloud](https://fastcrw.com).
