<div class="page-intro">
  <div class="page-kicker">Reference</div>
  <h1>Troubleshooting / FAQ</h1>
  <p class="page-subtitle">Eight common failure patterns with verified causes and fixes — from empty markdown to MCP server visibility issues.</p>
  <div class="page-capabilities">
    <div class="page-capability"><strong>Covers:</strong> scrape, search, MCP, auth, rate limits, credits</div>
    <div class="page-capability"><strong>Verified against:</strong> crw-core/src/error.rs · crw-server/src/middleware.rs · crw-server/src/app.rs</div>
  </div>
</div>

## Quick diagnosis checklist

Before diving into individual patterns:

1. Check `success` in the response body — `false` means a hard failure.
2. Check `error_code` (snake_case string) for the machine-readable cause.
3. Check `data.warnings[]` when `success: true` — soft issues like truncation, unsupported formats, and renderer fallbacks surface there. Anti-bot blocks are **not** in `data.warnings[]`; they always return `success: false` + `error_code: "anti_bot"` (see pattern 2). A page the origin answered with a `>= 400` status returns `success: false`; if it is a plain origin error page the code is `"http_error"` and the page stays in `data`, but when the anti-bot classifier recognises it as a wall (Cloudflare and friends serve their challenge with a 403) the code is `"anti_bot"` and the challenge shell is stripped from `data`.
4. Check `metadata.statusCode` — this is the **target site's** HTTP status, not fastCRW's.
5. Check the fastCRW HTTP status separately — `401`/`422`/`429` all mean different things.

---

## 1. Empty or minimal markdown

**Symptom:** Response is `success: true` but `data.markdown` is an empty string or fewer than 100 characters. The page renders fine in a browser.

**Cause:** The page is a JavaScript Single-Page Application (SPA). The HTTP-only fetcher receives the shell HTML before React/Vue/Next.js mounts content into the DOM. No JavaScript executes, so `readability` sees an empty container.

**Fix:** Set `renderJs: true` to force a headless browser render:

```bash
# cURL
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://app.example.com/dashboard",
    "renderJs": true,
    "waitFor": 1500
  }'
```

```python
# Python — pip install crw
from crw import CrwClient

client = CrwClient(api_key="crw_live_YOUR_KEY")
result = client.scrape(
    "https://app.example.com/dashboard",
    render_js=True,
    wait_for=1500,
)
print(result["markdown"][:300])
```

**Self-hosted:** `renderJs: true` requires a configured CDP renderer (LightPanda or Chrome). If neither is running you will get a warning `"JS rendering was requested but no renderer is available"` and the engine falls back to HTTP. Check `GET /health` and `GET /metrics/renderer-breakers`.

**`wait_for` guidance:** Start with `1000`–`2000` ms for most SPAs. Complex hydration (Next.js App Router, React 18 concurrent mode) may need `3000`–`5000` ms.

---

## 2. Anti-bot block / Cloudflare challenge page

**Symptom:** Response contains `success: false`, `error_code: "anti_bot"`, and the error message names the protection class (e.g., `"Blocked by anti-bot (cloudflare): ..."` or `"Blocked by anti-bot (datadome): ..."`).

**Cause:** The target site fingerprints the request as non-browser traffic. Common triggers: missing `User-Agent`, no `Sec-Fetch-*` headers, a datacenter IP, or a detectable headless browser.

> **Not a block: `"No usable content could be extracted (...)"`.** That case has
> its own code, `error_code: "no_usable_content"` — we fetched the page fine but
> it held nothing usable: a handful of visible characters, no content
> elements. That is usually a broken TLS certificate serving an error stub, a
> parked domain, or a JS shell that never hydrated. The staged anti-bot fixes
> below will not help it. Check the URL in a browser first; if it renders a real
> page there, raise `waitFor` or try `renderJs: true`. Either way you are not
> charged for it.

**Fix (staged):**

1. Add `"stealth": true` to route through randomized headers and a pooled real-browser UA.
2. Switch renderer to `"chrome_proxy"` to egress through residential IPs (fastcrw.com managed tier only):

```bash
# cURL — step 1: stealth headers
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://www.example-protected.com/product/123",
    "renderJs": true,
    "stealth": true
  }'
```

```bash
# cURL — step 2: residential proxy Chrome tier
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://www.example-protected.com/product/123",
    "renderer": "chrome_proxy",
    "stealth": true,
    "country": "us"
  }'
```

```python
# Python — residential proxy tier
from crw import CrwClient

client = CrwClient(api_key="crw_live_YOUR_KEY")
result = client.scrape(
    "https://www.example-protected.com/product/123",
    renderer="chrome_proxy",
    stealth=True,
    country="us",
)
print(result["markdown"][:300])
```

**Note:** `renderer: "chrome_proxy"` is only available on fastcrw.com managed infrastructure. Self-hosted deployments must supply their own residential proxy pool via `proxy` or `proxy_list` and use `renderer: "chrome"`.

**Do not retry anti-bot blocks in a tight loop** — each failed attempt teaches the WAF more about your traffic pattern.

---

## 3. HTTP 422 `target_unreachable`

**Symptom:** API returns HTTP `422` with `error_code: "target_unreachable"` and an error like `"Target unreachable: dns error: failed to lookup address"` or `"connection refused"`.

**Cause:** The fastCRW engine could not establish a TCP connection to the target host. Most common reasons:

- The hostname does not resolve (typo, private DNS, or the domain is down).
- The host refuses connections on port 80/443 (firewall, no HTTP server).
- The URL targets a private/reserved IP that the URL safety validator blocks (e.g., `localhost`, `10.x.x.x`, `192.168.x.x`).

**Fix:**

```bash
# Verify DNS resolves before calling the API
nslookup your-target.example.com

# Check the URL is reachable from outside your network
curl -I https://your-target.example.com

# Check the exact error message in the response body
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url": "https://your-target.example.com"}' | jq '.error'
```

**Error code mapping** (from `crw-server/src/error.rs`):

```
CrwError::TargetUnreachable      → HTTP 422, error_code: "target_unreachable"
CrwError::ExtractionError        → HTTP 422, error_code: "extraction_error"
CrwError::UnsupportedContentType → HTTP 422, error_code: "unsupported_content_type"
```

If the target URL is correct and externally reachable, the issue is intermittent — retry with backoff. If it is consistent, the target host has a problem unrelated to fastCRW.

---

## 4. HTTP 401 Invalid API key

**Symptom:** Every request returns HTTP `401` with `{"success": false, "error": "Invalid API key"}` or `{"success": false, "error": "Missing Authorization header"}`.

**Cause:** The `Authorization` header is missing, uses the wrong format, or the key value does not match any configured key.

The server checks (from `crw-server/src/middleware.rs`):

1. The header must be present and start with exactly `"Bearer "` (capital B, space after).
2. The token after `Bearer ` must match a key in `[auth].api_keys` (constant-time comparison).
3. If `[auth].api_keys` is empty, all requests pass — no auth required.

**Fix:**

```bash
# Correct header format
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer crw_live_YOUR_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com"}'

# Common mistakes that return 401:
# -H "Authorization: crw_live_YOUR_KEY"        ← missing "Bearer "
# -H "Authorization: bearer crw_live_YOUR_KEY" ← lowercase "bearer"
# -H "Authorization: Basic ..."               ← wrong scheme
```

```python
from crw import CrwClient

# Key is passed in the constructor — the SDK sets the header correctly
client = CrwClient(api_key="crw_live_YOUR_KEY")
```

**Self-hosted:** If you did not set `[auth].api_keys` in your config, the API is open to all. If you set keys, pass one of them verbatim as the Bearer token.

---

## 5. HTTP 429 — rate limit vs. credit exhausted

**Symptom:** Requests return HTTP `429`. But two completely different problems share this status code.

**How to tell them apart:**

| Condition | Body `error` contains | Has `Retry-After` header? | Fix |
|---|---|---|---|
| RPM rate limit | `"Rate limit exceeded"` or `"Rate limited"` | Yes (fastcrw.com SaaS load balancer); absent on self-hosted | Respect `Retry-After`; use exponential backoff on self-hosted |
| Credit exhausted | `"Insufficient credits"` | **No** | Top up or enable auto-recharge |

**Rate limit (RPM):**

```bash
# Honor the Retry-After header (fastcrw.com cloud)
RETRY_AFTER=$(curl -sI -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com"}' | grep -i retry-after | awk '{print $2}')
echo "Retry after: $RETRY_AFTER seconds"
```

```python
import time, requests

def scrape_with_retry(url, api_key, max_retries=5):
    headers = {"Authorization": f"Bearer {api_key}", "Content-Type": "application/json"}
    for attempt in range(max_retries):
        resp = requests.post(
            "https://api.fastcrw.com/v1/scrape",
            headers=headers,
            json={"url": url},
        )
        if resp.status_code == 429:
            body = resp.json()
            if "Insufficient credits" in body.get("error", ""):
                raise RuntimeError("Credits exhausted — top up at fastcrw.com/dashboard")
            # RPM rate limit — back off
            wait = int(resp.headers.get("Retry-After", 2 ** attempt))
            time.sleep(wait)
            continue
        resp.raise_for_status()
        return resp.json()
    raise RuntimeError("Max retries exceeded")

result = scrape_with_retry("https://example.com", "crw_live_YOUR_KEY")
```

**Credit exhausted:** Retrying on a timer does not help — the balance does not replenish on its own. Go to `fastcrw.com/dashboard` to top up or enable auto-recharge.

**Self-hosted:** `429` from a self-hosted instance is always the RPM rate limiter (`rate_limit_rps` in config). There is no credit system on self-hosted. No `Retry-After` header is emitted; use client-side exponential backoff.

---

## 6. Search returns empty results

**Symptom:** `POST /v1/search` returns HTTP `503` with `error_code: "search_disabled"`, or it returns `success: true` but `data.results` is an empty array `[]`.

**Two separate problems:**

### 6a. `error_code: "search_disabled"` (503)

**Cause:** The engine has no search backend configured. The search route requires `[search].search_backend_url` in config or the `CRW_SEARCH__SEARCH_BACKEND_URL` environment variable.

**Fix (self-hosted):**

```bash
# Quickest self-hosted setup — boot the bundled search sidecar
crw setup --local
```

```toml
# Or point at a backend you already run
# config.toml
[search]
search_backend_url = "http://localhost:8080"
```

```bash
# Or via environment variable
export CRW_SEARCH__SEARCH_BACKEND_URL=http://localhost:8080
```

```bash
# The bundled docker-compose.yml already wires CRW to the search sidecar
docker compose up -d
```

**fastcrw.com cloud:** `search_disabled` never occurs on the managed SaaS tier. `crw_search` is always available.

### 6b. Empty results array (no error)

**Cause:** The search backend returned zero results — usually because no search engines are enabled on it, the instance is rate-limited by upstream search providers, or the query matched no results.

**Diagnosis:**

```bash
# Hit the backend directly to check it is working
curl "http://localhost:8080/?q=test+query&format=json" | jq '.results | length'

# Check which engines the backend has enabled
curl "http://localhost:8080/config" | jq '.engines[] | select(.enabled == true) | .name'
```

**Fix:** Enable engines in the backend's `settings.yml`, or use the bundled Docker image which comes pre-configured.

---

## 7. MCP server not appearing in AI client

**Symptom:** After adding the MCP config, the tool list in Claude Code / Claude Desktop / Cursor / Windsurf does not show `crw_scrape`, `crw_crawl`, or the other CRW tools.

**Common causes and fixes:**

### 7a. The client was not restarted

MCP servers are registered at client start. Add the config entry then **fully restart the application** (not just reload the window).

### 7b. `npx` is not in PATH when the client launches

GUI applications on macOS may not inherit your shell's PATH. The binary is not found and the MCP process silently fails to start.

```json
// Workaround: use the absolute path to npx
{
  "mcpServers": {
    "crw": {
      "command": "/usr/local/bin/npx",
      "args": ["-y", "crw-mcp"]
    }
  }
}
```

Find your `npx` path with `which npx` in a terminal.

### 7c. Wrong config file location

| Client | Config file |
|---|---|
| Claude Code | `claude mcp add crw -- npx -y crw-mcp` (CLI manages the file) or `~/.claude/mcp.json` |
| Claude Desktop | `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) |
| Cursor | `.cursor/mcp.json` in project root, or `~/.cursor/mcp.json` (global) |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` |

### 7d. `crw_search` is missing but other tools appear (embedded mode)

`crw_search` is only advertised when a search backend is configured. In embedded mode with no backend, the tool is intentionally hidden. To get `crw_search`, either:

- Switch to fastcrw.com cloud mode (`CRW_API_URL` + `CRW_API_KEY`), or
- Run `crw setup --local` to boot a local backend, or set `CRW_SEARCH__SEARCH_BACKEND_URL` to one you already run.

### 7e. Diagnosing with the JSON-RPC wire

The MCP server communicates over stdio JSON-RPC. You can test it directly:

```bash
# List available tools (paste and press Enter, then Ctrl-D)
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' | npx -y crw-mcp 2>/dev/null
```

If this produces a JSON response with a `tools` array, the binary works and the problem is in the client config.

---

## 8. Credit exhausted — 402 vs 429

**Symptom:** The fastcrw.com SaaS returns either HTTP `402` or HTTP `429` related to credits or billing.

**How the SaaS uses these codes:**

| HTTP status | When | Body `error` |
|---|---|---|
| `429` | Credits balance is zero when the request arrives | `"Insufficient credits"` |
| `402` | Payment required (e.g., free tier cap reached, plan expired) | Various billing messages |

**Note for self-hosted users:** Neither `402` nor credit-`429` exists on self-hosted. The only `429` on self-hosted is the RPM rate limiter (`rate_limited` error code). Self-hosted has no billing system.

**Fix:**

```bash
# Check your balance (fastcrw.com SaaS)
curl https://fastcrw.com/api/v1/account/balance \
  -H "Authorization: Bearer $CRW_API_KEY" | jq '{balance, plan}'
```

```python
# Python — check balance before a large crawl
import requests

resp = requests.get(
    "https://fastcrw.com/api/v1/account/balance",
    headers={"Authorization": f"Bearer {api_key}"},
)
data = resp.json()
print(f"Balance: {data['balance']} credits")
if data["balance"] < 100:
    print("Warning: low balance — top up at fastcrw.com/dashboard")
```

**Do not retry a credit-`429` on a timer** — the balance is exhausted and retrying consumes no credits (the request is rejected before any work starts) but it does waste time and may look like abuse. Replenish the balance first.

---

## Error code reference

| `error_code` | HTTP | Source |
|---|---|---|
| `invalid_request` | 400 | Bad JSON, bad URL, missing required field |
| `invalid_url` | — | Reserved — not emitted in practice; invalid URLs are returned as `invalid_request` (HTTP 400) by all server routes |
| `target_unreachable` | 422 | DNS failure, connection refused, host down |
| `extraction_error` | 422 | LLM extraction failed or CSS/XPath selector invalid |
| `unsupported_content_type` | 422 | The origin answered with a body that is neither a web page nor a PDF (an office document, an image, an archive). Not retryable: no renderer turns one into a page |
| `http_error` | 502, or 200 with `success: false` | The origin answered with a `>= 400` status and what came back is its error page. The body is kept in `data` so you can read the error page and `metadata.statusCode`. A large page served under an error status is still returned as a success — some sites answer 403/404 while serving the real content. If the page is also recognised as an anti-bot wall, `anti_bot` wins instead and the body is cleared |
| `no_usable_content` | 200 with `success: false` | The fetch worked and produced nothing you asked for — a parked domain, an un-hydrated JS shell, an error stub, an oversized PDF the decompression-bomb guard refused, or any requested format that came back empty. Not an anti-bot block |
| `timeout` | 504 | Engine or upstream search timed out |
| `rate_limited` | 429 | RPM rate limit (engine-level) |
| `search_disabled` | 503 | `/v1/search` called with no search backend configured |
| `not_found` | 404 | Unknown endpoint or crawl job ID does not exist |
| `anti_bot` | 200 | Anti-bot wall detected and no renderer tier could clear it. Always `success: false`, and the page body is cleared. Detect via `error_code: "anti_bot"`, never via the HTTP status: the request reached the target and the target refused it, so this is not a gateway failure and is never billed |
| `renderer_error` | 500 | CDP browser internal error |
| `internal_error` | 500 | Unexpected engine failure |

Source: `crw-core/src/error.rs` (`error_code()` method), `crw-server/src/error.rs` (HTTP status mapping), and `crw-server/src/routes/scrape.rs` (the `http_error` / `no_usable_content` / `anti_bot` split).
