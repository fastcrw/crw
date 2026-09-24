# How to Connect CRW to n8n for Automated Scraping Workflows

> Connect n8n to CRW's API for automated web scraping — build scheduled scrapers, data pipelines, and alerts without code.

**Published:** 2026-04-26  
**Updated:** 2026-04-26  
**Canonical:** https://fastcrw.com/blog/n8n-web-scraping-crw

---

## What We're Building

Automated web scraping workflows in n8n using CRW as the scraping backend. n8n is an open-source workflow automation platform — think Zapier but self-hosted and with full HTTP request support. We'll connect n8n's HTTP Request nodes to CRW's REST API to build: (1) a scheduled scraper that monitors pages for changes, (2) a data extraction pipeline that feeds into Google Sheets, and (3) a content aggregation workflow with Slack notifications.

No coding required — just n8n's visual workflow builder and CRW's API endpoints.

## Prerequisites

- CRW running locally (`docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`) or a [fastCRW](https://fastcrw.com) API key
- n8n running locally (`docker run -p 5678:5678 n8nio/n8n`) or n8n cloud
- Basic familiarity with n8n's visual workflow editor

## CRW API Endpoints for n8n

CRW exposes a Firecrawl-compatible REST API. Here are the endpoints you'll use in n8n:

| Endpoint | Method | Purpose |
| --- | --- | --- |
| `/v1/scrape` | POST | Scrape a single page → markdown |
| `/v1/crawl` | POST | Start async crawl of a site |
| `/v1/crawl/{id}` | GET | Check crawl status / get results |
| `/v1/map` | POST | Discover URLs on a site |
| `/v1/extract` | POST | Extract structured data |

Base URL: `http://localhost:3000` (self-hosted) or `https://api.fastcrw.com` (fastCRW cloud).

## Step 1: Create a CRW Credential in n8n

First, set up a reusable credential for CRW's API:

1. In n8n, go to **Credentials → Add Credential → Header Auth**
2. Set Name: `CRW API`
3. Set Header Name: `Authorization`
4. Set Header Value: `Bearer crw_live_YOUR-API-KEY`

This credential will be reused across all CRW nodes in your workflows.

## Step 2: Basic Scrape Workflow

The simplest workflow: scrape a page and output the content.

Create a new workflow with these nodes:

1. **Manual Trigger** — click to run
2. **HTTP Request** — calls CRW's scrape endpoint

Configure the HTTP Request node:

```
{
  "method": "POST",
  "url": "http://localhost:3000/v1/scrape",
  "authentication": "genericCredentialType",
  "genericAuthType": "httpHeaderAuth",
  "sendHeaders": true,
  "headerParameters": {
    "parameters": [
      { "name": "Content-Type", "value": "application/json" }
    ]
  },
  "sendBody": true,
  "bodyParameters": {
    "parameters": [
      { "name": "url", "value": "https://example.com" },
      { "name": "formats", "value": "=["markdown"]" }
    ]
  }
}
```

The response will contain `data.markdown` with the clean page content.

## Step 3: Scheduled Scraping Workflow

Monitor a page for changes on a schedule:

1. **Schedule Trigger** — runs every hour (or any interval)
2. **HTTP Request** — scrape the target page
3. **Code** — compare with previous version
4. **IF** — branch on whether content changed
5. **Slack / Email** — notify on changes

n8n workflow JSON for the scrape + compare pattern:

```
{
  "nodes": [
    {
      "parameters": {
        "rule": { "interval": [{ "field": "hours", "hoursInterval": 1 }] }
      },
      "name": "Every Hour",
      "type": "n8n-nodes-base.scheduleTrigger",
      "position": [250, 300]
    },
    {
      "parameters": {
        "method": "POST",
        "url": "http://localhost:3000/v1/scrape",
        "authentication": "genericCredentialType",
        "genericAuthType": "httpHeaderAuth",
        "sendBody": true,
        "specifyBody": "json",
        "jsonBody": "{ "url": "https://competitor.com/pricing", "formats": ["markdown"] }"
      },
      "name": "Scrape Page",
      "type": "n8n-nodes-base.httpRequest",
      "position": [450, 300],
      "credentials": { "httpHeaderAuth": { "id": "1", "name": "CRW API" } }
    },
    {
      "parameters": {
        "jsCode": "const currentContent = $input.first().json.data.markdown;\nconst staticData = $getWorkflowStaticData('global');\nconst previousContent = staticData.lastContent || '';\nstaticData.lastContent = currentContent;\nconst changed = currentContent !== previousContent;\nreturn [{ json: { changed, currentContent, previousContent } }];"
      },
      "name": "Compare",
      "type": "n8n-nodes-base.code",
      "position": [650, 300]
    },
    {
      "parameters": {
        "conditions": {
          "boolean": [{ "value1": "={{ $json.changed }}", "value2": true }]
        }
      },
      "name": "Changed?",
      "type": "n8n-nodes-base.if",
      "position": [850, 300]
    }
  ]
}
```

The Code node uses n8n's static data to persist the last scraped content between runs. When the content changes, the IF node routes to your notification node.

## Step 4: Multi-Page Crawl Workflow

Crawl an entire site and process each page:

1. **Manual Trigger**
2. **HTTP Request** — start crawl via `/v1/crawl`
3. **Wait** — pause for 5 seconds
4. **HTTP Request** — check crawl status via `/v1/crawl/{id}`
5. **IF** — is crawl completed?
6. **Split In Batches** — process each page

Start the crawl:

```
// HTTP Request node: Start Crawl
{
  "method": "POST",
  "url": "http://localhost:3000/v1/crawl",
  "jsonBody": {
    "url": "https://docs.example.com",
    "limit": 50,
    "scrapeOptions": { "formats": ["markdown"] }
  }
}
// Returns: { "id": "crawl-abc123" }
```

Check status in a loop:

```
// HTTP Request node: Check Status
{
  "method": "GET",
  "url": "=http://localhost:3000/v1/crawl/{{ $json.id }}"
}
// Returns: { "status": "completed", "data": [...pages] }
```

Connect the IF node's "not completed" output back to the Wait node to create a polling loop. When completed, the data array contains all scraped pages.

## Step 5: Data Extraction to Google Sheets

Extract structured data from multiple pages and save to a spreadsheet:

1. **Schedule Trigger** — daily at 9 AM
2. **HTTP Request** — map the target site
3. **Code** — filter URLs to product pages
4. **Split In Batches** — process each URL
5. **HTTP Request** — scrape each page with the json format
6. **Google Sheets** — append extracted data

The extraction request:

```
// HTTP Request: Extract Data
{
  "method": "POST",
  "url": "http://localhost:3000/v1/scrape",
  "jsonBody": {
    "url": "={{ $json.url }}",
    "formats": ["json"],
    "jsonSchema": {
      "type": "object",
      "properties": {
        "product_name": { "type": "string" },
        "price": { "type": "string" },
        "description": { "type": "string" },
        "in_stock": { "type": "boolean" }
      }
    }
  }
}
```

CRW returns structured JSON matching your schema — no regex or HTML parsing needed. Pipe the output directly to a Google Sheets Append Row node.

## Step 6: Content Aggregation with Slack Alerts

Aggregate content from multiple sites and send a daily digest:

```
// Workflow: Daily Content Digest
//
// Schedule (9 AM) → Map Site A → Scrape New Pages → Map Site B → Scrape New Pages
//     → Code (combine + format) → Slack (post digest)

// Code node: Format Digest
const pages = $input.all().map(item => item.json);
const digest = pages
  .map(p => `*${p.data.metadata.title}*\n${p.data.metadata.sourceURL}\n${p.data.markdown.substring(0, 200)}...\n`)
  .join("\n---\n");

return [{ json: { digest, pageCount: pages.length } }];
```

## Tips for n8n + CRW Workflows

- **Use the Wait node** for crawl polling. Set it to 3-5 seconds between status checks.
- **Use Static Data** (`$getWorkflowStaticData`) to persist state between workflow runs — like the last scraped content for change detection.
- **Batch requests** with Split In Batches to avoid overwhelming CRW with concurrent requests. A batch size of 5 works well.
- **Error handling**: add an Error Trigger node and connect it to a Slack/email notification so you know when scraping fails.
- **Use expressions** like `={{ $json.data.markdown }}` to reference scraped content in downstream nodes.

## Self-Hosted vs fastCRW for n8n

Both n8n and CRW can be self-hosted, making this a fully open-source stack. Run them together with Docker Compose:

```
# docker-compose.yml
services:
  crw:
    image: ghcr.io/fastcrw/crw:latest
    ports:
      - "3000:3000"

  n8n:
    image: n8nio/n8n
    ports:
      - "5678:5678"
    environment:
      - N8N_BASIC_AUTH_ACTIVE=true
      - N8N_BASIC_AUTH_USER=admin
      - N8N_BASIC_AUTH_PASSWORD=changeme
    volumes:
      - n8n_data:/home/node/.n8n

volumes:
  n8n_data:
```

For production or when scraping diverse external sites, switch to [fastCRW](https://fastcrw.com):

```
// Change the URL in your HTTP Request nodes:
// From: http://localhost:3000/v1/scrape
// To:   https://api.fastcrw.com/v1/scrape
```

fastCRW handles scaling and reliability, which is important for workflows that scrape many different external sites.

## Why CRW for n8n Workflows?

**REST API fits n8n natively.** CRW's Firecrawl-compatible REST API works directly with n8n's HTTP Request node — no custom integrations or community nodes needed. Any endpoint that Firecrawl supports, CRW supports at the same URLs.

**Low latency matters for scheduled workflows.** When a workflow runs on a schedule and scrapes many pages, a local-first engine keeps each fetch quick so the run finishes well within its window instead of overlapping with the next one.

**Lightweight self-hosting.** CRW is a single small static binary in a lean Docker image. It runs comfortably alongside n8n on a single small VPS without competing for resources.

## Next Steps

- [Build a RAG pipeline](/blog/rag-pipeline-with-crw) from your scraped data
- [Use CRW's MCP server](/blog/mcp-web-scraping) for AI agent integration
- [Compare CRW vs Firecrawl](/blog/firecrawl-vs-crawl4ai-vs-crw) for performance benchmarks

## Get Started

Run CRW and n8n together:

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
docker run -p 5678:5678 n8nio/n8n
```

Or use [fastCRW](https://fastcrw.com) as the scraping backend and skip the CRW container entirely — just point your n8n HTTP Request nodes at `https://api.fastcrw.com`.
