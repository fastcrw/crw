# How to Add Web Scraping to Claude Code in 30 Seconds

> Give Claude Code web scraping superpowers with CRW's built-in MCP server. One command, zero config — scrape any website directly from your terminal AI assistant.

**Published:** 2026-04-13  
**Updated:** 2026-05-23  
**Canonical:** https://fastcrw.com/blog/claude-code-web-scraping

---

## The Problem: Claude Code Can't See the Web

Claude Code is incredibly powerful for writing code, debugging, and navigating your codebase. But the moment you need it to check a library's latest documentation, read an API changelog, or verify what a webpage actually says — it's blind. It only knows what's in its training data, which might be months old.

You've probably experienced this: you ask Claude Code to integrate a new library, and it confidently writes code against a deprecated API because its knowledge cutoff predates the latest release. Or you're debugging a production issue and need to cross-reference the actual error page content, but Claude Code can't fetch it.

This isn't a limitation of the model — it's a tooling gap. Claude Code supports MCP (Model Context Protocol), which means it can call external tools if you provide them. The question is: which web scraping tool do you connect?

## The Solution: CRW + MCP in One Command

CRW is an open-source web scraper with a built-in MCP server. Unlike other scrapers that require a separate MCP wrapper, CRW's MCP server is compiled into the same binary. There's nothing extra to install, configure, or maintain.

Here's the entire setup:

```
npx -y crw-mcp
claude mcp add crw -- npx -y crw-mcp
```

That's it. Two commands. Claude Code now has nine web scraping tools available: `crw_scrape`, `crw_crawl`, `crw_check_crawl_status`, `crw_map`, `crw_extract`, `crw_check_extract_status`, `crw_cancel_extract`, `crw_search`, and `crw_parse_file`. No API key, no Docker container, no configuration file. CRW is also listed on the official [MCP Registry](https://registry.modelcontextprotocol.io/?q=crw).

The next time you ask Claude Code something that requires web content, it will automatically decide whether to use these tools. Ask it to "check the latest Next.js docs for the App Router API" and it will scrape the documentation page, read the markdown content, and answer based on the live page — not its training data.

## What Exactly Gets Installed?

When you run `npx -y crw-mcp`, npm downloads and runs the CRW MCP server (npm package `crw-mcp`). This binary is roughly 10 MB and has zero runtime dependencies — no Redis, no headless browser. It runs as a stdio MCP server: Claude Code starts the process, sends JSON-RPC messages to its stdin, and reads responses from stdout. The Python SDK (`pip install crw`) is a separate client library, not the MCP server — for MCP, use `npx -y crw-mcp`.

The `claude mcp add` command registers this binary with Claude Code's MCP configuration. Behind the scenes, it adds an entry to your `~/.claude.json` file that looks like:

```
{
  "mcpServers": {
    "crw": {
      "command": "crw-mcp"
    }
  }
}
```

When Claude Code starts a new session, it launches `crw-mcp` as a subprocess. The MCP handshake happens automatically — Claude Code discovers the available tools, their parameter schemas, and their descriptions. From then on, the model can call any of the tools at will.

## The Tools Claude Code Gets

CRW exposes nine MCP tools, each mapped to a REST API endpoint:

### crw_scrape — Fetch a Single Page

The most commonly used tool. It fetches a URL and returns clean markdown, stripping navigation, ads, cookie banners, and other noise. Claude Code uses this when you ask about specific pages.

Example prompts that trigger `crw_scrape`:

- "What does the Stripe API docs say about idempotency keys?"
- "Scrape this error page and tell me what went wrong: https://..."
- "Read the README of this GitHub repo and summarize the installation steps"
- "What's the current pricing on Vercel's Pro plan?"

Behind the scenes, Claude Code sends a `tools/call` request with the URL and receives clean markdown back. The content goes into the model's context window, and it reasons about it like any other text.

### crw_crawl — Crawl Multiple Pages

For broader research tasks. It performs a BFS crawl starting from a URL, respecting robots.txt, and returns markdown for each discovered page up to a configurable limit.

Example prompts:

- "Crawl the Next.js documentation and find all pages about middleware"
- "Index this site's blog posts and summarize the last 5 articles"
- "Find all API endpoints documented on this site"

### crw_map — Discover URLs Without Fetching Content

A lightweight discovery tool. It returns all URLs found on a page without fetching their full content. Claude Code uses this when it needs to understand a site's structure before deciding what to scrape.

Example prompts:

- "What pages are on this documentation site?"
- "Show me the sitemap for this blog"
- "Find all product pages on this e-commerce site"

## Real-World Workflow Examples

### 1. Debugging with Live Documentation

You're integrating the Anthropic SDK and hitting a weird error. Instead of switching to your browser and searching through docs manually:

```
You: "I'm getting a 'content_block_delta' event type I don't recognize.
     Check the Anthropic streaming docs and tell me what changed."

Claude Code:
  → calls crw_scrape("https://docs.anthropic.com/en/api/streaming")
  → reads the live documentation
  → explains the new event types introduced in the latest API version
  → updates your code to handle the new event
```

This entire flow happens in your terminal. No browser tab, no copy-paste, no context switching.

### 2. Library Migration Research

You need to migrate from Express to Hono. Instead of reading migration guides in your browser:

```
You: "I'm migrating this Express app to Hono. Scrape the Hono docs
     and help me convert my middleware."

Claude Code:
  → calls crw_map("https://hono.dev/docs") to discover all doc pages
  → identifies the middleware and migration guides
  → calls crw_scrape on the 3 most relevant pages
  → reads your Express middleware code
  → rewrites it for Hono using the live documentation as reference
```

### 3. Competitive Analysis

You're building a SaaS and want to compare competitor pricing:

```
You: "Scrape the pricing pages for Supabase, PlanetScale, and Neon.
     Create a comparison table in markdown."

Claude Code:
  → calls crw_scrape on each pricing page
  → extracts the pricing tiers from clean markdown
  → generates a comparison table with features, limits, and costs
  → saves it as a markdown file in your project
```

### 4. Changelog Monitoring

You want to know what changed in a dependency before upgrading:

```
You: "Check the changelog for tailwindcss v4 and list breaking changes
     that affect our project."

Claude Code:
  → calls crw_scrape("https://tailwindcss.com/blog/tailwindcss-v4")
  → reads the release announcement
  → cross-references with your tailwind.config.js and CSS files
  → lists specific breaking changes that affect your codebase
  → suggests fixes for each one
```

## Why CRW Instead of Other MCP Scrapers?

There are several web scraping MCP servers available. Here's why CRW is the best fit for Claude Code:

### Single Binary, Zero Dependencies

CRW compiles to a single Rust binary. No Node.js runtime, no Python environment, no Docker container running in the background. This matters for Claude Code because MCP servers are started as subprocesses — a heavy server means slower session startup and higher memory usage.

CRW's MCP binary has a tiny resident footprint at idle — far lighter than Node.js-based MCP scrapers, which carry a full V8 runtime before they even process a request.

### Built-in, Not Bolted On

Most scrapers (including Firecrawl) have MCP support as a separate repository or npm package that wraps their REST API. This means you need to run the scraper server first, then run the MCP wrapper that proxies to it — two processes, two things to maintain.

CRW's MCP server is compiled into the same binary. The `crw-mcp` command starts a complete scraping engine with MCP transport. There's no intermediary, no HTTP overhead, no port configuration.

### Fast Startup

Claude Code starts MCP servers at the beginning of each session. A server that takes 5 seconds to start delays every new conversation. CRW starts in about 85 milliseconds — imperceptible to the user.

### Clean Markdown Output

The quality of the scraped content matters enormously for LLM reasoning. CRW strips navigation bars, footers, cookie banners, ad slots, and sidebar clutter. It preserves heading hierarchy, code blocks, tables, and lists. The model gets clean, structured content that's easy to reason about — not a wall of HTML noise.

## Advanced Configuration

### Adding Authentication

If you're running CRW as a server (not just the MCP binary), you can add API key authentication:

```
claude mcp add crw -- crw-mcp --env CRW_API_URL=http://localhost:3000 --env CRW_API_KEY=your-key
```

### Using fastCRW Cloud

If you don't want to run a local binary, you can use fastCRW's managed cloud. This gives you a global proxy network, higher success rates on bot-protected sites, and JS rendering without running a local browser:

```
claude mcp add crw -- crw-mcp --env CRW_API_URL=https://api.fastcrw.com --env CRW_API_KEY=your-api-key
```

Sign up at [fastcrw.com](https://fastcrw.com) for 1000 free credits.

### JS Rendering for SPAs

By default, CRW fetches pages via HTTP and converts the HTML to markdown. For single-page applications (React, Vue, Angular) that render content client-side, you need JS rendering. Set up a local renderer:

```
cargo install crw-server
crw-server setup  # Downloads LightPanda browser
crw-server &      # Start server in background

# Point MCP at the server
claude mcp add crw -- crw-mcp --env CRW_API_URL=http://localhost:3000
```

Now when Claude Code scrapes a SPA, CRW automatically detects the empty initial HTML and renders the page through LightPanda before extracting markdown.

## How CRW Compares

On our public benchmark, CRW posts **63.74% truth-recall (522 of 819 labeled URLs) — the highest of the three tools tested** — plus ~92% scrape success of reachable URLs and 0 thrown errors across 3,000 requests. The full latency distribution and a one-command repro are on [/benchmarks](/benchmarks).

| Metric | CRW | Firecrawl |
| --- | --- | --- |
| Deployment | **Single static binary** | Node + Redis + PG + RabbitMQ |
| License | **AGPL-3.0, self-host free** | AGPL-3.0 self-host / hosted |
| API | **Firecrawl-compatible** | Native |

For Claude Code specifically, the practical wins are a lightweight, local-first MCP server: faster session starts and more memory available for the AI model itself, with no exit cost since you can self-host the engine free under AGPL-3.0.

## Works with Other MCP Clients Too

The same `crw-mcp` binary works with Cursor, Windsurf, Cline, Continue.dev, and OpenAI Codex CLI. Each client uses a slightly different config file format, but the setup is identical: point the MCP config at the `crw-mcp` binary.

See the full [MCP setup guide](/blog/mcp-web-scraping) for all client configurations, advanced patterns, and programmatic SDK usage.

## Troubleshooting

### "crw-mcp: command not found"

The binary isn't on your PATH. Find it with:

```
which crw-mcp
# or
find ~/.cargo/bin -name crw-mcp
```

If using Claude Code, register with the full path:

```
claude mcp add crw -- ~/.cargo/bin/crw-mcp
```

### Claude Code doesn't use the scraping tools

The model decides when to use tools based on your prompt. If you want to force a scrape, be explicit:

```
# Vague — model might not scrape
"What's the latest version of React?"

# Explicit — model will use crw_scrape
"Scrape https://react.dev and tell me the latest version"
```

### Scraping returns empty or garbled content

This usually means the page is a SPA that requires JavaScript rendering. Set up a CRW server with JS rendering enabled (see the Advanced Configuration section above) and point the MCP binary at it.

### Timeout on large pages

CRW has a default timeout of 30 seconds. For slow-loading pages, set a higher timeout via environment variable:

```
claude mcp add crw -- crw-mcp --env CRW_TIMEOUT=60000
```

## What You Can Build With This

Once Claude Code has web scraping, entirely new workflows open up:

- **Live documentation lookups** — always reference the latest docs, not stale training data
- **Automated dependency updates** — scrape changelogs, identify breaking changes, apply fixes
- **Competitive intelligence** — compare competitor features, pricing, and positioning
- **Content generation** — scrape source material, synthesize summaries, generate reports
- **API exploration** — scrape API docs, generate client code, write integration tests
- **Bug reproduction** — scrape error pages, stack traces, and GitHub issues for context
- **Code examples** — scrape official examples and adapt them to your codebase

The key insight is that web scraping isn't just about getting data — it's about giving your AI assistant real-time context that makes it dramatically more useful.

## Related Guides

- [How to Expose Web Scraping to AI Agents with MCP](/blog/mcp-web-scraping) — full MCP setup for all clients, SDK usage, advanced patterns
- [$5 VPS Web Scraping](/blog/crw-on-5-dollar-vps) — deploy CRW on the cheapest possible server and connect it to Claude Code
- [Scraping Cloudflare-Protected Sites](/blog/bypass-cloudflare-scraping) — how CRW's stealth mode handles bot detection

## Get Started Now

Two commands, 30 seconds, zero configuration:

```
npx -y crw-mcp
claude mcp add crw -- npx -y crw-mcp
```

CRW is open-source (AGPL-3.0): [github.com/fastcrw/crw](https://github.com/fastcrw/crw)

For managed hosting with a global proxy network and JS rendering: [fastcrw.com](https://fastcrw.com)

## FAQ

### How do I add web scraping to Claude Code?

Install the CRW MCP server with npx -y crw-mcp, then register it with claude mcp add fastcrw -- npx -y crw-mcp. Claude Code will automatically discover the scraping tools and use them when your prompts require web content. The crw package on PyPI is the Python SDK client, not the MCP server.

### What is CRW MCP for Claude Code?

CRW is an open-source web scraper with a built-in MCP (Model Context Protocol) server. When connected to Claude Code, the crw-mcp server exposes nine tools — scrape, search, crawl, check_crawl_status, map, extract, check_extract_status, cancel_extract, and parse_file — that let Claude Code fetch and read live web pages (and files, including PDFs) directly from your terminal. Single-URL structured JSON extraction runs through the scrape tool's json format with a schema; the `extract` tool adds native async multi-URL extraction.

### Does Claude Code web scraping require an API key?

No. The self-hosted CRW binary works without any API key — it scrapes pages directly from your machine. If you want to use the fastCRW managed cloud for its proxy network and JS rendering, you can start free on the Free plan, which includes 1000 one-time lifetime credits.

### Can Claude Code scrape JavaScript-rendered pages?

Yes, but you need to set up JS rendering. Run crw-server setup to download the LightPanda browser engine, start crw-server in the background, and point the MCP binary at it. CRW automatically detects SPAs and renders them before extracting content.

### How does CRW compare to using WebFetch in Claude Code?

CRW provides significantly cleaner output. It strips navigation, ads, footers, and boilerplate, giving the model clean markdown that's easy to reason about. It also supports crawling multiple pages, site mapping, and structured JSON extraction via the scrape tool's json format — capabilities that a simple fetch doesn't provide.

### How accurate and fast is CRW's MCP scraper?

On the public 3-way scrape benchmark — Firecrawl's scrape-content-dataset-v1 of 1,000 URLs with 819 labeled, harness diagnose_3way.py, run 2026-05-08 — CRW posted 63.74% truth-recall (522 of 819 labeled URLs), the highest of the three, plus ~92% scrape success of reachable URLs and 0 thrown errors across 3,000 requests. Its p50 latency was 1914ms (fastest).
