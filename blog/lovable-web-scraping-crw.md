# How to Use CRW with Lovable for AI App Prototyping

> Build a web app prototype with Lovable's AI app builder that uses CRW/fastCRW for live web scraping — from prompt to working app in minutes.

**Published:** 2026-04-19  
**Updated:** 2026-04-19  
**Canonical:** https://fastcrw.com/blog/lovable-web-scraping-crw

---

## What We're Building

A working web application — built entirely with **Lovable**'s AI app builder — that scrapes websites using CRW's API and displays the results in a clean UI. We'll build a "Research Assistant" app that lets users enter a URL, scrapes the page content, and shows a formatted summary.

[Lovable](https://lovable.dev) is an AI-powered app builder that generates full-stack web apps from natural language prompts. Combined with CRW's Firecrawl-compatible API, you can prototype data-powered apps in minutes instead of days.

## Prerequisites

- A [Lovable](https://lovable.dev) account (free tier available)
- A [fastCRW](https://fastcrw.com) cloud API key (recommended for Lovable since it needs a public API endpoint)
- Basic understanding of what you want your app to do

**Why fastCRW Cloud?** Lovable generates apps that run in the browser. Browser-based apps can't reach `localhost:3000`, so you need a publicly accessible CRW endpoint. fastCRW provides this out of the box.

## Step 1: Create Your Lovable Project

Log into Lovable and click **"New Project"**. You'll see a prompt input where you describe what you want to build. Let's start with a clear, detailed prompt:

```
Build a "Research Assistant" web app with these features:

1. A clean input form where users can enter a URL
2. A "Scrape" button that calls a REST API to extract the page content
3. Display the scraped content as formatted markdown
4. Show the page title and metadata in a header card
5. Add a loading spinner while scraping
6. Use a modern, minimal design with Tailwind CSS

The API endpoint is: POST https://api.fastcrw.com/v1/scrape
Headers: Content-Type: application/json, Authorization: Bearer crw_live_API_KEY
Body: { "url": "USER_INPUT_URL", "formats": ["markdown"] }
Response: { "success": true, "data": { "markdown": "...", "metadata": { "title": "...", "sourceURL": "..." } } }
```

Lovable generates the full app — React components, API integration, styling — from this single prompt. The more specific you are about the API contract, the better the generated code.

## Step 2: Review and Refine the Generated App

Lovable produces a working app on the first try, but you'll want to refine it. Use follow-up prompts to iterate:

### Add Error Handling

```
Add error handling for the scrape API call:
- Show a red error banner if the API returns success: false
- Show a friendly message if the URL is invalid
- Add a timeout of 30 seconds with a timeout error message
- Handle network errors gracefully
```

### Add API Key Configuration

```
Add a settings panel (gear icon in the header) where users can enter
their own fastCRW API key. Store it in localStorage so it persists
between sessions. Use this key in the Authorization header instead
of a hardcoded value.
```

### Enhance the Content Display

```
Render the scraped markdown content using react-markdown with:
- Syntax highlighting for code blocks (use react-syntax-highlighter)
- Proper heading hierarchy
- Styled tables and lists
- A "Copy to clipboard" button for the raw markdown
```

## Step 3: Add Multi-Page Crawling

Extend the app to crawl entire websites using CRW's crawl endpoint. Prompt Lovable:

```
Add a "Crawl Site" mode with a toggle between "Single Page" and "Full Site".

In Full Site mode:
1. Call POST https://api.fastcrw.com/v1/crawl with body:
   { "url": "USER_URL", "limit": 20, "scrapeOptions": { "formats": ["markdown"] } }
2. The response returns { "id": "job-id" }
3. Poll GET https://api.fastcrw.com/v1/crawl/{id} every 3 seconds
4. Show a progress bar with the number of pages scraped
5. When status is "completed", display all pages in a sidebar list
6. Clicking a page shows its markdown content in the main panel
```

This turns your simple scraper into a full site explorer — and Lovable generates all the polling logic, state management, and UI components for you.

## Step 4: Add Data Extraction

CRW's `/v1/extract` endpoint uses LLM-powered extraction to pull structured data from pages. Add this to your app:

```
Add an "Extract Data" tab next to the content display.

The user can enter a natural language prompt like "Extract all product names
and prices" and optionally define a JSON schema.

Call POST https://api.fastcrw.com/v1/extract with body:
{
  "url": "CURRENT_URL",
  "prompt": "USER_PROMPT",
  "schema": USER_SCHEMA_OR_NULL
}

Display the extracted data in a formatted table.
Add a "Download CSV" button to export the results.
```

## Step 5: Add URL Discovery with Map

Before crawling a site, users might want to preview what pages are available. Add CRW's map endpoint:

```
Add a "Discover Pages" button that calls POST https://api.fastcrw.com/v1/map
with body: { "url": "USER_URL" }

Display the returned URLs in a checklist. Users can select which pages
to scrape. Add "Select All" and "Select None" buttons.
When the user clicks "Scrape Selected", scrape each checked URL
using the /v1/scrape endpoint and show results.
```

## Example: Building a Competitive Intelligence Dashboard

Here's a more ambitious prompt that builds a full competitive intelligence tool:

```
Build a "Competitive Intelligence Dashboard" with:

1. A sidebar where users can add competitor URLs (stored in localStorage)
2. A "Refresh All" button that scrapes all saved URLs via
   POST https://api.fastcrw.com/v1/scrape
3. For each competitor, show:
   - Page title and last scraped timestamp
   - Key content changes since last scrape (diff view)
   - Extracted data: pricing, features, key messaging
4. A comparison table showing all competitors side by side
5. Export to CSV functionality

Use the fastCRW API at https://api.fastcrw.com with the API key
from localStorage (settings panel).
```

Lovable generates a complete dashboard with state management, diff comparison, and export — all powered by CRW's scraping API.

## Handling CORS and API Keys

When calling APIs from browser-based apps, two concerns come up:

### CORS

fastCRW's cloud API supports CORS, so browser requests work directly. If you're self-hosting CRW behind a reverse proxy, ensure your proxy adds the appropriate CORS headers:

```
Access-Control-Allow-Origin: *
Access-Control-Allow-Headers: Content-Type, Authorization
Access-Control-Allow-Methods: POST, GET, OPTIONS
```

### API Key Security

Storing API keys in localStorage is fine for personal tools and prototypes. For production apps with multiple users, route API calls through a backend proxy:

```
// Prompt Lovable to add a Supabase Edge Function:
Add a Supabase Edge Function at /functions/v1/scrape that:
1. Accepts the same body as CRW's /v1/scrape
2. Adds the API key from environment variables
3. Proxies the request to https://api.fastcrw.com/v1/scrape
4. Returns the response to the client

This keeps the API key server-side and secure.
```

## Deploying Your Lovable App

Lovable provides one-click deployment. Your app gets a public URL immediately. For custom domains:

- Click **"Settings"** in your Lovable project
- Add your custom domain under **"Domains"**
- Update your DNS records as instructed

Since the scraping happens server-side (CRW processes the request), your Lovable app is lightweight — it's just a frontend that calls the API and displays results.

## Why CRW + Lovable Works Well

- **Clean API contract:** CRW's Firecrawl-compatible REST API is simple enough for Lovable to generate correct integration code from a prompt description.
- **Markdown output:** CRW returns clean markdown, which React rendering libraries handle natively. No HTML parsing or content cleaning in the browser.
- **Low-latency responses:** CRW's local-first engine keeps scrape calls quick, so the UI feels responsive instead of leaving users staring at a spinner.
- **Structured extraction:** The `/v1/extract` endpoint returns JSON that maps directly to table components — no text parsing gymnastics.

## Ideas for Apps to Build

- **Documentation Search:** Scrape a docs site and build a search interface with full-text search
- **News Aggregator:** Scrape multiple news sources, extract headlines and summaries, display in a feed
- **Price Tracker:** Monitor product pages, extract prices, show price history charts
- **Content Repurposer:** Scrape blog posts, summarize with AI, reformat for different platforms
- **SEO Analyzer:** Scrape competitor pages, extract meta tags and headings, show optimization suggestions

## Conclusion

Lovable + CRW is the fastest path from idea to working web scraping app. Lovable handles the frontend, and CRW handles the scraping — you describe what you want in plain English and get a deployed app in minutes.

For more on CRW's capabilities, see our [website-to-markdown conversion guide](/blog/website-to-markdown) and [CRW vs. Firecrawl comparison](/blog/firecrawl-vs-crawl4ai-vs-crw).

Get started: [self-host CRW](https://github.com/fastcrw/crw) or sign up for [fastCRW cloud](https://fastcrw.com).
