# Where CRW Still Falls Short — and What We're Improving

> An honest look at CRW's current limitations — screenshots, PDF parsing, anti-bot, SPA coverage, retry logic, caching — and the roadmap for each.

**Published:** 2026-04-03  
**Updated:** 2026-05-23  
**Canonical:** https://fastcrw.com/blog/crw-limitations

---

## Why We're Writing This

Trust in developer tools is built on honesty. If we only wrote about CRW's strengths, you'd discover the gaps when they matter most — mid-project. This post documents CRW's current limitations clearly, with workarounds and roadmap status for each. Use it to make an informed decision before committing.

## Limitation 1: Screenshots Need a Chrome-Class Browser Tier

**Current state:** CRW captures page screenshots. `"formats": ["screenshot"]` returns a `data:image/png;base64,…` URL, and `screenshot@fullPage` captures the whole page. The catch is the browser tier underneath: capture is the CDP `Page.captureScreenshot` command, so it needs Chrome (`chrome`, `chrome_proxy`) or Playwright. LightPanda and Camoufox cannot capture.

**What that means in practice:** an instance configured with only LightPanda or Camoufox refuses a screenshot request instead of returning an empty image. Screenshots also require JS rendering, so `renderJs: false` together with `screenshot` is rejected.

**Who it affects:** Teams self-hosting a minimal, LightPanda-only deployment for footprint reasons. Chrome is heavier than LightPanda, so a screenshot-capable instance costs more RAM.

**How to check before you rely on it:** `GET /v1/capabilities` reports `screenshot.supported` and lists the renderer tiers the instance actually constructed under `renderers.available`. Both are derived from the real build and config, so a `true` there means the instance can genuinely capture.

```
const caps = await fetch(`${BASE_URL}/v1/capabilities`).then((r) => r.json());

if (caps.screenshot.supported) {
  // safe to request formats: ["markdown", "screenshot"]
} else {
  // this deployment has no capture-capable tier — configure [renderer.chrome]
}
```

## Limitation 2: PDF Only, and No OCR

**Current state:** CRW parses PDFs. A PDF URL passed to `/v1/scrape` is auto-routed to the parser by content type, and files can be uploaded to the parse endpoint. It is PDF only: DOCX, XLSX, RTF and ODT are not parsed.

**The real gap is OCR.** The parser (`pdf-inspector`, pure Rust) reads the text layer. A scanned or image-only PDF has no text layer, so it yields empty or partial text with a warning rather than an error. There is no OCR engine in the binary.

**Who it affects:** Teams indexing scanned document archives, or mixed repositories that hold Office formats alongside PDFs.

**Workaround:** Run OCR upstream (so the PDF carries a text layer before CRW sees it), or convert Office documents to PDF first. For scanned-only archives, a dedicated OCR pipeline remains the right tool.

**Check before you rely on it:** `GET /v1/capabilities` reports `documents.parsers` and `documents.fileUpload.maxBytes` (the enforced cap) for the instance you are talking to.

## Limitation 3: Anti-Bot Is Not Best-in-Class

**Current state:** CRW has basic anti-bot measures — realistic user agents, request header mimicry, cookie handling, and configurable delays between requests. For most publicly accessible content this is sufficient. For aggressively protected sites using Cloudflare Enterprise, PerimeterX, DataDome, or Akamai Bot Manager, CRW may receive blocks more often than specialized anti-bot services.

**Why it's limited:** Effective anti-bot bypass at the highest level requires residential proxy networks, TLS fingerprint spoofing (matching browser JA3/JA4 signatures), browser behavioral mimicry (mouse movements, scroll patterns), and CAPTCHA solving integrations. These are expensive, legally complex, and require ongoing maintenance as detection systems evolve. CRW's focus is the 80% of scraping tasks that don't require sophisticated anti-bot — public docs, news, product pages, open APIs.

**Who it affects:** Teams scraping high-protection targets: e-commerce with active price-scraping protection, social media platforms, financial data sites, and any site actively investing in scraper detection.

**Workaround:** For difficult targets, combine CRW with an external residential proxy network. CRW supports HTTP/HTTPS proxy configuration:

```
curl -X POST http://localhost:3000/v1/scrape   -H "Authorization: Bearer YOUR_API_KEY"   -H "Content-Type: application/json"   -d '{
    "url": "https://protected-site.com",
    "formats": ["markdown"],
    "proxy": "http://user:pass@residential-proxy.example.com:8080"
  }'
```

This separates the IP/fingerprint layer (handled by your proxy provider: Oxylabs, BrightData, Smartproxy) from the content extraction layer (handled by CRW). You get anti-bot bypass without requiring CRW to bundle a full anti-detection stack.

**Roadmap:** We're evaluating built-in proxy rotation via configurable proxy pools. Native CAPTCHA solving is not planned — this is better handled at the proxy/browser layer.

## Limitation 4: SPA Coverage Is Inconsistent

**Current state:** CRW's JavaScript rendering via LightPanda works for many SPAs, but reliability on complex React/Vue/Angular applications is lower than Playwright-based alternatives. Some pages with heavy client-side routing, lazy loading, or browser APIs not yet implemented in LightPanda may return incomplete content.

**Why it's inconsistent:** LightPanda is an experimental browser written in Zig that implements a subset of browser APIs. It's not Chromium — it doesn't have the full W3C API surface area. Pages that depend on APIs not yet implemented (e.g., some Web Animations API methods, certain Canvas operations, WebRTC) may not render correctly.

**How to detect it:** If a scrape returns thin content (fewer than 200 words) for a page you know has substantial content, or if the markdown is clearly navigation text rather than page content, LightPanda likely failed to render the application state. Compare with a manual browser visit.

**Workaround:** For URLs where content quality is critical and the page is JavaScript-heavy, route to Firecrawl or use Playwright directly. You can detect this conditionally:

```
async function scrapeWithFallback(url: string) {
  const result = await crwScrape(url);

  // If markdown is thin, fall back to Firecrawl
  if (result.data.markdown.split(" ").length < 200) {
    console.warn(`Thin content for ${url}, falling back to Firecrawl`);
    return firecrawlScrape(url);
  }

  return result;
}
```

**Roadmap:** Improving LightPanda SPA coverage is the highest-priority gap in CRW's core scraping capability. We're actively contributing to LightPanda and tracking its API surface area expansion. This is an active area of development.

## Limitation 5: No WebSocket / SSE Streaming for Crawl Progress

**Current state:** The crawl progress API uses polling — you call `GET /v1/crawl/:id` repeatedly until the job completes. There's no WebSocket or Server-Sent Events stream for real-time progress updates.

**Why it's missing:** Polling is simpler to implement, has no persistent connection management, and works through proxies and load balancers without special configuration. For most backend automation use cases, polling every 2 seconds is perfectly adequate.

**Who it affects:** UI-heavy applications that want to show a real-time progress bar ("Crawled 47/100 pages...") without the overhead of polling. Also affects very large crawls where you want early results rather than waiting for completion.

**Workaround:** Poll at a short interval and expose progress to your UI through your own backend:

```
// Server: poll CRW and push to client via your own SSE stream
app.get("/crawl-progress/:jobId", async (req, res) => {
  res.setHeader("Content-Type", "text/event-stream");

  const poll = async () => {
    const status = await fetch(`http://crw:3000/v1/crawl/${req.params.jobId}`);
    const data = await status.json();

    res.write(`data: ${JSON.stringify(data)}

`);

    if (data.status !== "completed" && data.status !== "failed") {
      setTimeout(poll, 1500);
    } else {
      res.end();
    }
  };

  poll();
});
```

**Roadmap:** SSE (Server-Sent Events) for crawl progress is planned as a simpler alternative to WebSockets. This will allow real-time progress without full-duplex WebSocket complexity.

## Limitation 6: No Persistent Job Storage

**Current state:** Crawl job state is stored in memory. If CRW restarts — planned or unplanned — all in-progress crawl jobs are lost. The job ID becomes invalid and the status endpoint returns 404.

**Why it's missing:** Persistent storage means adding an external dependency (Redis, SQLite, PostgreSQL). This conflicts with the core design goal: a single binary with no external dependencies. Every dependency is an operational burden.

**Who it affects:** Teams running long crawl jobs (multi-hour site indexing) where an unexpected CRW restart would require restarting the entire crawl from scratch.

**Workaround:** Break large crawls into smaller batches and manage state externally:

```
// Instead of one 1000-page crawl, do 10 x 100-page crawls
// and track completion in your own database
const batches = urlBatches(allUrls, 100);
for (const batch of batches) {
  const jobId = await startCrawl(batch[0], { limit: 100 });
  await waitForCompletion(jobId);
  await saveResultsToDb(jobId);
  console.log(`Completed batch, ${batches.indexOf(batch) + 1}/${batches.length}`);
}
```

Also use a process manager that restarts CRW quickly: `docker run --restart unless-stopped` or a systemd unit with `Restart=on-failure`.

**Roadmap:** Optional SQLite-backed job persistence is planned as an opt-in feature. The default behavior (in-memory, no dependencies) will be preserved.

## Limitation 7: No Built-in Retry with Backoff

**Current state:** CRW does not automatically retry failed requests. If a page returns a 429 (rate limited), 503 (service unavailable), or a network timeout, the scrape fails immediately and the error is returned to the caller.

**Why it's missing:** Retry logic with backoff is best implemented at the client layer where you can make informed decisions about which errors to retry and how aggressively. Server-side retry can mask important signals (persistent 403s, quota exhaustion).

**Workaround:** Implement retry in your client code:

```
async function scrapeWithRetry(url: string, maxRetries = 3) {
  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    try {
      const res = await fetch("http://localhost:3000/v1/scrape", { // or https://api.fastcrw.com/v1/scrape for cloud
        method: "POST",
        headers: {
          "Authorization": "Bearer YOUR_API_KEY",
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ url, formats: ["markdown"] }),
      });
      const data = await res.json();
      if (data.success) return data;
      if (res.status === 429) {
        const wait = Math.min(1000 * Math.pow(2, attempt), 30000);
        await new Promise(r => setTimeout(r, wait));
        continue;
      }
      throw new Error(data.error);
    } catch (err) {
      if (attempt === maxRetries) throw err;
      await new Promise(r => setTimeout(r, 1000 * attempt));
    }
  }
}
```

**Roadmap:** Configurable per-request retry is planned for a future release. When added, it will be opt-in with explicit retry count and backoff strategy parameters.

## Limitation 8: No Response Caching

**Current state:** CRW does not cache scraped responses. Every call to `/v1/scrape` performs a fresh HTTP fetch, regardless of how recently that URL was scraped. This means scraping the same URL twice costs twice the network round-trips.

**Workaround:** Add a caching layer in front of CRW. A Redis-backed approach works well:

```
import { createClient } from "redis";

const redis = createClient();
const TTL = 60 * 60; // 1 hour cache

async function cachedScrape(url: string) {
  const cacheKey = `scrape:${url}`;
  const cached = await redis.get(cacheKey);
  if (cached) return JSON.parse(cached);

  const result = await fetch("http://localhost:3000/v1/scrape", { // or https://api.fastcrw.com/v1/scrape for cloud
    method: "POST",
    headers: {
      "Authorization": "Bearer YOUR_API_KEY",
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ url, formats: ["markdown"] }),
  }).then(r => r.json());

  await redis.setEx(cacheKey, TTL, JSON.stringify(result));
  return result;
}
```

**Roadmap:** Optional HTTP response caching with configurable TTL is planned. When added, it will be opt-in to avoid surprising behavior for use cases that require fresh content.

## Honest Summary: Limitations Matrix

| Limitation | Severity | Workaround Available | Roadmap Priority |
| --- | --- | --- | --- |
| Screenshots need a Chrome-class tier | Low | ✅ Configure `[renderer.chrome]` | Shipped |
| PDF only, and no OCR | Medium | ✅ OCR upstream / convert to PDF | Low |
| Anti-bot not best-in-class | Medium | ✅ Residential proxy | Medium |
| SPA coverage inconsistent | High | ⚠️ Fallback to Firecrawl | High |
| No crawl progress streaming | Low | ✅ Client-side polling proxy | Medium |
| No persistent job storage | Medium | ✅ External state tracking | Medium |
| No built-in retry | Low | ✅ Client-side retry | Low |
| No response caching | Low | ✅ Redis cache layer | Low |

## What CRW Is Good At Today

Despite these gaps, CRW works well for a specific, well-defined set of use cases:

- Scraping HTML-primary content to clean markdown for LLMs
- Building RAG pipelines from websites and documentation
- Exposing web scraping to AI agents via MCP
- Self-hosting a Firecrawl-compatible API with minimal infrastructure
- High-concurrency scraping of publicly accessible content
- Low-memory self-hosting as a sidecar to existing applications

If your use case falls in this set, CRW's limitations are unlikely to be blocking. Screenshots and PDF parsing both ship today (screenshots need a Chrome-class renderer tier; PDF only, no OCR, no DOCX). What is still genuinely thin is OCR and non-PDF document formats — if you need those, use the tools that do them well.

## How We Prioritize the Roadmap

GitHub issues drive our priority decisions. If a specific limitation is blocking your use case, [open an issue](https://github.com/fastcrw/crw/issues) with your use case description. Limitations with multiple real-world users asking for them get prioritized. The roadmap reflects what developers are actually trying to do with CRW, not what sounds impressive in a feature list.

## Try CRW

### Open-Source Path

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

[GitHub](https://github.com/fastcrw/crw) · [Docs](https://us.github.io/crw)

### Hosted Path — fastCRW

[fastCRW](https://fastcrw.com) — 1000 free credits, no credit card required.

## FAQ

### What can't CRW do?

The main gaps today: OCR over scanned documents, DOCX and XLSX parsing, best-in-class anti-bot bypass, reliable rendering of complex SPAs, WebSocket crawl-progress streaming, persistent job storage across restarts, and automatic request retry. Screenshots and PDF parsing do ship, each with a boundary: capture needs a Chrome-class browser tier, and PDF parsing reads the text layer only. Each limitation has a documented workaround. The limitations matrix in this post lays out severity and roadmap priority for each.

### Is CRW production-ready?

CRW is production-ready for its target use cases: scraping HTML content to clean markdown, building RAG pipelines, exposing web scraping via MCP, and self-hosting a Firecrawl-compatible API. It is not the right fit for OCR over scanned documents, Office-format parsing, or scraping highly-protected sites without external proxy support. If your workflow stays within the supported use cases, the limitations listed here are unlikely to block you.

### Does CRW support screenshots?

Yes, on an instance with a Chrome-class browser tier. `formats: ["screenshot"]` returns a base64 PNG data URL, and `screenshot@fullPage` captures the full page. Capture runs over CDP, so it needs Chrome (`chrome`, `chrome_proxy`) or Playwright; LightPanda and Camoufox cannot capture, and an instance holding only those refuses the request rather than returning an empty image. Call `GET /v1/capabilities` and check `screenshot.supported` for the deployment you are pointed at.

### Can CRW scrape JavaScript-heavy sites and SPAs?

CRW renders many JavaScript-heavy pages through LightPanda, an experimental browser written in Zig that implements a subset of browser APIs. Reliability is good for typical React, Vue, and Angular SPAs but lower than Playwright-based tools on complex apps with heavy client-side routing or unimplemented browser APIs. SPA coverage is the highest-priority gap in CRW's core scraping; for pages where content quality is critical, use a thin-content fallback to Firecrawl.

### Does CRW support PDF parsing?

Yes. A PDF URL passed to /v1/scrape is auto-routed to the parser by content type, and files can be uploaded to the parse endpoint. It is PDF only — DOCX and XLSX are not parsed — and there is no OCR engine, so a scanned, image-only PDF has no text layer to read and yields empty or partial text with a warning. Run OCR upstream for scanned archives, and convert Office documents to PDF first.

### How fast is CRW?

On Firecrawl's public 1,000-URL scrape-content dataset (diagnose_3way.py, 2026-05-08), CRW posted the best median latency at 1914 ms versus Firecrawl's 2305 ms. One real limitation: CRW does not yet have built-in retry with backoff, so handle 429s and timeouts in your client code.
