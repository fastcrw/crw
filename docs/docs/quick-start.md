<div class="page-intro">
  <div class="page-kicker">Get Started</div>
  <h1>Quick Start</h1>
  <p class="page-subtitle">Get your first successful CRW response in under three minutes. This page follows the API path; the optional MCP path (giving an AI assistant live web access) is at the bottom.</p>
  <div class="page-capabilities">
    <div class="page-capability"><strong>Goal:</strong> first success in under 3 minutes</div>
    <div class="page-capability"><strong>Base URL:</strong> <code>https://api.fastcrw.com</code></div>
    <div class="page-capability"><strong>Free tier:</strong> 500 credits to start, up to 500 more from onboarding tasks, no card required</div>
  </div>
  <div class="page-actions">
    <a class="page-btn primary" href="https://fastcrw.com/register?ref=docs-quick-start-inline" target="_blank" rel="noopener">Get API key</a>
    <a class="page-btn secondary" href="https://fastcrw.com/playground" target="_blank" rel="noopener">Open Playground</a>
  </div>
</div>

## Prerequisites

> **New to CRW? Use `/v1`.** This quick start uses the native `/v1/scrape` route. The `/firecrawl/v2` routes exist for Firecrawl SDK migration and compatibility-only features.

:::info
**Prerequisites**

- **Terminal** — macOS Terminal, Linux shell, or Windows [WSL](https://learn.microsoft.com/en-us/windows/wsl/install)
- **`curl`** — ships with macOS 10.15+ and most Linux distros; Windows users can use WSL or [download curl](https://curl.se/windows/)
- **A free account** at [fastcrw.com/register](https://fastcrw.com/register?ref=docs-quick-start-inline): 500 credits to start, and up to 500 more from onboarding tasks, no card required
- **Node.js 18+** — only for the MCP path (optional, described at the bottom of this page)
:::

## Get a key

Register at [fastcrw.com/register](https://fastcrw.com/register?ref=docs-quick-start-inline). Once you confirm your email, your API key appears on the dashboard. Your account starts with **500 credits, and up to 500 more from onboarding tasks**. one credit equals one basic scrape request, so you have plenty to explore.

Copy the key and keep it somewhere safe. You will paste it into the `Authorization` header below.

## First scrape

Paste your key in place of `YOUR_API_KEY` and run:

```bash
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://example.com",
    "formats": ["markdown"]
  }'
```

**Windows (PowerShell / Command Prompt)**:

```powershell
curl.exe -X POST https://api.fastcrw.com/v1/scrape ^
  -H "Authorization: Bearer YOUR_API_KEY" ^
  -H "Content-Type: application/json" ^
  -d "{\"url\":\"https://example.com\",\"formats\":[\"markdown\"]}"
```

**Expected response:**

```json
{
  "success": true,
  "data": {
    "markdown": "# Example Domain\n\nThis domain is for use in illustrative examples...",
    "metadata": {
      "title": "Example Domain",
      "sourceURL": "https://example.com",
      "statusCode": 200,
      "elapsedMs": 38
    }
  }
}
```

## Confirm success

A good response has three signs:

1. `"success": true` at the top level.
2. A non-empty `"markdown"` string inside `data`.
3. `metadata.statusCode` of `200`.

If you see all three, you are unblocked for almost every other page in this docs set. Check your remaining balance any time on the [fastcrw.com dashboard](https://fastcrw.com/dashboard).

## Something went wrong?

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `401 Unauthorized` | Key missing or malformed | Make sure the header is exactly `Authorization: Bearer YOUR_API_KEY` with the `Bearer ` prefix and no extra spaces |
| `404 Not Found` | URL typo in the request path | Check that you are calling `/v1/scrape`, not `/scrape` or `/api/scrape` |
| `429 Too Many Requests` | Request-rate limit or per-plan concurrency cap | Wait for the `Retry-After` interval and retry; see [Rate Limits](/docs/rate-limits) |
| `402 Payment Required` | `errorCode: "insufficient_credits"`: your credits are used up (the FREE plan's one-time lifetime allowance, or a paid plan's included and top-up credits). `errorCode: "llm_not_available"`: a managed-LLM feature on the FREE plan | Do not retry. Upgrade, buy a credit pack, or fix the payment method named in `X-FASTCRW-Stop-Reason`, then retry |
| `"markdown": ""` (empty string) | Page requires JavaScript to render | Add `"renderJs": true` to your request body and retry |
| `"success": false` with an error message | Request body issue | Double-check JSON syntax — trailing commas and unquoted keys both cause parse errors |

## What's next

You now know how to call the engine. The next decision is which endpoint fits your use case:

→ **[Choose Your Endpoint](choose-endpoint.md)** — a quick decision tree across `scrape`, `map`, `crawl` (multi-page traversal), `search`, `extract`, and `parse`.

## MCP path (optional)

MCP (Model Context Protocol) is a standard that lets AI assistants call external tools — in this case, live web scraping — without you writing any glue code. If your goal is to give Claude, Cursor, Windsurf, or another MCP-compatible assistant live web access, this is the fastest way to do it.

**Requires Node.js 18+** ([nodejs.org/en/download](https://nodejs.org/en/download/)).

```bash
claude mcp add crw -- npx -y crw-mcp
```

Once registered, your AI assistant gains nine web tools (`crw_scrape`, `crw_crawl`, `crw_check_crawl_status`, `crw_map`, `crw_extract`, `crw_check_extract_status`, `crw_cancel_extract`, `crw_search` (web search; available when a search backend is configured), and `crw_parse_file`). On the next conversation turn the assistant can call them automatically whenever it needs to fetch a live page or run a crawl. No further setup is needed for the embedded mode.

For client-specific config files (Codex, Cursor, Windsurf, Cline, Continue.dev) and the proxy mode that connects to the fastcrw.com cloud, see [MCP Server](mcp.md).
