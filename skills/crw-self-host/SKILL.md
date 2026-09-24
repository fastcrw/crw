---
name: crw-self-host
description: |
  Stand up your own fastCRW API server — single binary, Docker, or
  docker-compose with a bundled search-backend sidecar. Use when the user wants
  to run crw locally or on their own infra, configure renderers/proxies/
  auth/LLM extraction, or understand the embedded vs proxy MCP modes.
license: AGPL-3.0
metadata:
  author: us
  version: "0.3.0"
  homepage: https://fastcrw.com
  repository: https://github.com/fastcrw/crw
allowed-tools: Bash(crw:*) Bash(curl:*) Bash(docker:*) Bash(cargo:*) Read
---

# crw-self-host — Stand up your own crw

One static binary, one config file. No Redis, no Postgres, no Node.js runtime in
the request path. Self-host free under AGPL-3.0, or point at `api.fastcrw.com`
for managed.

## When to use

- You want full data residency (URLs and queries never leave your infra).
- You want to run recurring crawls/audits at VPS cost, not per-page credits.
- You need to configure proxy pools, custom renderers, or LLM extraction.
- Step 0 before any self-hosted crw workflow.

## Install paths

Choose one. All install paths run the same Rust binary.

### MCP server (`crw-mcp`) — recommended for AI agent use

```bash
npx crw-mcp                           # zero install (npm; embedded engine, ~14 MB RAM)
brew install fastcrw/crw/crw-mcp           # Homebrew
cargo install crw-mcp                 # Cargo (~17 MB, full embedded)
docker run -i ghcr.io/fastcrw/crw crw-mcp  # Docker
pip install crw                       # Python SDK (auto-downloads binary on first use)
```

**Lean build** (~4.2 MB, no headless browser engine — proxy/cloud-only mode):
```bash
cargo build --profile release-small --no-default-features -p crw-mcp
```

### CLI (`crw`) — scrape from the terminal

```bash
brew install fastcrw/crw/crw
curl -fsSL https://fastcrw.com/install | CRW_BINARY=crw sh
cargo install crw-cli

# APT (Debian/Ubuntu):
curl -fsSL https://apt.fastcrw.com/gpg.key | sudo gpg --dearmor -o /usr/share/keyrings/crw.gpg
echo "deb [signed-by=/usr/share/keyrings/crw.gpg] https://apt.fastcrw.com stable main" \
  | sudo tee /etc/apt/sources.list.d/crw.list
sudo apt update && sudo apt install crw
```

### API server (`crw-server`) — Firecrawl-compatible REST endpoint

For serving multiple apps, other languages (Node.js, Go, Java), or as a shared
microservice.

```bash
brew install fastcrw/crw/crw-server
curl -fsSL https://fastcrw.com/install | CRW_BINARY=crw-server sh
docker run -p 3000:3000 ghcr.io/fastcrw/crw
```

## Running the API server

**Binary directly (`crw serve`):**
```bash
crw serve                             # reads config.default.toml, listens on :3000
crw serve --port 3001                 # custom port (-p short form also accepted)
crw serve --config myconfig.toml      # load specific config file
CRW_PORT=3001 crw-server              # env-var alternative for the standalone binary
```

**Docker (single container, no search):**
```bash
docker run -p 3000:3000 ghcr.io/fastcrw/crw
curl http://localhost:3000/v1/scrape \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com"}'
```

**Docker Compose — full stack with bundled search:**
```bash
docker compose up -d                                          # http + LightPanda + search backend
docker compose --profile heavy up -d                          # + Chrome fallback
docker compose -f docker-compose.yml \
  -f docker-compose.stealth.yml --profile stealth up -d       # + browserless stealth tier
```

`docker compose up` starts three services: `crw` (API server), `lightpanda`
(JS renderer), and `searxng` (search backend). The `crw` service waits for
`searxng` to pass its healthcheck before accepting traffic, so the first search
request after startup doesn't race the cold start.

After `compose up`:
```bash
curl http://localhost:3000/health                              # → {"status":"ok",...}
curl -X POST http://localhost:3000/v1/search \
  -H "Content-Type: application/json" \
  -d '{"query":"fastCRW","limit":5}'                          # own backend, no API key
```

## Search backend sidecar

`docker-compose.yml` bundles a search-backend container (`searxng/searxng:2026.5.9-0cba32c15`)
that backs `/v1/search` with no API key and no per-query cost. It is internal-only —
the port is not published to the host by default. The `crw` container points at it as
`http://searxng:8080` via the Docker bridge network (set in `config.docker.toml`
as `[search] searxng_url = "http://searxng:8080"`).

To debug the search backend directly, add to `docker-compose.override.yml`:
```yaml
services:
  searxng:
    ports:
      - "127.0.0.1:8888:8080"
```

For a standalone binary pointing at an external search backend:
```bash
CRW_SEARCH__SEARXNG_URL=http://my-searxng:8080 crw-server
```

When `searxng_url` is unset, `/v1/search` returns HTTP 503 `search_disabled`.

## Key config knobs (`config.default.toml`)

Config is a TOML file. The binary loads `config.default.toml` by default; override
with `CRW_CONFIG=<name>` (loads `<name>.toml`). Docker uses `config.docker.toml`
(set via `CRW_CONFIG=config.docker` in `docker-compose.yml`).

Every key can be overridden by environment variable using the pattern
`CRW_<SECTION>__<KEY>` (double underscore between section and key):

```bash
CRW_SERVER__PORT=8080
CRW_RENDERER__MODE=chrome
CRW_RENDERER__POOL_SIZE=8
CRW_SEARCH__SEARXNG_URL=http://localhost:8080
CRW_CRAWLER__PROXY=http://user:pass@proxy:8080
CRW_EXTRACTION__LLM__API_KEY=sk-...
CRW_AUTH__API_KEYS='["key-one","key-two"]'   # JSON array as string
```

### `[renderer]` — JS rendering

```toml
[renderer]
mode = "auto"        # auto | lightpanda | chrome | playwright | none
pool_size = 4        # browser context pool size
page_timeout_ms = 30000
```

- `auto` — tries LightPanda first (fast, ~64 MB), falls back to Chrome for
  complex SPAs and Cloudflare challenges. Recommended for production.
- `lightpanda` — LightPanda only; fast p50, lower recall on heavy SPAs.
- `chrome` — Chromium only via CDP.
- `playwright` — Playwright-controlled browser.
- `none` — HTTP-only, no JS rendering.

Configure renderer endpoints:
```toml
[renderer.lightpanda]
ws_url = "ws://127.0.0.1:9222/"

[renderer.chrome]
ws_url = "ws://127.0.0.1:9223/"
```

### `[search]` — web search

```toml
[search]
enabled = true
# searxng_url = "http://localhost:8080"   # required for /v1/search to work
timeout_ms = 15000
default_limit = 5
max_limit = 20
```

### `[crawler]` — proxy and stealth

```toml
[crawler]
# Single proxy:
# proxy = "http://user:pass@proxy:8080"
# proxy = "socks5://user:pass@proxy:1080"

# Pool with rotation:
# proxy_list = ["http://user:pass@a:8080", "http://user:pass@b:8080"]
# proxy_rotation = "sticky_per_host"   # sticky_per_host | round_robin | random

# Stealth mode (rotate UA + inject browser-like headers):
# [crawler.stealth]
# enabled = true
# inject_headers = true
# jitter_factor = 0.2
```

Proxy rotation applies to both the HTTP path and the Chrome/CDP path for
scrape, crawl, and map. LightPanda has no proxy support and is skipped
(fail-closed) when a proxy is active. SOCKS5 proxies with credentials are not
supported on the Chrome path — use http/https proxies there.

### `[extraction.llm]` — structured JSON extraction + summaries

Required to enable `formats: ["json"]` (schema extraction), `formats: ["summary"]`,
and `/v1/search` with `answer: true`.

```toml
[extraction.llm]
provider = "anthropic"          # "anthropic" | "openai" | "openai-responses" | "deepseek" | "azure" | "openai-compatible"
api_key = "sk-..."              # or CRW_EXTRACTION__LLM__API_KEY env var
model = "claude-sonnet-4-20250514"
max_tokens = 4096
# base_url = "https://custom-endpoint.example.com"   # Chat Completions or Responses endpoints
max_concurrency = 4
max_html_bytes = 100000

# DeepSeek example:
# provider = "deepseek"
# api_key = "..."
# model = "deepseek-chat"
# base_url = "https://api.deepseek.com/v1"

# Azure OpenAI example:
# provider = "azure"
# api_key = "..."
# model = "gpt-4o-mini"                              # Azure deployment name
# base_url = "https://<resource>.openai.azure.com"
# azure_api_version = "2024-05-01-preview"
```

### `[auth]` — API key auth

By default, self-hosted crw requires no auth. Add keys to lock it down:

```toml
[auth]
api_keys = ["crw-key-one", "crw-key-two"]
```

Empty (or absent) `api_keys` = no auth required — good for local/trusted-network
deployments. On managed `api.fastcrw.com`, Bearer auth is always required.

### `[document]` — PDF parsing

```toml
[document]
enabled = true
max_pages = 0              # 0 = no limit
max_upload_bytes = 52428800   # 50 MiB cap for POST /v2/parse uploads
upload_concurrency = 4
max_concurrent_parses = 4
parse_timeout_ms = 30000
sandbox = false            # set true in Docker (untrusted uploads)
```

## MCP modes: embedded vs proxy

`crw-mcp` runs in one of two modes determined by the `CRW_API_URL` env var:

**Embedded mode** (no `CRW_API_URL`): the Rust engine runs in-process inside the
MCP process. No separate server needed. ~14 MB RAM. Zero setup.
```bash
npx crw-mcp                            # embedded
claude mcp add crw -- npx -y crw-mcp  # Claude Code, embedded
```

**Proxy mode** (`CRW_API_URL` set): MCP forwards all calls to the REST endpoint.
Use this when you want a shared `crw-server` to serve multiple agents, or to
point at `api.fastcrw.com`.
```bash
CRW_API_URL=https://api.fastcrw.com CRW_API_KEY=crw_live_... npx crw-mcp

# Claude Code:
claude mcp add crw \
  -e CRW_API_URL=https://api.fastcrw.com -e CRW_API_KEY=crw_live_... \
  -- npx -y crw-mcp
```

## Verify the setup

```bash
# Health (no auth)
curl http://localhost:3000/health
# → {"status":"ok","version":"..."}

# Scrape (no auth on default self-host)
curl -X POST http://localhost:3000/v1/scrape \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com","formats":["markdown"]}' | jq .success
# → true

# Search (requires a search-backend sidecar via docker compose up)
curl -X POST http://localhost:3000/v1/search \
  -H "Content-Type: application/json" \
  -d '{"query":"hello","limit":3}' | jq .success
# → true (or 503 search_disabled if search backend not wired up)
```

## Production hardening notes

- Set `SEARXNG_SECRET_KEY` via `.env` (`openssl rand -hex 32`) — the compose default
  is a placeholder acceptable only for local use.
- Enable `[document] sandbox = true` if you accept untrusted PDF uploads (Docker
  images do this by default in `config.docker.toml`).
- For public-facing deployments, set `[auth] api_keys` and put a reverse proxy
  (nginx/Caddy) in front for TLS.
- `rate_limit_rps = 10` in `config.default.toml` (global). Set to `0` to disable
  (Docker config does this for bench/production load).

Full production hardening guide: [docs.fastcrw.com/self-hosting-hardening/](https://docs.fastcrw.com/self-hosting-hardening/)

## See also

- [crw-migrate](../crw-migrate/SKILL.md) — coming from Firecrawl? one-line swap
- [crw](../crw/SKILL.md) — hub skill, verb ladder and output hygiene
- [crw-best-practices](../crw-best-practices/SKILL.md) — SDK patterns, proxy tuning, error handling
