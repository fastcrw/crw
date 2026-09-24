---
name: crw-migrate
description: |
  Coming from Firecrawl? Switch to fastCRW in one line. Use when the user
  has existing Firecrawl SDK code (firecrawl-py, firecrawl-js, REST calls,
  or an MCP config) and wants to point it at fastCRW — managed or
  self-hosted. Covers the exact base_url swap, which endpoints are
  drop-in, which have gaps, and how to verify the switch worked.
license: AGPL-3.0
metadata:
  author: us
  version: "0.3.0"
  homepage: https://fastcrw.com
  repository: https://github.com/fastcrw/crw
allowed-tools: Bash(crw:*) Bash(curl:*) Read
---

# crw-migrate — Coming from Firecrawl?

Switch in one line. The v2 API is a drop-in for the official `firecrawl-py` v4 SDK
and `firecrawl-js` — swap the base URL and keep your code.

## When to use

- You have existing Firecrawl SDK code or REST calls you want to repoint.
- You're replacing a `firecrawl-mcp-server` entry in your MCP config.
- You want to know exactly which Firecrawl endpoints are covered vs. which need adaptation.

## The one-line swap

### firecrawl-py v4 (Python SDK)

**Managed fastCRW:**
```python
from firecrawl import FirecrawlApp

app = FirecrawlApp(
    api_url="https://api.fastcrw.com",
    api_key="crw_live_..."
)
```

**Self-hosted fastCRW (default port 3000, no auth):**
```python
app = FirecrawlApp(
    api_url="http://localhost:3000",
    api_key="any"          # self-host ignores the key when auth is not configured
)
```

### firecrawl-js (TypeScript/Node SDK)

```ts
import FirecrawlApp from "@mendable/firecrawl-js";

const app = new FirecrawlApp({
  apiUrl: "https://api.fastcrw.com",   // or "http://localhost:3000"
  apiKey: "crw_live_...",
});
```

### REST / curl

Replace `https://api.firecrawl.dev` with `https://api.fastcrw.com` (or your
self-hosted `http://localhost:3000`). Auth header stays the same:
`Authorization: Bearer <key>`.

```bash
# Before
curl -X POST https://api.firecrawl.dev/v1/scrape \
  -H "Authorization: Bearer crw_live_..." \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com","formats":["markdown"]}'

# After — change exactly two things: host + key
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer crw_live_..." \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com","formats":["markdown"]}'
```

## Compatibility matrix

### Drop-in endpoints (no code change needed)

| Endpoint | Notes |
|---|---|
| `POST /v1/scrape` | Full markdown/html/links/json formats, `onlyMainContent`, `waitFor`, `renderJs`, `includeTags`/`excludeTags` |
| `POST /v1/crawl` + `GET /v1/crawl/:id` + `DELETE /v1/crawl/:id` | Async BFS crawl, polling shape matches |
| `POST /v1/map` | URL discovery |
| `POST /v1/search` | Own search backend instead of Fire-engine; same response shape |
| `POST /v2/scrape` | v2 surface with `parsers` field for PDFs |
| `POST /v2/crawl` + `GET /v2/crawl/active` | v2 crawl |
| `POST /v2/map` | v2 map |
| `POST /v2/search` | v2 search |
| `POST /v2/batch/scrape` | Batch scrape |
| `POST /v2/parse` | PDF → markdown (see gaps below) |

### Gaps — what needs adaptation

| Firecrawl feature | fastCRW equivalent | Action |
|---|---|---|
| `POST /v1/extract` (standalone LLM extraction route) | Supported. Async: returns a job id, poll `GET /v1/extract/{id}`. `POST /v1/scrape` with `formats: ["json"]` + `jsonSchema` also works inline for a single URL. | None, but switch to polling if you port the route as-is. |
| `POST /v1/extract` multi-URL batch | Supported. Pass `urls: [...]`; the cap is `limits.maxExtractUrls` on `GET /v1/capabilities` (default 50). | None. |
| `POST /v1/deep-research` | Not implemented — cloud-only Firecrawl feature. | No equivalent. |
| `POST /v1/agent` (Spark models) | Not implemented. | No equivalent. |
| `/v2/parse` — DOCX/XLSX/RTF/ODT | PDF only. fastCRW uses `pdf-inspector` (no OCR). | Keep Firecrawl for non-PDF docs, or convert to PDF first. |
| `parsers: [{mode: "ocr"}]` | Accepted for wire-compat; degrades to text-layer extraction with a `pdf_ocr_unsupported` warning (no OCR engine). | If OCR is required, keep Firecrawl. |
| MCP tool names `firecrawl_*` | fastCRW MCP uses `crw_*` (see below). | Update MCP config. |
| Fire-engine anti-bot | Not available. fastCRW uses LightPanda → Chrome stealth ladder. | For heavy Cloudflare sites, test coverage; consider proxy pool. |

### `jsonSchema` alias

Firecrawl's `/v1/extract` uses `extract.schema`. fastCRW's `/v1/scrape` accepts
both the `jsonSchema` top-level field **and** the `extract.schema` alias for
closer Firecrawl parity:

```json
{
  "url": "https://example.com",
  "formats": ["json"],
  "jsonSchema": {
    "type": "object",
    "properties": { "title": { "type": "string" } }
  }
}
```

Requires `[extraction.llm]` configured in `config.toml` (or
`CRW_EXTRACTION__LLM__API_KEY` env var). See [crw-self-host](../crw-self-host/SKILL.md).

## Switching your MCP config

Firecrawl MCP uses `firecrawl-mcp-server`; fastCRW's MCP server is `crw-mcp`.
Tool names change from `firecrawl_*` to `crw_*`:

| Firecrawl MCP tool | fastCRW MCP tool |
|---|---|
| `firecrawl_scrape` | `crw_scrape` |
| `firecrawl_crawl` | `crw_crawl` |
| `firecrawl_check_crawl_status` | `crw_check_crawl_status` |
| `firecrawl_map` | `crw_map` |
| `firecrawl_search` | `crw_search` |
| `firecrawl_extract` | `crw_scrape` with `formats=["json"]` + `jsonSchema` |
| — | `crw_parse_file` (PDF upload; no Firecrawl MCP equivalent) |

### Claude Code — replace in `~/.claude/claude_desktop_config.json` (or settings)

```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"],
      "env": {
        "CRW_API_URL": "https://api.fastcrw.com",
        "CRW_API_KEY": "crw_live_..."
      }
    }
  }
}
```

For self-hosted (no auth, no env needed):
```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"]
    }
  }
}
```

Embedded mode (`npx crw-mcp` with no `CRW_API_URL`) runs the engine in-process
— zero server to stand up, ~14 MB RAM.

## Verify the swap — checklist

Run these after pointing at fastCRW. Each should return `"success": true`:

```bash
# 1. Health check (no auth)
curl http://localhost:3000/health
# → {"status":"ok",...}

# 2. Basic scrape
curl -X POST "$CRW_API_URL/v1/scrape" \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com","formats":["markdown"]}' | jq .success
# → true

# 3. Map (URL discovery)
curl -X POST "$CRW_API_URL/v1/map" \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com"}' | jq '.data | length'
# → N (should be > 0)

# 4. Search (requires a search backend — managed always works; self-host needs sidecar)
curl -X POST "$CRW_API_URL/v1/search" \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"query":"fastCRW scraper","limit":3}' | jq .success
# → true

# 5. Structured extraction (requires [extraction.llm] configured)
curl -X POST "$CRW_API_URL/v1/scrape" \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url":"https://example.com",
    "formats":["json"],
    "jsonSchema":{"type":"object","properties":{"title":{"type":"string"}}}
  }' | jq '.data.json'
```

Compare the `data.markdown` / `data.metadata` shape from your existing
Firecrawl responses — the field names on the overlap surface (`title`,
`description`, `sourceURL`, `statusCode`) match. A few metadata sub-fields
diverge; inspect with `jq .data.metadata` if your code reads specific keys.

## See also

- [crw-self-host](../crw-self-host/SKILL.md) — stand up your own crw server + search backend
- [crw-best-practices](../crw-best-practices/SKILL.md) — SDK patterns, error handling, batching
- [crw](../crw/SKILL.md) — hub skill, full verb ladder
