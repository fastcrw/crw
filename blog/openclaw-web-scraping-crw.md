# Add Web Scraping to OpenClaw Agents with CRW

> Install the CRW plugin for OpenClaw and give your WhatsApp, Telegram, and Discord AI agents the ability to scrape, crawl, and map any website.

**Published:** 2026-04-19  
**Updated:** 2026-04-19  
**Canonical:** https://fastcrw.com/blog/openclaw-web-scraping-crw

---

## What is OpenClaw?

[OpenClaw](https://github.com/openclaw/openclaw) is a self-hosted AI agent gateway with 330K+ GitHub stars. It connects messaging platforms — WhatsApp, Telegram, Discord, Slack, and more — to AI models like Claude, GPT, and DeepSeek. Agents can use tools to take actions, and that's where CRW comes in.

## What We're Building

An OpenClaw agent that can scrape, crawl, and map websites on demand. Users send a URL in chat, and the agent fetches clean content, summarizes it, or crawls an entire site for research.

## Step 1: Install the Plugin

```
openclaw plugins install openclaw-plugin-crw
```

## Step 2: Configure — Pick One

### Option A: Cloud (fastcrw.com) — Quickest Start

[Sign up at fastcrw.com](https://fastcrw.com) and get **1000 free credits**. Add to your OpenClaw config:

```
{
  "plugins": {
    "crw": {
      "apiKey": "crw_live_..."
    }
  }
}
```

That's it — cloud is the default. No server to run.

### Option B: Self-hosted with binary (free, no limits)

```
curl -fsSL https://fastcrw.com/install | bash
crw  # starts on http://localhost:3000
```

```
{
  "plugins": {
    "crw": {
      "apiUrl": "http://localhost:3000"
    }
  }
}
```

### Option C: Self-hosted with Docker

```
docker run -d -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

Same config as Option B.

## Step 3: Use It

Once configured, your OpenClaw agents automatically get three new tools:

### crw_scrape — Scrape a page

**User (via WhatsApp):** "Summarize this article: https://example.com/blog/ai-trends"

**Agent:** Uses `crw_scrape` → gets clean markdown → summarizes → responds in chat.

### crw_crawl — Crawl a site

**User:** "Research everything on docs.example.com and give me a summary"

**Agent:** Uses `crw_crawl` → discovers and scrapes all pages → synthesizes → responds.

### crw_map — Discover URLs

**User:** "What pages does example.com have?"

**Agent:** Uses `crw_map` → returns all discovered URLs via sitemap + link traversal.

## Real-World Use Cases

### Customer Support Bot

Your support bot receives a question. It crawls your docs site with `crw_crawl`, finds the relevant section, and replies with the exact answer — with a link to the source.

### News Aggregator

Users send news URLs via Telegram. The agent scrapes each article with `crw_scrape`, extracts key points, and posts a daily digest to a group chat.

### Competitor Research

Send a competitor's URL via Slack. The agent maps the site with `crw_map`, crawls product and pricing pages with `crw_crawl`, and produces a competitive analysis report.

## CRW vs Firecrawl Plugin

OpenClaw already has a Firecrawl plugin. Here's how CRW compares:

| Feature | CRW Plugin | Firecrawl Plugin |
| --- | --- | --- |
| Cloud option | [fastcrw.com](https://fastcrw.com) | firecrawl.dev |
| Self-hosted | Single small static binary | 5+ containers |
| API key required | No (self-hosted) | Yes (always) |
| Deployment | One binary, no runtime | Node + Redis + browser |
| License | AGPL-3.0, self-host free | AGPL-3.0 self-host / hosted |

## Links

- [openclaw-plugin-crw on npm](https://www.npmjs.com/package/openclaw-plugin-crw)
- [GitHub: us/openclaw-plugin-crw](https://github.com/us/openclaw-plugin-crw)
- [CRW on GitHub](https://github.com/fastcrw/crw)
- [fastcrw.com — 1000 free credits](https://fastcrw.com)

## Get Started

```
openclaw plugins install openclaw-plugin-crw
```

Or sign up for [fastCRW](https://fastcrw.com) to skip infrastructure setup entirely.
