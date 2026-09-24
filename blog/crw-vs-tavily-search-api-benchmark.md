# CRW vs Firecrawl vs Tavily: 200-Query Benchmark (Search + Scrape)

> We benchmarked CRW against Firecrawl and Tavily on a labeled public dataset: 63.74% truth-recall (522 of 819 labeled URLs), ~92% scrape success of reachable URLs, 0 errors. Full latency distribution and a one-command repro on /benchmarks.

**Published:** 2026-04-04  
**Updated:** 2026-05-09  
**Canonical:** https://fastcrw.com/blog/crw-vs-tavily-search-api-benchmark

---

**May 2026 note.** This comparison reflects our April 2026 run. The full latency distribution and dataset live on [/benchmarks](/benchmarks) with a committed one-command re-run script (query mix per class, region, sample size, p50/p95/p99 with confidence intervals, cache state, timeouts) so updates stay reproducible. If a rerun produces a material delta in either direction, we will update this post and the [Tavily alternatives hub](/alternatives/tavily) honestly. The compatibility verdict on Tavily — **Tavily-style with a 30-line adapter, not drop-in** — is documented in the [compatibility matrix](/alternatives/tavily#compatibility-matrix).

We benchmarked **CRW, Firecrawl, and Tavily** head-to-head on both search and scrape — a labeled query and URL set, all run concurrently against all three providers under the same dataset, different run conditions.

Here's what we found, and how to reproduce it yourself.

## Search and Scrape: The Defensible Result

Rather than freeze a point-in-time latency table — numbers that drift with every provider release and every network condition — we publish the full latency distribution (p50/p95/p99 per provider, per run) alongside the labeled dataset and a one-command repro on our public [/benchmarks](/benchmarks) page.

The durable, defensible finding from that run: **63.74% truth-recall (522 of 819 labeled URLs), 91.8% scrape success (of reachable URLs), 0 errors**. On search, CRW returns results with lower, more predictable latency than the alternatives because it aggregates engines in parallel rather than serializing through a single upstream. On scrape, CRW's lightweight Rust-based renderer is lower-latency than a full-Chromium pipeline while still handling JavaScript-heavy pages that simpler extractors can't.

The tail behavior is what matters for user-facing AI agents: CRW's p95 stays close to its median, so occasional slowness is rare — the difference between "instant" and "noticeable" in an interactive flow.

## The Full Picture: CRW's Position

CRW is the only provider in this comparison that covers **both** search and scrape with a low-latency, local-first architecture:

| Capability | CRW | Details |
| --- | --- | --- |
| **Search latency** | Lower, more predictable | Parallel engine aggregation; full distribution on /benchmarks |
| **Scrape latency** | Lower (no Chromium in path) | Lightweight Rust renderer; full distribution on /benchmarks |
| **JS rendering** | Yes — via LightPanda | Lightweight Zig-based browser, not heavy Chromium |
| **Search + Scrape** | Single API call | `scrapeOptions` fetches full content from search results |
| **Self-hosting** | Single small static binary | AGPL-3.0 Rust binary — run on your infra for free |
| **Pricing** | Lower cost per request | CRW Standard $69/mo for 100K credits; Tavily Researcher $100/mo for 12K searches |

## What We Tested

### Search: 100 Queries Across 10 Categories

We designed a dataset of 100 queries spanning 10 distinct categories to simulate real-world AI agent usage patterns:

| Category | Queries | Example |
| --- | --- | --- |
| Programming | 15 | "Next.js 15 server actions best practices" |
| AI / Machine Learning | 15 | "fine tuning LLM with LoRA QLoRA guide" |
| DevOps / Cloud | 12 | "kubernetes horizontal pod autoscaler custom metrics" |
| Current Events | 10 | "SpaceX Starship latest launch update" |
| Product Research | 10 | "Supabase vs Firebase vs PocketBase comparison" |
| Security | 8 | "post-quantum cryptography NIST standards" |
| Scientific | 8 | "CRISPR gene editing clinical trials results" |
| Niche / Long-tail | 12 | "eBPF XDP packet processing Linux kernel" |
| Business / Startup | 5 | "SaaS pricing strategies freemium vs usage based" |
| Multilingual | 5 | "yapay zeka ile web kazıma otomasyonu" (Turkish) |

### Scrape: 101 URLs Across Major Categories

We scraped 101 URLs spanning frameworks (React, Vue, Svelte, Angular), languages (Rust, Go, Python, TypeScript, Zig), databases (PostgreSQL, Redis, MongoDB), cloud providers (AWS, GCP, Azure), AI tools (OpenAI, Anthropic, HuggingFace), and developer productivity tools (Figma, Linear, Notion).

## Why CRW Is Faster

CRW's speed advantage comes from architectural decisions, not tricks:

- **Single-binary Rust core:** a small static binary with no headless-browser memory baseline. No JVM, no Python runtime, no Node.js overhead. Just fast compiled code handling your requests.
- **LightPanda renderer:** A headless browser built from scratch in Zig that handles JavaScript rendering at a fraction of Chromium's resource cost. This is why CRW's scrape latency stays low where a full-Chromium pipeline pays a render cost on every request.
- **Multi-engine aggregation:** CRW's search queries multiple engines simultaneously — the fastest response wins. This is why search latency is so consistent.
- **Minimal processing overhead:** Results are normalized and scored at the edge with minimal transformation. No AI post-processing on the hot path.

## What This Means for AI Agents

If you're building AI agents that search and scrape the web, latency compounds fast:

- **Multiple searches per agent run:** lower per-call latency compounds into a meaningfully faster run end-to-end
- **Many pages scraped per pipeline:** a no-Chromium scrape path keeps the whole batch faster and lighter
- **At scale across many runs per day:** the cumulative wall-clock saving is substantial — measure it for your own workload with the one-command repro on [/benchmarks](/benchmarks)

And because CRW supports [search + scrape in a single API call](https://docs.fastcrw.com/search), you can eliminate an entire round-trip that most agent architectures currently require:

```
import CRW from 'crw-js';

const crw = new CRW({ apiKey: 'your-key' });

// Search and scrape in one call — no separate scraping step
const results = await crw.search({
  query: "latest transformer architecture improvements",
  limit: 5,
  scrapeOptions: {
    formats: ["markdown"],
    onlyMainContent: true,
  },
});

// Each result includes full markdown content
for (const r of results.data) {
  console.log(r.title, r.markdown?.length, "chars");
}
```

## Methodology

Full transparency on how we ran this:

- **100 search queries** across 10 categories + **101 scrape URLs** across major web categories
- **Concurrent execution:** All 3 providers tested simultaneously per URL/query via `Promise.all` — no sequential advantage for any provider
- **5 results per search query** for all providers
- **Tavily advanced search depth** — we used Tavily's best mode, not basic
- **Markdown format** for scrape results across all providers
- **Single run** from the same network location, same time of day
- **Full results:** 124KB JSON report with per-URL and per-query data

The benchmark script, dataset, and full results are [open source](https://github.com/fastcrw/crw). Run it yourself — we encourage independent verification.

## Try It Yourself

CRW's Search and Scrape APIs are live with a [one-time lifetime 1000 credits](/pricing) (not a monthly meter) — no card required.

```
# Search — parallel engine aggregation, low latency
curl -X POST https://api.fastcrw.com/v1/search \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"query": "best search API for AI agents", "limit": 5}'

# Scrape — no Chromium in the request path, JS rendering included
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com", "formats": ["markdown"]}'
```

Or [self-host the entire stack](https://github.com/fastcrw/crw) for free. Same APIs, your infrastructure.

## Frequently Asked Questions

### Is CRW a drop-in replacement for Firecrawl?

Pretty close. CRW's API is Firecrawl-compatible: same endpoint patterns (`POST /v1/scrape`, `POST /v1/search`), same JSON body structure. Migration is typically under 15 minutes — change the base URL and API key, and you're done.

### How does CRW handle JS-heavy pages?

CRW uses LightPanda, a headless browser built from scratch in Zig, which renders JavaScript without the resource overhead of Chromium. This keeps scrape latency low — it gets the benefits of JS rendering without the weight of a full browser.

### How does pricing compare?

CRW Standard ($69/mo) gives 100K credits. Firecrawl Growth ($188/mo) gives 100K credits. Tavily Researcher ($100/mo) gives 12K searches. At equivalent tiers, CRW's cost per request is substantially lower than both, and CRW can also be self-hosted for free under AGPL-3.0.

### Can I run the benchmark myself?

Absolutely. Run `bun benchmarks/triple-bench.ts` from the repo. Add your own API keys and verify independently. The full dataset and results JSON are included.
