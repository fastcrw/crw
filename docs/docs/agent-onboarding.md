# Agent Onboarding Guide

## Overview

This guide is written for AI agents and the systems that orchestrate them. It covers the minimum you need to start making API calls, the most common patterns, and how to handle errors gracefully.

## Authentication

Every request requires an API key in the `Authorization` header:

```
Authorization: Bearer YOUR_API_KEY
```

:::note
On self-hosted instances, authentication is configured via your server settings. Cloud only (fastcrw.com) provides a dashboard for key management.
:::

## Available Endpoints

| Endpoint | Method | Purpose |
| --- | --- | --- |
| `/v1/scrape` | POST | Extract content from a single URL |
| `/v1/crawl` | POST | Start an async recursive crawl job |
| `/v1/crawl/:id` | GET | Poll status of a crawl job |
| `/v1/map` | POST | Discover all reachable URLs on a domain |
| `/v1/search` | POST | Search the web and return results with content |
| `/firecrawl/v2/parse` | POST | Parse a file (PDF) into markdown or structured output |

## Quick Start Pattern

For most agent workflows, start with this sequence:

1. **Discover** what pages exist on a target domain:

```bash
curl -X POST https://api.fastcrw.com/v1/map \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com"}'
```

2. **Extract** content from specific pages:

```bash
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com/page", "formats": ["markdown"]}'
```

3. **Search** when you need to find relevant pages across the web:

```bash
curl -X POST https://api.fastcrw.com/v1/search \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"query": "your search query", "limit": 5}'
```

## Crawl: Start and Poll

Crawls are asynchronous. POST to `/v1/crawl` returns a job ID immediately; you must poll `GET /v1/crawl/:id` until the status reaches `completed` or `failed`.

### Start a crawl

```bash
curl -X POST https://api.fastcrw.com/v1/crawl \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com", "maxPages": 50, "formats": ["markdown"]}'
```

Response:

```json
{
  "success": true,
  "id": "3f2a1b4c-8e9d-4f7a-b6c5-2d1e0f9a8b7c"
}
```

### Poll until done

`GET /v1/crawl/:id` returns:

| Field | Type | Notes |
| --- | --- | --- |
| `status` | string | `scraping` \| `completed` \| `failed` |
| `total` | number | Total pages discovered |
| `completed` | number | Pages scraped so far |
| `data` | array | Pages scraped so far — **partial during `scraping`** |
| `error` | string | Present only when `status` is `failed` |

**Important:** `data[]` is populated incrementally. You can consume partial results while the crawl is still `scraping`.

Poll loop example (bash):

```bash
ID="3f2a1b4c-8e9d-4f7a-b6c5-2d1e0f9a8b7c"
while true; do
  RESULT=$(curl -s -H "Authorization: Bearer $API_KEY" \
    "https://api.fastcrw.com/v1/crawl/$ID")
  STATUS=$(echo "$RESULT" | jq -r '.status')
  echo "Status: $STATUS  ($(echo "$RESULT" | jq '.completed')/$(echo "$RESULT" | jq '.total') pages)"
  if [ "$STATUS" = "completed" ] || [ "$STATUS" = "failed" ]; then
    break
  fi
  sleep 3
done
echo "$RESULT" | jq '.data | length'
```

Python example:

```python
import time
import requests

def poll_crawl(crawl_id: str, api_key: str, interval: float = 3.0) -> dict:
    url = f"https://api.fastcrw.com/v1/crawl/{crawl_id}"
    headers = {"Authorization": f"Bearer {api_key}"}
    while True:
        resp = requests.get(url, headers=headers)
        resp.raise_for_status()
        data = resp.json()
        status = data["status"]
        print(f"status={status}  pages={data['completed']}/{data['total']}")
        if status in ("completed", "failed"):
            return data
        time.sleep(interval)
```

## Response Format

All endpoints return JSON with a consistent structure:

```json
{
  "success": true,
  "data": { ... }
}
```

On failure:

```json
{
  "success": false,
  "error": "description of what went wrong",
  "error_code": "machine_readable_code"
}
```

## Error Handling for Agents

Agents must distinguish between different failure modes — some are permanent, some are transient, and some require user action.

| Status | Meaning | Agent action |
| --- | --- | --- |
| 200 | Success | Process the response |
| 400 | Bad request — invalid parameters or URL | Fix the request; do not retry as-is |
| 401 | Invalid or missing API key | Check authentication; do not retry |
| 402 | Out of credits (`errorCode: "insufficient_credits"`) or a managed-LLM feature on the FREE plan (`errorCode: "llm_not_available"`) | Alert the user; do not retry. Branch on `errorCode`. When an enabled auto-recharge could not run, `X-FASTCRW-Stop-Reason` names the cause |
| 404 | Resource not found (e.g. crawl ID does not exist) | Do not retry |
| 422 | Unprocessable entity — URL is valid but target is unreachable or extraction failed | Log and skip; do not retry the same URL |
| 429 | Rate or concurrency limited | Back off and retry after the `Retry-After` interval. On the legacy `/v2` surface only, a `429` carrying `X-FASTCRW-Credits-Available` means out of credits: do not retry |
| 500 | Internal server error (renderer crash, internal error) | Retry once with exponential backoff; if persistent, reduce request complexity or contact support |
| 502 | Bad gateway — upstream fetch error from the target site | Retry with exponential backoff; the target may be temporarily unavailable |
| 503 | Service unavailable — a required subsystem (e.g. search) is down | Retry with exponential backoff |
| 504 | Gateway timeout — the request exceeded the deadline | Retry; consider passing a larger `deadlineMs` or reducing `maxPages` for crawls |

### 402 vs 429: credit awareness

These two codes have different meanings and require different handling:

- **402** means the request cannot run until the account changes. `errorCode: "insufficient_credits"` is an exhausted balance (the FREE plan's one-time lifetime credits, or a paid plan's included and top-up credits); `errorCode: "llm_not_available"` is a managed-LLM feature on the FREE plan. Retrying will not help. The response headers carry `X-FASTCRW-Credits-Available` and `X-FASTCRW-Upgrade-Url`, plus `X-FASTCRW-Stop-Reason` when an enabled auto-recharge could not run.
- **429** means slow down: a request-rate or concurrency limit. Honor `Retry-After`. The legacy Firecrawl-compatible `/v2` surface (root `/v2`, `/firecrawl/v2`, `compat.fastcrw.com`) still answers an exhausted balance with a `429` without an `errorCode`.

:::note
Stop-reason values (`card_declined`, `action_required`, `no_payment_method`, `no_consent`, `error`) and the response headers listed above (`X-FASTCRW-Stop-Reason`, `X-FASTCRW-Credits-Available`, `X-FASTCRW-Upgrade-Url`) are SaaS billing internals; verify against the [fastcrw.com dashboard docs](https://fastcrw.com/docs) if exact values matter for your integration.
:::

Your agent should branch on `errorCode` and the status code, not guess from the error message:

```python
if resp.status_code == 402:
    body = resp.json()
    stop_reason = resp.headers.get("X-FASTCRW-Stop-Reason")
    upgrade_url = resp.headers.get("X-FASTCRW-Upgrade-Url", "https://fastcrw.com/dashboard/billing")
    raise RuntimeError(f"{body.get('errorCode')} ({stop_reason or 'no auto-recharge'}): {body.get('error')} Top up at {upgrade_url}")
elif resp.status_code == 429:
    # Rate or concurrency limit: wait for Retry-After, then retry.
    time.sleep(int(resp.headers.get("Retry-After", "2")))
```

## Handling JS-Gated Pages

If `crw_scrape` returns an empty or near-empty `markdown` field for a page that visually has content, the page is likely JS-gated (client-side rendered, anti-bot wall, or heavy SPA). The engine's auto-detect heuristic may have missed it, or the page requires real browser interaction (login flow, cookie consent, infinite scroll).

In that case, use **crw-browse** — the separate interactive browser MCP server that gives your agent full CDP control:

- `goto` — navigate to a URL and wait for load
- `tree` — inspect the accessibility tree to find elements
- `click`, `fill`, `type_text` — interact with forms and UI
- `evaluate` — run JavaScript on the page
- `text` / `html` — read page content after JS execution
- `console` — drain the console-message ring buffer (up to 200 entries; filter by level)
- `network` — drain the network-request ring buffer (up to 500 entries; filter by status)
- `storage` — read or write cookies, localStorage, or sessionStorage
- `screenshot` — capture the page as PNG/JPEG (requires `--chrome-ws-url`)
- `wait` — block until a CSS selector is present/visible or a page condition fires (`load`, `networkidle`)
- `script` — execute a sequence of up to 50 tool calls in a single request

crw-browse is a separate binary (`crw-browse`), not a tool inside the main MCP server. See the [crw-browse documentation](/docs/crw-browse) for setup. As a quick heuristic: if scrape returns `markdown: ""` or a very short markdown with no meaningful content, switch to crw-browse for that URL.

## Common Agent Patterns

### Research Loop

```
search(topic) -> scrape(top results) -> analyze -> search(refined query) -> repeat
```

### Site Exploration

```
map(domain) -> filter URLs -> scrape(relevant pages) -> synthesize
```

### Full-Site Crawl

```
crawl(domain) -> poll until completed/failed -> process data[]
```

### Monitoring

```
scrape(url) -> store result -> wait -> scrape(url) -> compare with previous
```

### JS-Gated Fallback

```
scrape(url) -> if markdown empty -> crw-browse goto(url) -> tree/text -> extract
```

## Rate Limits

Check the [rate limits documentation](/docs/rate-limits) for current limits. Agents should respect `Retry-After` headers and implement exponential backoff.

## MCP Integration

If your agent runtime supports MCP, see the [MCP guide](/docs/mcp) for a simpler integration path that avoids direct HTTP calls.
