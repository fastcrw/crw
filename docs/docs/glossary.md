# Glossary

Plain-English definitions of terms used throughout the fastCRW documentation, in alphabetical order.

---

## BFS (Breadth-First Crawl)

Breadth-first search is the traversal strategy fastCRW uses for the [`/v1/crawl`](/docs/crawling) endpoint. Starting from a seed URL, the crawler visits all links at depth 1 before moving on to depth 2, then depth 3, and so on. This means you get a broad cross-section of a site early rather than going deep into one branch first. The crawl uses an async VecDeque queue internally and stops when `maxPages`, `maxDepth`, or an empty queue is reached. See [Architecture](/docs/architecture) for the internal request flow.

---

## CDP (Chrome DevTools Protocol)

Chrome DevTools Protocol is the WebSocket-based protocol that lets external programs control a running Chromium browser — navigating pages, waiting for JavaScript to finish, clicking elements, and reading the final DOM. fastCRW uses CDP (via `tokio-tungstenite`) to power its `chrome` and `chrome_proxy` rendering modes. When a page cannot be scraped cleanly by an HTTP request alone, the engine can fall back to CDP to obtain the fully rendered HTML. See [JS Rendering](/docs/js-rendering) for the rendering modes and per-request `renderer` field.

---

## changeTracking

`changeTracking` is one of the 9 output formats you can request in `formats`. Instead of returning page content, it returns a diff object comparing the current scrape result against a prior snapshot you supply. The diff includes a status (`same` or `changed`) and a `diff` envelope (`{ text?, json? }`): in `gitDiff` mode, `text` is the unified markdown diff and `json` is a parse-diff AST; in `json` mode, `json` is a per-field path map (`{"<path>": {previous, current}}`); in mixed mode, `text` carries the unified markdown diff and `json` carries the per-field path map (the AST is not emitted in mixed mode). When no `previous` snapshot is supplied, `first_observation: true` is set on the result. The monitoring layer additionally tracks `new`, `removed`, and `error` states at the set level, but those are not part of the `changeTracking` format output itself. It is the stateless primitive that powers the full [Monitoring](/docs/monitoring) feature: monitors run changeTracking automatically on a schedule, store the snapshots server-side, and notify you via webhook or email when meaningful changes are detected. Self-hosted users can use changeTracking directly without the monitoring control-plane.

---

## chunkStrategy

`chunkStrategy` is an optional request field on `POST /v1/scrape` that tells the engine to split the page's markdown into smaller pieces server-side before returning it. You supply an object with a `type` (`"topic"` to split on headings, `"sentence"` to split on punctuation, or `"regex"` for a custom pattern), an optional `maxChars` cap per chunk, and an optional `dedupe` flag to drop near-duplicate chunks. The engine returns the chunks as a `chunks` array alongside the normal `markdown` field — no text-splitting library needed on the client side. Note that `chunkStrategy` only works on `POST /v1/scrape`; it is not forwarded by batch or crawl jobs. See the [RAG recipe](/docs/recipe-rag) for a complete example.

---

## credit

A credit is the billing unit for the hosted cloud at `fastcrw.com`, worth $0.001. Every scrape costs 1 credit regardless of renderer (HTTP, lightpanda, or Chrome); there is no renderer or proxy surcharge. LLM-backed work (`POST /v1/extract`, or `formats: ["json"]` / `summary` on scrape) costs that 1-credit base render plus the actual LLM cost of the call, dynamically metered: there is no flat credit number for it. See the [Credit Costs](/docs/credit-costs) table for current billing. A map call costs 1 credit, a search call costs 2 credits, and crawl jobs charge 1 credit at start plus one additional credit per page as pages are discovered during polling. The FREE plan grants 500 credits once, for the lifetime of the account, not monthly, and they never reset, plus up to 500 more from onboarding tasks. Purchased top-up credits never expire and survive plan cancellation. Self-hosted deployments have no billing layer and are unaffected by credit costs. Check your balance with `GET /api/v1/account/balance` on the SaaS control-plane. See [Credit Costs](/docs/credit-costs) for the full table.

---

## Firecrawl-compatible

"Firecrawl-compatible" means fastCRW exposes a `/firecrawl/v2` compatibility layer for existing Firecrawl v2 SDK integrations. It is intentionally close enough for migration work, but it is not the recommended API for new fastCRW builds. New projects should start with `/v1`; migration projects should validate the documented differences before switching production traffic. See [Migrate from Firecrawl](/docs/migrate-from-firecrawl) for the exact list of changes and [Compatibility](/docs/compatibility) for the behavior matrix.

---

## llmProvider

`llmProvider` is a request field that selects which LLM provider fastCRW routes extraction or summarization calls to. Accepted values are `"anthropic"`, `"openai"`, `"openai-responses"`, `"deepseek"`, `"azure"`, and `"openai-compatible"`. Use it together with `llmApiKey` and `llmModel` to pin a specific provider per request, or set the server-wide default in `config.toml` under `[extraction.llm]`. When using Azure or a custom Chat Completions/Responses endpoint, also supply `baseUrl`. See [Extract](/docs/extract) and [Output Formats](/docs/output-formats) for full field references.

---

## MCP (Model Context Protocol)

Model Context Protocol is an open standard that lets AI assistants (Claude Code, Claude Desktop, Cursor, Windsurf, Cline, and others) discover and call external tools through a uniform interface. fastCRW ships a built-in MCP server (`crw-mcp`) that exposes 10 tools: `crw_scrape`, `crw_crawl`, `crw_check_crawl_status`, `crw_map`, `crw_extract`, `crw_check_extract_status`, `crw_cancel_extract`, `crw_enrich`, `crw_search`, and `crw_parse_file`. Proxy mode drops the embedded-only `crw_enrich` and adds `crw_find_data` and `crw_get_data` (11 tools). In embedded mode the MCP binary runs the scraping engine inline, so no separate server is needed. In proxy mode it forwards calls to a remote CRW server (the hosted cloud or your own self-hosted instance). See [MCP Server](/docs/mcp) for setup and [MCP Client Setup](/docs/mcp-clients) for host-specific config snippets.

---

## renderer / renderedWith

`renderer` is a per-request field that pins which rendering engine to use for a scrape: `"auto"` (default fallback chain), `"lightpanda"`, `"chrome"`, `"chrome_proxy"` (residential-proxy Chrome tier), or `"impersonated-http"` (a JS-less HTTP tier that presents a real Chrome TLS/HTTP2 fingerprint; it never implies `renderJs`). Self-hosted instances can additionally configure `"playwright"`; it is not available on the managed cloud. It overrides the server-level `mode` setting for that one request. `renderedWith` is the response-side counterpart — a field in `data.metadata` that tells you which renderer actually ran (`"http"`, `"impersonated-http"`, `"lightpanda"`, `"chrome"`, `"chrome_proxy"`, or `"http_only_fallback"`). Inspecting `renderedWith` is the first step in debugging empty or incomplete scrape results on JavaScript-heavy pages. See [JS Rendering](/docs/js-rendering).

---

## search backend

The self-hostable meta-search service that powers the `POST /v1/search` endpoint. It aggregates results from multiple search providers without exposing queries to any single third party, and it runs as one extra container alongside the `crw` binary. fastCRW bundles it as a sidecar in its Docker Compose stack, so `docker compose up` starts it automatically at `searxng:8080` inside the Compose network — no third-party search API key required. You can point fastCRW at a backend you already run with `CRW_SEARCH__SEARCH_BACKEND_URL`, or disable search entirely with `[search].enabled = false`. On the hosted cloud at `fastcrw.com`, it is managed for you. See [Search](/docs/search) and [Docker](/docs/docker).

---

## v1 vs v2 API

fastCRW exposes two route families at `https://api.fastcrw.com`. **v1** (`/v1/*`) is the native, recommended API for new integrations: scrape, crawl, map, search, structured extraction through scrape, and change tracking. **v2** (`/firecrawl/v2/*`) is a Firecrawl v2 compatibility layer with object-format fields, paginated crawl/batch status, a V2Document response shape, batch scraping (`POST /firecrawl/v2/batch/scrape`), file parsing (`POST /firecrawl/v2/parse`), and deprecated extract compatibility. Both route families are served by the same engine, but `/firecrawl/v2` should be treated as migration compatibility rather than the default API. See [v2 API Reference](/docs/v2-api) for the full route table and [Choose an Endpoint](/docs/choose-endpoint) for guidance on which route to start with.
