# Authentication

CRW keeps auth simple: use a Bearer token on the hosted API, and enable the same pattern on self-hosted deployments only when you need it.

## Two API Namespaces

fastCRW exposes two distinct base URLs with different scopes. Always use the correct one for your operation:

| Namespace | Base URL | What lives here |
|-----------|----------|-----------------|
| **Engine** | `https://api.fastcrw.com` | Scrape, crawl, map, search, extract, parse — all content-fetching endpoints (`/v1/*`, `/firecrawl/v2/*`) |
| **SaaS control-plane** | `https://fastcrw.com/api` | Account balance, monitor CRUD, billing — management operations tied to your account |
| **Hosted MCP** | `https://fastcrw.com/mcp` | The remote MCP server for claude.ai, ChatGPT and Claude Desktop: paste this URL (OAuth sign-in) or `https://fastcrw.com/mcp/<API_KEY>`. `api.fastcrw.com` does not serve MCP. A self-hosted engine serves `/mcp` itself. |

**Self-hosted deployments expose the engine namespace only.** There is no control-plane on a self-hosted instance; account/billing APIs are SaaS-exclusive.

Quick rule of thumb:
- Fetching content from the web → `https://api.fastcrw.com`
- Reading your balance or managing monitors → `https://fastcrw.com/api`
- Self-hosted → `http://localhost:3000` (engine routes only)

## Hosted API

Every authenticated request to the hosted API uses:

```http
Authorization: Bearer YOUR_API_KEY
```

Example:

```bash
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com","formats":["markdown"]}'
```

## Self-Hosted API

Self-hosted CRW does not require auth unless you configure API keys.

Set one or more keys in config:

```toml
[auth]
api_keys = ["crw_live_key-1234", "crw_live_key-5678"]
```

Once keys are configured, every route under `/v1/*`, `/firecrawl/v2/*`, and `/mcp` requires the same Bearer header.

## Behavior

- No keys configured: self-hosted API routes are open
- Keys configured: `/v1/*`, `/firecrawl/v2/*`, and `/mcp` require `Authorization: Bearer ...`
- `/health` always stays public

## Error Cases

```json
{
  "success": false,
  "error": "Missing Authorization header"
}
```

```json
{
  "success": false,
  "error": "Invalid API key"
}
```

## Common Mistakes

- Forgetting the `Bearer ` prefix
- Mixing a hosted key with a self-hosted deployment
- Turning on auth locally and forgetting that MCP over HTTP will also need the header

## What To Read Next

- [Quick Start](#quick-start) for the first authenticated request
- [API Playground](#playground) for interactive testing
- [Self-Hosting](#self-hosting) for private deployments
