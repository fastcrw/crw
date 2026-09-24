# Configuration

Configuration is loaded in layers: **defaults → config file → environment variables**.

## Config files

crw looks for configuration in this order:

1. `config.default.toml` (embedded defaults)
2. `config.local.toml` in the working directory
3. File specified by `CRW_CONFIG` environment variable

## Full reference

```toml
[server]
host = "0.0.0.0"
port = 3000
request_timeout_secs = 120
rate_limit_rps = 10              # Max requests/second (global). 0 = unlimited.

[renderer]
mode = "auto"               # auto | lightpanda | playwright | chrome | camoufox | none
page_timeout_ms = 30000
pool_size = 4
# render_js_default = true  # alias: force_js = true
                            # forces JS rendering when a request omits `renderJs`

[renderer.lightpanda]
ws_url = "ws://127.0.0.1:9222/"

# [renderer.playwright]
# ws_url = "ws://playwright:9222"

# [renderer.chrome]
# ws_url = "ws://chrome:9222"

# Residential proxy tier (opt-in 4th renderer). When base credentials are set
# AND a chrome_proxy ws_url is configured, the engine adds a chrome_proxy tier
# to the fallback chain (after lightpanda → chrome). Country is selected per
# request via the `country` field on the scrape body; see JS rendering docs.
# proxy_base_user = ""              # base username, WITHOUT __cr.<cc> suffix
# proxy_base_pass = ""
# proxy_default_country = "us"      # 2-letter ISO 3166-1 alpha-2, lowercase

# [renderer.chrome_proxy]
# ws_url = "ws://chrome-proxy:9222"

[crawler]
max_concurrency = 10
requests_per_second = 10.0
respect_robots_txt = true
user_agent = "CRW/0.0.1"
default_max_depth = 2
default_max_pages = 100
job_ttl_secs = 3600
# proxy = "http://proxy:8080"                 # HTTP proxy
# proxy = "socks5://user:pass@proxy:1080"     # SOCKS5 proxy (also supports http://, https://)

# [crawler.stealth]
# enabled = true             # inject browser-like headers + rotate UA globally
# inject_headers = true      # send Accept, Sec-Fetch-*, etc. on every HTTP request
# jitter_factor = 0.2        # ±20% random jitter on rate-limit intervals
# user_agents = []           # custom UA pool; empty = use built-in Chrome/Firefox/Safari pool

[extraction]
default_format = "markdown"
only_main_content = true

# Per-domain CSS selector overrides applied before readability narrowing.
# Map key is the exact host (no wildcards). User-supplied request `selector` wins.
# [extraction.domain_selectors]
# "example.com" = "article, main"

# LLM-assisted extraction fallback. Fires when DOM-based extraction scores below
# `quality_threshold`. Disabled by default — requires [extraction.llm] to be set.
# [extraction.llm_fallback]
# enable = false
# quality_threshold = 0.3    # score below this triggers the LLM fallback
# max_html_bytes = 100000    # truncate HTML before sending to LLM
# always_run = false         # true = LLM on every page (higher cost, higher recall)

[extraction.llm]
provider = "anthropic"       # also: "openai", "openai-responses", "deepseek", "azure", "openai-compatible"
api_key = ""
model = "claude-sonnet-4-20250514"
max_tokens = 4096
max_html_bytes = 100000      # content fed to LLM is truncated at this byte count
max_concurrency = 4          # bounded fan-out for per-result summaries in /v1/search
# base_url = ""              # for custom Chat Completions or Responses endpoints
# azure_api_version = ""     # required when provider = "azure"
# require_byok_header = ""   # tenant guard: reject LLM requests missing this header AND without llmApiKey

# Responses-compatible gateway:
# provider = "openai-responses"
# model = "provider-model-id"
# base_url = "https://gateway.example.com/v1"  # /responses is appended

# /v1/search endpoint — proxies to a self-hosted search backend.
# Absence of search_backend_url disables /v1/search with HTTP 503 (error_code: "search_disabled").
# [search]
# enabled = true               # set false to disable /v1/search even if search_backend_url is set
# search_backend_url = "http://localhost:8080"
# timeout_ms = 15000
# default_limit = 5
# max_limit = 20
# rerank_enabled = true        # RRF+BM25 re-rank for LLM answer/summarize path

# /v1/map URL filter — strips tracking params and drops action URLs from map results.
# [map.url_filter]
# strip_tracking_params = true
# drop_action_urls = true
# gov_tld_drop_actions = false   # when true, .gov/.mil hosts also run action-URL drop
# extra_tracking_params = []     # additive on top of built-in list
# extra_action_params = []
# extra_preserve_params = []

# Document (PDF) parsing controls.
# [document]
# enabled = true
# max_pages = 0              # 0 = no limit
# attempt_scanned = false    # best-effort text from image PDFs (no OCR)
# max_upload_bytes = 52428800   # 50 MiB — POST /firecrawl/v2/parse upload cap
# max_concurrent_parses = 4    # process-wide cap on concurrent PDF parses (primary DoS guard)
# upload_concurrency = 4        # max concurrent upload-body buffers (each up to max_upload_bytes)
# parse_timeout_ms = 30000      # wall-clock timeout per parse; 0 disables
# max_decompressed_bytes = 104857600  # 100 MiB decompression-bomb guard; 0 disables
# sandbox = false            # isolate each parse in a child process (Unix only)
# sandbox_memory_bytes = 536870912   # RLIMIT_AS for sandbox child (Unix); 512 MiB

# MCP tool-response shaping (self-hosted). Strips credit-billing fields
# (`creditCost` / `creditsUsed`) from MCP tool results — saves context tokens
# and keeps the model's attention on the payload. MCP surfaces only; the REST
# API is unchanged.
# [mcp]
# hide_credits = false

[auth]
# api_keys = ["crw_live_key-1234"]
```

## Stealth mode

Enable globally to make CRW look like a real browser on every HTTP request:

```toml
[crawler.stealth]
enabled = true
```

When enabled:
- User-Agent is rotated from a built-in pool of Chrome 131, Firefox 133, and Safari 18 strings
- 12 browser-like headers are injected: `Accept`, `Accept-Language`, `Accept-Encoding`, `Sec-Ch-Ua`, `Sec-Ch-Ua-Mobile`, `Sec-Ch-Ua-Platform`, `Sec-Fetch-Dest`, `Sec-Fetch-Mode`, `Sec-Fetch-Site`, `Sec-Fetch-User`, `Priority`, `Upgrade-Insecure-Requests`

Override per-request by setting `stealth: true/false` in the scrape payload.

### Stealth profile (`config.stealth.toml`)

For higher-yield scraping against bot-detecting sites, CRW ships a ready-made `config.stealth.toml` overlay that also wires in a [browserless v2](https://github.com/browserless/browserless) Chrome backend with its anti-fingerprint plugin enabled. Use it when `[crawler.stealth]` alone is not enough and you need a full stealth Chrome session.

```bash
# Start browserless via Docker Compose (stealth profile)
docker compose --profile stealth up -d

# Run crw-server on the host, loading the stealth overlay
CRW_CONFIG=config.stealth crw-server
```

What the overlay changes beyond `[crawler.stealth]` (key settings):

| Setting | Value | Effect |
|---------|-------|--------|
| `renderer.chrome.ws_url` | `ws://localhost:9224/chromium?token=crwtest&stealth=true` | Routes Chrome sessions to browserless with anti-fingerprint plugin. `crwtest` is a placeholder — set `BROWSERLESS_TOKEN` in `.env` and update this URL to match. |
| `renderer.chrome_intercept_resources` | `true` | Blocks fonts/images in Chrome to cut per-page latency |
| `renderer.chrome_nav_budget_ms` | `12 000` | Longer nav budget for heavy JS sites |
| `renderer.http_timeout_ms` | `4 000` | Tighter HTTP timeout (fast fail to browser fallback) |
| `renderer.lightpanda_timeout_ms` | `2 500` | Tighter LightPanda budget so Chrome always has residual time within the request deadline |
| `server.rate_limit_rps` | `0` | Disables global rate limit for local/private deploys |

> **License note:** browserless v2 is SSPL-3.0. If you expose this stack to third parties as a service, review SSPL §13 compliance before deploying. The default `chrome` profile (chromedp / headless-shell, Apache-2/BSD) carries no such obligation.

## Environment variables

Use the `CRW_` prefix with `__` as a nesting separator:

| Config | Environment Variable |
|--------|---------------------|
| `server.port` | `CRW_SERVER__PORT` |
| `server.host` | `CRW_SERVER__HOST` |
| `renderer.mode` | `CRW_RENDERER__MODE` |
| `renderer.render_js_default` | `CRW_RENDERER__RENDER_JS_DEFAULT` (alias: `CRW_RENDERER__FORCE_JS`) |
| `crawler.max_concurrency` | `CRW_CRAWLER__MAX_CONCURRENCY` |
| `crawler.requests_per_second` | `CRW_CRAWLER__REQUESTS_PER_SECOND` |
| `server.rate_limit_rps` | `CRW_SERVER__RATE_LIMIT_RPS` |
| `crawler.stealth.enabled` | `CRW_CRAWLER__STEALTH__ENABLED` |
| `crawler.proxy` | `CRW_CRAWLER__PROXY` |
| `renderer.proxy_base_user` | `CRW_RENDERER__PROXY_BASE_USER` |
| `renderer.proxy_base_pass` | `CRW_RENDERER__PROXY_BASE_PASS` |
| `renderer.proxy_default_country` | `CRW_RENDERER__PROXY_DEFAULT_COUNTRY` |
| `extraction.llm.api_key` | `CRW_EXTRACTION__LLM__API_KEY` |
| `extraction.llm.provider` | `CRW_EXTRACTION__LLM__PROVIDER` |
| `extraction.llm.model` | `CRW_EXTRACTION__LLM__MODEL` |
| `extraction.llm.base_url` | `CRW_EXTRACTION__LLM__BASE_URL` |
| `extraction.llm.max_html_bytes` | `CRW_EXTRACTION__LLM__MAX_HTML_BYTES` |
| `extraction.llm.max_concurrency` | `CRW_EXTRACTION__LLM__MAX_CONCURRENCY` |
| `extraction.llm.require_byok_header` | `CRW_EXTRACTION__LLM__REQUIRE_BYOK_HEADER` |
| `mcp.hide_credits` | `CRW_MCP__HIDE_CREDITS` |
| _(boot guard)_ | `CRW_DISABLE_SERVER_LLM_KEY` — when set to `1`, refuses to boot if `[extraction.llm].api_key` is also configured. Use behind a SaaS proxy that injects per-request LLM keys. |

## Renderer modes

| Mode | Description |
|------|-------------|
| `auto` | HTTP first, auto-detect SPAs, CDP fallback |
| `lightpanda` | Always use LightPanda via CDP |
| `playwright` | Always use Playwright via CDP |
| `chrome` | Always use Chrome via CDP |
| `camoufox` | Opt-in Camoufox stealth tier (REST). Requires `--features camoufox` + a `[renderer.camoufox]` endpoint |
| `none` | HTTP only, no JS rendering |

The server `mode` controls **availability** of renderers in the pool. Per-request `renderer` selects from what's available — see [JS rendering](#js-rendering). A request that pins an unavailable renderer returns HTTP 400 with the configured pool listed.

## Docker configuration

For Docker deployments, use `config.docker.toml` or environment variables:

```bash
docker run -p 3000:3000 \
  -e CRW_SERVER__PORT=3000 \
  -e CRW_RENDERER__MODE=lightpanda \
  -e CRW_EXTRACTION__LLM__API_KEY=sk-... \
  ghcr.io/fastcrw/crw:latest
```
