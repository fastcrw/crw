# Why I Built CRW: A Lightweight Firecrawl-Compatible Scraper in Rust

> The story behind CRW — why Rust, why single-binary, and why Firecrawl-compatible for AI agent and RAG use cases.

**Published:** 2026-03-04  
**Updated:** 2026-03-04  
**Canonical:** https://fastcrw.com/blog/why-i-built-crw

---

## It Started With a 4.6-Second Wait

Last year I was building an AI agent. Its job was simple: scrape a web page, convert the content to clean markdown, feed it to an LLM. Classic RAG pipeline. The kind of thing you can describe in one sentence and prototype in an afternoon.

I started with Firecrawl. The API design is excellent — `/v1/scrape`, `/v1/crawl`, clean JSON responses, good docs. Everything worked fine in development. Then I hit production.

**4.6 seconds per page.** Single page. One URL. The agent was spending more time waiting for content than actually reasoning about it. With a 5-page research task, the wall-clock time was 20+ seconds just on fetching. The LLM inference itself was faster than the I/O upstream of it.

The latency had two sources: cold start overhead from the Node.js process, and the Playwright browser spin-up time. Even for a plain static HTML page with no JavaScript rendering needed, Firecrawl's architecture spins up a browser context because that's what the system is built around. For my use case — mostly documentation sites, Wikipedia pages, and standard news articles — this was architectural overhead I was paying every single time.

I tried switching to Crawl4AI. The speed was better, but now I had a different problem: deployment. Python + Playwright + Chromium = 2 GB Docker image, 300 MB idle RAM, 10 minutes to get running on a fresh server. For a tool that's supposed to be a lightweight sidecar to my main application, that felt wrong. I was already running a vector database, an embedding service, and my main API — the scraping layer was consuming more resources than any of them.

I looked at Spider.cloud. Fast, but closed-source. You don't control anything, and the pricing scales quickly. For an agent that was making 500–1,000 scrape calls per day, the per-request costs added up to something I didn't want to commit to for a project that might double in usage any given week.

I looked at writing a thin wrapper around `reqwest` + some HTML parsing crate. That took an evening. It was fast — much faster than anything else I'd tried. But it had no API, no crawl orchestration, no markdown conversion, and it certainly wasn't compatible with the Firecrawl SDKs my other tools were already using.

One evening I thought: how hard would it be to write a fast, Firecrawl-compatible scraper in Rust? Not a toy — a real API server with `/v1/scrape`, `/v1/crawl`, `/v1/map`, proper error handling, Docker image, and documentation.

Eight months later: CRW.

## Why Rust?

The honest answer: web scraping at its core is CPU-bound byte processing. Parsing HTML, traversing a DOM, extracting text, converting to markdown — these are all operations on bytes. Rust's zero-cost abstractions, lack of garbage collector pauses, and the quality of crates like **lol-html** make it a natural fit.

But the more complete answer is that Rust's constraint model forces you to think differently about resource usage. When you write a web server in Node.js, it's easy to accumulate allocations without noticing because the GC cleans them up. In Rust, every allocation is intentional. That discipline pays off in the runtime profile — not just peak performance, but steady-state memory usage under sustained load.

lol-html is a streaming HTML rewriter built by Cloudflare for use in Workers. It processes HTML in a single pass without building an in-memory DOM tree. The design is a handler-based API: you register callbacks for specific HTML elements, attributes, or text nodes, and lol-html calls them as the stream passes through. For content extraction — finding the main article body, stripping navigation and ads, converting headers to markdown — this streaming model is exactly what you need, and it's extremely fast because you never materialize the full DOM.

The result: CRW is local-first and low-latency, with the bulk of a static-page request being network time rather than engine overhead. That's the kind of profile where your AI agent is actually thinking more than it's waiting — see the full latency distribution and one-command repro on our [public benchmark](/benchmarks).

The memory story is even more dramatic. A Node.js + Playwright process carries a heavy resident browser. CRW is a small single binary with a tiny idle footprint. That's not a rounding error — it's a fundamental difference in runtime overhead. The same $20/month server that runs one Firecrawl instance can run many CRW instances serving concurrent requests.

## The Architecture Decisions

Several architectural choices in CRW are worth examining in detail, because they reflect real tradeoffs rather than just preference.

### lol-html vs. a Full DOM Parser

The most controversial choice is using `lol-html` instead of a full DOM parser like `scraper` or `html5ever`. A full DOM parser gives you XPath, CSS selectors, and the ability to traverse the document in any order. That flexibility comes at a cost: the entire DOM lives in memory simultaneously, and memory usage scales with document size.

lol-html's streaming model means memory usage is roughly constant regardless of document size — you process bytes and emit output without holding the whole tree. For pages up to tens of megabytes (which covers 99%+ of real-world scraping targets), the streaming approach is both faster and more memory-efficient.

The tradeoff is expressiveness. Some extraction tasks require non-linear DOM traversal — "find the `` element, but only if it comes after a `` with class `site-header`." lol-html's streaming model makes these harder to express. In practice, for the content-extraction use case CRW is optimized for, the heuristics (strip known noise elements, keep semantic content elements, convert to markdown) are simple enough that the streaming model is sufficient.

### Axum as the Web Framework

Axum is Tokio's first-party web framework. It's built on Tower middleware, which means composing features like rate limiting, authentication, request logging, and timeout handling is done via standard Tower layers rather than bespoke middleware APIs. This matters for long-term maintainability.

The alternatives I considered: Actix-web has better raw benchmark numbers but a different concurrency model (Actix actors vs. pure Tokio futures). Warp is elegant but has a reputation for cryptic type errors. Axum felt like the right balance of ergonomics, ecosystem alignment with Tokio, and long-term support.

The practical consequence: adding API key authentication to CRW was about 20 lines of Tower middleware, not a framework-specific plugin. Adding rate limiting will be similarly contained when it lands.

### Tokio's Async Model

CRW uses Tokio as its async runtime. Web scraping is I/O-bound — you're mostly waiting on network requests — so async concurrency is the right model. A single CRW process can handle many concurrent scrape requests, with each request yielding while waiting on the network and resuming when bytes arrive.

The specific Tokio configuration CRW uses is a multi-threaded runtime with the number of worker threads set to the CPU core count. This means CPU-bound work (the lol-html HTML processing) is parallelized across cores, while I/O work is multiplexed efficiently. In practice, on a 2-core server, CRW can handle burst concurrency of 50–100 in-flight requests without queuing.

### Single Binary vs. Microservices

CRW is a single binary that includes the REST API server, the MCP server, the crawl orchestrator, and the HTML extraction logic. No separate queue process. No separate worker pool. No Redis. No external state store required.

The tradeoff: you can't scale the crawl worker independently of the API layer. If you're doing massive crawls (tens of thousands of URLs), you might want a dedicated crawl fleet with separate API gateway nodes. CRW isn't designed for that scale today.

What the single-binary model gives you is everything you need for the typical AI infrastructure use case: one command to start, one process to monitor, one thing that can fail. The operational simplicity is a feature, not a limitation — it's the right default for the 99% of deployments that are one server, one scraping API, one team.

## What the First Version Looked Like

The first working prototype was about 200 lines of Rust. No API server — just a function that took a URL, fetched it with `reqwest`, ran the HTML through a basic lol-html pipeline, and returned a string. It looked roughly like this:

```
use lol_html::{element, rewrite_str, RewriteStrSettings};
use reqwest::Client;

async fn scrape_to_text(url: &str) -> anyhow::Result<String> {
    let client = Client::new();
    let html = client.get(url).send().await?.text().await?;

    let mut output = String::new();

    let result = rewrite_str(
        &html,
        RewriteStrSettings {
            element_content_handlers: vec![
                // Strip noise elements
                element!("nav, footer, aside, script, style, noscript", |el| {
                    el.remove();
                    Ok(())
                }),
                // Preserve headings as markdown
                element!("h1, h2, h3, h4, h5, h6", |el| {
                    let level = el.tag_name().chars().last()
                        .unwrap_or('1')
                        .to_digit(10)
                        .unwrap_or(1);
                    let prefix = "#".repeat(level as usize);
                    el.prepend(&format!("

{} ", prefix), lol_html::html_content::ContentType::Text);
                    el.append("
", lol_html::html_content::ContentType::Text);
                    Ok(())
                }),
            ],
            ..RewriteStrSettings::default()
        },
    )?;

    // Strip remaining HTML tags with a simple pass
    output = result
        .split('<')
        .enumerate()
        .map(|(i, s)| {
            if i == 0 { s.to_string() }
            else { s.splitn(2, '>').nth(1).unwrap_or("").to_string() }
        })
        .collect::<Vec<_>>()
        .join("");

    Ok(output.trim().to_string())
}
```

It was fast. Embarrassingly fast compared to anything Python-based. That was the proof-of-concept moment that made the project real: a 50-line function that outperformed a 2 GB Docker container.

The path from that to a full API was about three months of evenings. Axum API server, proper lol-html pipeline with semantic markdown conversion, crawl orchestrator with depth/limit controls, Docker build, and documentation. Each piece was individually straightforward; the integration work was where time went.

## The Firecrawl Compatibility Rabbit Hole

Implementing Firecrawl compatibility was more involved than I expected. The public docs describe the happy-path API, but production compatibility means handling all the edge cases: optional fields, array vs. string format parameters, error response shapes, crawl status state machines.

The `/v1/crawl` endpoint was the most complex piece. Firecrawl's crawl is asynchronous — you POST to start a crawl, get back a job ID, and poll `GET /v1/crawl/:id` until the status is `completed`. The response shape during polling includes partial results — pages already scraped — rather than waiting until everything is done to return anything. Implementing this correctly with Tokio meant building a small in-memory job store, spawning the crawl as a background task, and streaming results into the job record as they completed.

The `scrapeOptions` nesting was another detail that bit me. In Firecrawl's crawl endpoint, per-page scrape configuration goes inside a `scrapeOptions` object, whereas the scrape endpoint takes these fields at the top level. Getting the deserialization right for both shapes took a few iterations.

The test I used: take a sample application using the Firecrawl JavaScript SDK and point `apiUrl` at my local CRW instance. When the SDK tests passed without modification, I considered the endpoint compatible. This approach surfaced a dozen subtle issues that reading the docs alone wouldn't have caught.

The result of getting this right: anyone already using a Firecrawl SDK can migrate to CRW with a one-line change. That migration path is a genuine forcing function — it means CRW gets tested against real Firecrawl-shaped workloads constantly.

## Why MCP Changed Everything

The Model Context Protocol (MCP) was announced while CRW was in development. I'd been planning to ship just the REST API and MCP was a late addition — and it turned out to be the feature that changed how people used the project.

Before MCP, using CRW from an AI agent required HTTP plumbing: your agent code made fetch calls, parsed JSON, injected content into the prompt. That's not hard, but it's boilerplate that every agent has to reimplement. The agent has to know about headers, error codes, response shapes.

With MCP, the agent sees CRW's tools — `scrape`, `crawl`, `map`, `search`, and more. It calls them with parameters. It gets back content. The transport layer is invisible. Claude Desktop, Cursor, and any other MCP-compatible AI client can use CRW without the user writing a single line of API integration code.

What I didn't anticipate was how this changes the *population* of people who can use the tool. Shipping an MCP server means researchers, writers, and analysts who use Claude but don't write API integrations can get live web data in their AI workflows. That's a different audience than "developers building RAG pipelines" — and it's a much larger one.

The MCP server ships as `crw mcp` — a subcommand of the same binary. For self-hosted users, you run the binary and configure your MCP client to point at it. For fastCRW users, the `crw-mcp` npm package wraps the fastCRW API in an MCP server, so you get the proxy network and managed infrastructure behind the same MCP tool interface.

## Six Months of Benchmarking

One of the side effects of building a scraper that emphasizes performance is that you end up doing a lot of benchmarking. Some things I learned:

**The headline average hides a bimodal distribution.** Static HTML pages are dominated by network time; JavaScript-rendered SPAs via LightPanda are meaningfully slower because of the rendering step. The blended average reflects a corpus that mixes mostly static pages with a minority of SPAs. If your workload is entirely static documentation, expect consistently low single-page latency — the full distribution and a one-command repro are on our [public benchmark](/benchmarks).

**Network latency dominates for single-page scrapes.** For a server in US-East scraping a US-hosted page, network RTT is 20–50 ms. For a European server scraping a European page, similar. But cross-continental scraping (US server, Asian target) can add 150–300 ms of latency that has nothing to do with CRW's processing speed.

**Concurrency scaling is nearly linear up to ~50 simultaneous requests.** Beyond that, the bottleneck shifts to outbound network bandwidth rather than CPU. CRW's processing overhead per request is small enough that it's not the bottleneck — the Internet is.

**Memory usage under load stays proportional to active request count, not total request count.** There's no significant memory growth over hours of sustained load. The GC-free Rust runtime means there's no accumulation of unreachable memory waiting for collection.

We wrote up the full benchmark methodology and numbers in a dedicated post: [CRW benchmarks: latency, memory, and throughput at scale](/blog/benchmark-crw).

## The Open Source Decision

CRW is licensed under AGPL-3.0. The choice was deliberate and worth explaining.

MIT would have been simpler — permissive, no obligations. But MIT would also allow someone to take CRW, add a few features, and sell it as a competing hosted product without contributing anything back. Given that CRW is explicitly designed to be the engine behind a hosted service (fastCRW), MIT felt like inviting that outcome.

GPL would require modifications to be open-sourced, but doesn't address the SaaS case — you can run GPL software as a service without releasing your modifications. That's the specific scenario AGPL closes: if you run CRW as a network service, your modifications must be open-sourced.

AGPL-3.0 is the right license for this project: it keeps CRW free for self-hosters, requires network-service operators to open-source their modifications, and creates a clear boundary between the open-source project and the fastCRW commercial layer. fastCRW is built on CRW but adds the proxy network, managed infrastructure, and user management — none of which are in the open-source core.

The commercial layer funds development. Open source doesn't mean the project is a hobby — it means the community can audit, trust, and contribute to the core while the hosted service provides the revenue to make continued development viable.

## What CRW Is Not

I want to be clear about what CRW is not, because overpromising is how you lose trust.

CRW is **not** the best anti-bot bypass tool. There are specialized services with better proxy rotation and CAPTCHA solving for high-protection targets. If you're scraping sites with aggressive bot detection, CRW will get blocked on the same requests any standard HTTP client would fail on. fastCRW's proxy network helps, but it's not a dedicated anti-bot platform.

CRW captures screenshots and parses PDFs, but both come with a boundary. Screenshots need a Chrome-class browser tier (LightPanda and Camoufox cannot capture), and PDF parsing reads the text layer only — there is no OCR, and no DOCX or XLSX. If your pipeline needs OCR over scanned documents or Office-format extraction, another tool is the better fit today.

CRW's SPA support via LightPanda is functional but newer than Playwright. For the most complex JavaScript-heavy applications — single-page apps that require multiple interactions before content loads, or sites that detect headless browsers aggressively — Firecrawl or Crawl4AI may be more reliable today.

CRW is the right tool when you want a **lightweight, self-hosted, Firecrawl-compatible API for clean content extraction** — especially in AI agent and RAG contexts where latency and deployment simplicity matter more than browser automation capability.

## Try It

### Open-Source Path — Self-Host for Free

CRW is AGPL-3.0 licensed. Run it anywhere:

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

Or install the CLI directly:

```
cargo install crw
```

Source code: [github.com/fastcrw/crw](https://github.com/fastcrw/crw) · [Documentation](https://us.github.io/crw)

### Hosted Path — fastCRW

If you want the managed version with proxy networks and auto-scaling: [fastcrw.com](https://fastcrw.com) — 1000 free credits, no credit card required.

## Further Reading

- [CRW vs Firecrawl: detailed feature comparison](/blog/firecrawl-vs-crawl4ai-vs-crw)
- [CRW vs Crawl4AI: API-first vs framework](/blog/crw-vs-crawl4ai)
- [How to self-host a Firecrawl-compatible API with CRW](/blog/self-host-firecrawl-api)
- [How to expose web scraping to AI agents with MCP](/blog/mcp-web-scraping)
- [How to build a RAG pipeline from websites using CRW](/blog/rag-pipeline-with-crw)

## Frequently Asked Questions

### Why is CRW written in Rust?

Web scraping is CPU-bound byte processing — parsing HTML, extracting text, converting to markdown. Rust's zero-cost abstractions and lack of garbage collector pauses make it well-suited to this workload. Practically, CRW has a tiny idle footprint compared with Node.js + Playwright-based scrapers that carry a resident browser. The crate ecosystem — especially lol-html for streaming HTML processing and Tokio for async I/O — made Rust the natural choice.

### Is CRW stable enough for production?

CRW is used in production for HTML scraping, RAG pipelines, and MCP-connected AI agents. For static HTML extraction — documentation sites, news articles, standard web pages — it's reliable. Current gaps include screenshots, PDF extraction, and some complex JavaScript SPAs. If your workload fits within those boundaries, CRW is production-ready. See the [limitations post](/blog/crw-limitations) for an honest current-state assessment before committing.

### What's the difference between CRW and fastCRW?

CRW is the open-source Rust scraping engine licensed under AGPL-3.0. You can self-host it for free on any server. fastCRW is the hosted service at [fastcrw.com](https://fastcrw.com) — it runs CRW under the hood and adds a proxy network for bypassing bot protection, managed scaling, and a usage dashboard. Same API, different infrastructure model.

### Can I contribute to CRW?

Yes — pull requests are welcome on [GitHub](https://github.com/fastcrw/crw). The most useful contributions right now are: additional test coverage for edge-case HTML documents, improved markdown conversion fidelity for specific page types (tables, code blocks, nested lists), and documentation improvements. Open an issue before starting a large feature so we can align on scope.

### How does CRW make money?

CRW is open-source. fastCRW — the hosted version at [fastcrw.com](https://fastcrw.com) — is the commercial product. fastCRW charges per-credit for API usage above the free tier and offers subscription plans for predictable workloads. Revenue from fastCRW funds ongoing development of the open-source CRW core. The AGPL-3.0 license ensures that any hosted CRW derivative must open-source its modifications.

### Why build another web scraper when so many already exist?

The existing options forced a choice between good API design (Firecrawl) and operational simplicity (didn't exist). CRW reuses Firecrawl's excellent API design but replaces the heavy Node.js + Playwright runtime with a small single Rust binary with a tiny idle footprint. The combination — Firecrawl-compatible API, one self-contained binary, low local-first latency, no browser bundle — didn't exist before CRW.

### Why not just contribute to Firecrawl instead?

Firecrawl's architecture is Node.js + Playwright by design — that's what enables screenshots, PDF parsing, and broad browser automation. You can't shrink that stack to a tiny single-binary footprint without replacing the fundamental runtime. CRW is a different architectural bet for different constraints, not a competing implementation of the same design.
