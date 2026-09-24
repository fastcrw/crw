# How to Automate Web Scraping with Make.com and CRW

> Step-by-step guide to building automated web scraping workflows in Make.com using CRW's Firecrawl-compatible API — no code required.

**Published:** 2026-04-20  
**Updated:** 2026-04-20  
**Canonical:** https://fastcrw.com/blog/make-web-scraping-crw

---

## What We're Building

A fully automated web scraping pipeline in **Make.com** (formerly Integromat) that: (1) triggers on a schedule or webhook, (2) calls CRW's API to scrape web pages into clean markdown, (3) processes the results, and (4) sends the data to Google Sheets, Airtable, or any destination you choose — all without writing a single line of code.

CRW's Firecrawl-compatible REST API makes it easy to integrate with Make.com's HTTP module. Whether you're monitoring competitor pricing, collecting research data, or feeding content into an AI pipeline, this workflow handles it all.

## Prerequisites

- A [Make.com](https://www.make.com) account (free tier works for testing)
- CRW running locally (`docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`) or a [fastCRW](https://fastcrw.com) cloud API key
- A destination for your scraped data (Google Sheets, Airtable, Slack, etc.)

## Step 1: Create a New Scenario

Log into Make.com and click **"Create a new scenario"**. A scenario is Make's term for an automated workflow — it's a sequence of modules that execute in order.

For this tutorial, we'll build a scenario that scrapes a list of URLs on a schedule and stores the results. The final flow will look like:

**Schedule Trigger → Iterator (URLs) → HTTP Request (CRW Scrape) → Google Sheets (Store Results)**

## Step 2: Set Up the Trigger

Every Make scenario starts with a trigger. You have several options:

- **Schedule:** Run every hour, daily, or on a custom cron — ideal for monitoring pages that change regularly
- **Webhook:** Trigger the scrape from an external event (e.g., a Slack command, form submission, or API call)
- **Google Sheets:** Watch a spreadsheet for new URLs — add a row, and Make scrapes it automatically

For this guide, we'll use the **Schedule** trigger set to run once daily. Click the clock icon, select "Every day", and pick your preferred time.

## Step 3: Define Your URL List

Add a **"Set multiple variables"** module (under Tools) to define the URLs you want to scrape. Create an array variable called `urls`:

```
[
  "https://example.com/page-1",
  "https://example.com/page-2",
  "https://example.com/page-3"
]
```

Alternatively, you can pull URLs from a Google Sheet or Airtable. Add a **"Search Rows"** module from the Google Sheets app and map the URL column.

After the variable module, add an **Iterator** module to loop through each URL. The iterator takes the array and processes one URL at a time through the remaining modules.

## Step 4: Configure the HTTP Module for CRW Scrape

This is the core of the workflow. Add an **"HTTP → Make a request"** module and configure it:

### Request Configuration

- **URL:** `https://api.fastcrw.com/v1/scrape` (or `http://localhost:3000/v1/scrape` for self-hosted)
- **Method:** POST
- **Headers:** `Content-Type`: `application/json`
- `Authorization`: `Bearer crw_live_YOUR_API_KEY`

  **Body type:** Raw
  **Content type:** JSON (application/json)
  **Request content:**

```
{
  "url": "{{1.url}}",
  "formats": ["markdown"]
}
```

Replace `{{1.url}}` with the actual Make.com mapping from your iterator. In the Make UI, click into the field and select the URL variable from the iterator output.

### Response Handling

Check **"Parse response"** so Make automatically parses the JSON response. CRW returns:

```
{
  "success": true,
  "data": {
    "markdown": "# Page Title

Clean content here...",
    "metadata": {
      "title": "Page Title",
      "sourceURL": "https://example.com/page-1",
      "description": "Page description"
    }
  }
}
```

## Step 5: Add Error Handling

Web scraping can fail — pages go down, rate limits kick in, or URLs change. Add a **Router** after the HTTP module with two paths:

- **Path 1 (Success):** Filter where `data.success` equals `true` — continue to your data destination
- **Path 2 (Error):** Filter where `data.success` equals `false` — log the error or send a notification

You can also add a **Sleep** module (1–2 seconds) between iterations to avoid overwhelming your CRW instance or staying within rate limits.

## Step 6: Store Results in Google Sheets

Add a **"Google Sheets → Add a Row"** module on the success path. Map the fields:

- **Column A (URL):** Map to `data.metadata.sourceURL`
- **Column B (Title):** Map to `data.metadata.title`
- **Column C (Content):** Map to `data.markdown`
- **Column D (Scraped At):** Use Make's `{{now}}` function

For other destinations, Make supports 1,500+ app integrations. Common choices:

- **Airtable:** Great for structured data with rich field types
- **Notion:** Store scraped content as database entries or pages
- **Slack/Email:** Get notifications with scraped content summaries
- **Webhook:** Forward results to your own API or pipeline

## Advanced: Crawl an Entire Site

For crawling multiple pages from a single domain, use CRW's `/v1/crawl` endpoint. This requires two HTTP modules since crawling is asynchronous:

### Module 1: Start the Crawl

```
POST https://api.fastcrw.com/v1/crawl

{
  "url": "https://docs.example.com",
  "limit": 50,
  "scrapeOptions": {
    "formats": ["markdown"]
  }
}
```

This returns a job ID: `{"success": true, "id": "crawl-abc123"}`

### Module 2: Poll for Results

Add a **Repeater** module that polls the crawl status every 5 seconds:

```
GET https://api.fastcrw.com/v1/crawl/{{crawl_id}}
```

Use a **Break** directive when `status` equals `"completed"`. The response contains a `data` array with all scraped pages.

## Advanced: Extract Structured Data

CRW's `/v1/extract` endpoint can pull structured data from pages using LLM-powered extraction. This is perfect for pulling specific fields like prices, dates, or contact info:

```
POST https://api.fastcrw.com/v1/extract

{
  "urls": ["https://example.com/product"],
  "prompt": "Extract the product name, price, and availability status",
  "schema": {
    "type": "object",
    "properties": {
      "product_name": { "type": "string" },
      "price": { "type": "string" },
      "in_stock": { "type": "boolean" }
    }
  }
}
```

The structured response maps directly to spreadsheet columns or database fields — no text parsing required.

## Real-World Use Cases

### Competitor Price Monitoring

Schedule a daily scrape of competitor product pages. Use CRW's extract endpoint to pull prices into a Google Sheet. Add a **Filter** module to detect price changes and send a Slack alert when competitors adjust pricing.

### Content Aggregation for AI

Scrape industry news sites and documentation pages. Feed the markdown into an OpenAI module to generate summaries. Store in Notion as a daily briefing — a fully automated research assistant.

### Lead Generation

Scrape business directories or job boards. Extract company names, emails, and descriptions using CRW's extract endpoint. Push qualified leads directly into your CRM via Make's Salesforce or HubSpot integrations.

## Tips for Production Workflows

- **Rate limiting:** Add a 1-second delay between scrape requests. Make's Sleep module works well for this.
- **Data deduplication:** Use Make's **Search Rows** module to check if a URL was already scraped before processing.
- **Monitoring:** Enable Make's built-in execution history. Failed runs show exactly which module failed and why.
- **Partial runs:** Enable "Allow storing incomplete executions" so Make saves progress even if a later module fails.
- **Cost control:** Make's free tier allows 1,000 operations/month. Each HTTP request counts as one operation. For high-volume scraping, the Pro plan is recommended.

## Self-Hosted vs. Cloud

You have two options for running CRW with Make.com:

### Self-Hosted CRW

- Run CRW on your own server: `docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`
- No API key required, no per-request costs
- Your Make.com scenario must be able to reach your server (use a tunnel like ngrok for local testing)
- Best for high-volume scraping or when data privacy is critical

### fastCRW Cloud

- Use `https://api.fastcrw.com` as your API URL
- No infrastructure to manage — works immediately from Make.com
- Managed cloud infrastructure with no server management
- Best for getting started quickly or when you don't want to maintain a server

## Conclusion

Make.com + CRW is a powerful combination for no-code web scraping automation. You get Make's visual workflow builder and 1,500+ integrations paired with CRW's fast, reliable scraping API. The entire setup takes about 15 minutes, and once running, it handles data collection on autopilot.

For more advanced use cases, check out our guides on [building RAG pipelines with CRW](/blog/rag-pipeline-with-crw) and [using CRW's MCP server with AI agents](/blog/mcp-web-scraping).

Ready to get started? [Self-host CRW](https://github.com/fastcrw/crw) for free or sign up for [fastCRW](https://fastcrw.com) to get a cloud API key in seconds.
