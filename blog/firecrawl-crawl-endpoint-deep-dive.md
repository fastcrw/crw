# Firecrawl /crawl Deep Dive: Jobs, Limits, Credit Cost, and Safe Patterns (2026)

> Everything about Firecrawl's crawl endpoint — the async job model, depth and page limits, why crawl is the biggest credit sink, polling patterns, and how the same crawl works against a Firecrawl-compatible engine.

**Published:** 2026-05-20  
**Updated:** 2026-05-20  
**Canonical:** https://fastcrw.com/blog/firecrawl-crawl-endpoint-deep-dive

---

*By the fastCRW team · Last reviewed 2026-05-18*

**Disclosure:** fastCRW is a Firecrawl-compatible scraper built by the author. The crawl mechanics and code here also run against fastCRW via a base-URL change.

## Crawl is scrape, multiplied — and that's the whole risk

Crawl discovers a site's pages and scrapes each one. Conceptually simple; operationally it's the endpoint that most often produces a surprise bill and a runaway job, because **cost and duration scale with site size, not with the number of API calls you made**. One crawl request against a 20,000-page site is one call and twenty thousand credits. Internalize that before anything else.

## The async job model

Crawl is not request/response — it's submit-then-poll on both Firecrawl and fastCRW:

```
# 1. submit
POST /v1/crawl
{ "url": "https://docs.example.com", "limit": 200 }
-> { "id": "job_abc" }

# 2. poll
GET /v1/crawl/job_abc
-> { "status": "scraping", "completed": 40, "total": 200, "data": [ ... ] }
-> { "status": "completed", "completed": 200, "total": 200, "data": [ ... ] }
```

Design your client around three states: started, working (with progress), terminal (completed/failed). Don't hardcode assumptions about intermediate status strings or result chunk sizes beyond that lifecycle — those are exactly the details that can differ between compatible engines, so key your logic on the lifecycle, not the wire trivia.

## The controls that keep a crawl bounded

An unbounded crawl is an unbounded bill and an unbounded runtime. Always set explicit bounds:

- **`limit`** — hard ceiling on pages crawled. The single most important guardrail. Set it deliberately, never omit it in production.
- **Depth** — how many link-hops from the seed. Most documentation/content sites need only 2–3; deeper mostly adds noise and cost.
- **Include / exclude path patterns** — restrict to the section you actually want (e.g. only `/docs/*`, exclude `/blog/*` and `/changelog/*`). This is the highest-leverage cost control after `limit`.
- **Per-page scrape options** — apply the narrowest `formats` to every crawled page (markdown only, usually). Crawl multiplies whatever per-page work you request.

## Map first, then crawl

The professional pattern: call `/v1/map` first to discover the URL set cheaply, inspect the count and shape, *then* crawl with a `limit` sized to what map told you. Crawling blind is how teams discover a "small docs site" was 12,000 pages after the credits are already spent. Map is the cheap reconnaissance that makes crawl predictable.

```
POST /v1/map  { "url": "https://docs.example.com" }
-> { "links": [ /* full URL list */ ] }
# inspect length and patterns, choose limit + include rules, THEN:
POST /v1/crawl { "url": "https://docs.example.com",
                 "limit": <known_count>,
                 "includePaths": ["^/docs/"] }
```

## Why crawl is the credit sink

At ~1 credit per crawled page, crawl dominates spend for any team doing site-scale ingestion. The cost traps:

- **Recrawls** of the same site repeat the full page cost every run. Use change-detection or incremental crawl windows where possible; don't re-pay for a static knowledge base nightly.
- **Pagination explosions** — faceted search, calendars, and infinite-scroll archives can generate thousands of near-duplicate URLs. Exclude them with path patterns.
- **Tier ceilings** — a few large crawls can blow past a monthly credit cap and force a tier jump (e.g. Firecrawl Standard 100k → Growth at $333/mo). Forecast the worst crawl month, not the average.
- **Extraction on every page** — if you extract structured JSON per crawled page on Firecrawl, the separate extract subscription compounds the crawl bill. fastCRW keeps JSON extraction inside the same per-page credit, no second subscription.

## A robust polling loop

```
import time

def crawl_and_wait(client, url, limit, timeout_s=1800):
    job = client.crawl_url(url, params={"limit": limit})
    job_id = job["id"]
    deadline = time.time() + timeout_s
    delay = 2
    while time.time() < deadline:
        s = client.check_crawl_status(job_id)
        if s["status"] == "completed":
            return s["data"]
        if s["status"] == "failed":
            raise RuntimeError(f"crawl failed: {job_id}")
        time.sleep(delay)
        delay = min(delay * 1.5, 30)  # backoff, capped
    raise TimeoutError(f"crawl {job_id} exceeded {timeout_s}s")
```

Note the capped exponential backoff (don't hammer the status endpoint) and the hard timeout (a crawl with no deadline is an operational liability). This loop is backend-agnostic — it works against Firecrawl or a Firecrawl-compatible engine unchanged.

## The same crawl, two backends

```
from firecrawl import FirecrawlApp

fc  = FirecrawlApp(api_key="fc-...", api_url="https://api.firecrawl.dev")
crw = FirecrawlApp(api_key="key",    api_url="https://your-fastcrw-host")

params = {"limit": 100, "includePaths": ["^/docs/"]}
a = fc.crawl_url("https://docs.example.com", params=params)
b = crw.crawl_url("https://docs.example.com", params=params)
# compare discovered URL set + page count to validate parity
```

When validating a migration, the crawl checks that matter are: same discovered URL set (or an explainable diff), comparable page count, and the same per-page document shape. Whitespace differences in markdown are expected and fine; missing main content or large coverage gaps are not.

## Self-hosting changes the crawl economics entirely

Every cost trap above assumes per-page metering. fastCRW's engine is a single ~10MB (compressed download) AGPL-3.0 Rust binary you can self-host with **unlimited requests and zero per-page credit**. For site-scale recurring crawls — the exact workload where credit cost compounds worst — that turns "what will this crawl cost" into "it runs on the box I already pay for." You keep the Firecrawl-compatible crawl API; you remove the meter. And because the managed cloud uses the same API, you can prototype managed and move heavy recurring crawls in-house later with a base-URL change, not a rewrite.

## Crawl safety checklist

1. Map the target first; size `limit` to the real count.
2. Always set `limit`, depth, and include/exclude patterns explicitly.
3. Request the narrowest per-page `formats` (markdown only unless you truly need more).
4. Cap polling with backoff and a hard timeout.
5. Make recrawls incremental; never re-pay for static content on a schedule.
6. Alert on credit burn slope, and keep a self-host fallback configured for the heaviest recurring crawls.

## Incremental crawling: the pattern that actually controls cost

The single highest-leverage crawl optimization is not tuning depth — it is not re-crawling what has not changed. Naive scheduled full crawls re-pay the entire page cost every run, forever. The incremental pattern:

1. **Persist a manifest** of every crawled URL with a content hash and last-seen timestamp.
2. **Re-map on schedule** (cheap) and diff the URL set to find added and removed pages.
3. **Re-scrape only:** new URLs, URLs whose lightweight signals (Last-Modified, sitemap lastmod, or a cheap HEAD/conditional fetch) indicate change, and a small rotating sample of the rest as a drift check.
4. **Skip re-embedding** any page whose content hash is unchanged.

For a stable knowledge base this can cut recurring crawl spend by an order of magnitude versus nightly full recrawls — and it is engine-agnostic, so it is worth building regardless of which Firecrawl-compatible backend you run.

## Crawl politeness and reliability

An aggressive crawl that gets your IP throttled costs you more than credits — it costs you coverage and data quality. Production-grade crawl hygiene:

- **Respect robots and a sane rate.** Hammering a target triggers anti-bot responses that degrade the very data you are paying to collect.
- **Bound concurrency per host**, not just globally. Fifty in-flight requests spread across many domains is fine; fifty against one small site is a self-inflicted outage.
- **Treat partial crawls as normal.** Large crawls will have some failed pages. Record them, retry the transient class with backoff, and surface a coverage percentage rather than pretending crawls are all-or-nothing.
- **Make crawls resumable.** A crawl that dies at page 9,000 of 10,000 should resume, not restart — restarting re-pays 9,000 credits for nothing.

These are properties of disciplined crawling on the open web, independent of vendor. The crawl job model both Firecrawl and fastCRW expose supports building this; the discipline is yours to add.

## When a crawl is the wrong tool entirely

Reaching for crawl reflexively is a common and expensive mistake. Before launching one, ask:

- **Do I actually need the whole site, or 30 known pages?** If the latter: map, filter, scrape the 30. Orders of magnitude cheaper than crawling 10,000 to use 30.
- **Is there a sitemap or API that gives me the structure for free?** Map leverages sitemaps; sometimes the site itself exposes exactly what you need without crawling at all.
- **Is the content I want even discoverable by crawling?** Login-walled or search-only content will not appear in a crawl no matter how deep — a different acquisition strategy is needed.

Crawl is the right tool for genuine site-scale ingestion and nothing else. Used judiciously, with map-first scoping and incremental refresh, it is predictable. Used reflexively, it is the endpoint most likely to produce the bill that makes someone question the whole pipeline. And for the heavy recurring site-scale case where it is genuinely the right tool, self-hosting the open-core engine removes the per-page meter that makes that exact workload expensive — same Firecrawl-compatible crawl API, no metering, on hardware you already pay for.

## Sources

- Firecrawl crawl docs: [docs.firecrawl.dev](https://docs.firecrawl.dev)
- fastCRW repo: [github.com/fastcrw/crw](https://github.com/fastcrw/crw)

Related: [Firecrawl /map deep dive](/blog/firecrawl-map-endpoint-deep-dive) · [Firecrawl credits & rate limits](/blog/firecrawl-credits-rate-limits)

## FAQ

### How much does a Firecrawl crawl cost?

Roughly 1 credit per crawled page, so the cost equals the site's crawled page count, not the number of API calls. A 20,000-page crawl is one request and about 20,000 credits. Map the site first and set an explicit limit to keep it bounded.

### How do I stop a Firecrawl crawl from running away?

Always set an explicit limit, constrain depth, and use include/exclude path patterns to restrict the crawl to the section you want. Map the site first so the limit is sized to the real page count rather than guessed.

### Can I avoid per-page crawl credits entirely?

Self-host fastCRW's open-core engine — a single ~10MB (compressed download) AGPL-3.0 Rust binary with unlimited requests and no per-page credit, exposing the same Firecrawl-compatible crawl API. Ideal for heavy recurring site crawls where metering compounds.
