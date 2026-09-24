# Self-Hosting

Use self-hosting when you want CRW running inside your own infrastructure. The hosted API is still the fastest first-run path; self-hosting is the better fit when network control, auth, and deployment ownership matter more than zero-setup speed.

## Quick Start

The shortest self-hosted path is Docker:

```bash
docker run -p 3000:3000 ghcr.io/fastcrw/crw
```

Then make a local request:

```bash
curl -X POST http://localhost:3000/v1/scrape \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com","formats":["markdown"]}'
```

:::note
500 credits to start, up to 500 more from onboarding tasks, no card.
<a href="https://fastcrw.com/register?ref=docs-self-hosting-inline" target="_blank" rel="noopener">Get your API key</a>
:::

## What You Get

- the same core self-hosted routes: `scrape`, `crawl`, `map`, `mcp`, `health`
- `search` too, when you run the Docker stack — it boots a search sidecar so `/v1/search` and the `crw_search` MCP tool work out of the box (see [Docker → Search backend](/docker))
- optional auth with Bearer tokens
- optional browser-backed rendering
- your own reverse proxy, logging, rate limits, and deployment choices

## Deployment Paths Compared

Three deployment paths cover most self-hosted use cases. Choose based on how much JS rendering you need and how much you want to operate.

| | Single binary (`config.default`) | Docker Compose (`config.docker`) | Docker Compose stealth (`docker-compose.stealth.yml`) |
|---|---|---|---|
| **Setup effort** | Lowest — one binary, one TOML | Medium — `docker compose up -d`, two or three sidecars | Highest — compose + stealth override file, token rotation |
| **JS rendering included** | No — LightPanda must be started separately; Chrome disabled by default | Yes — LightPanda bundled; Chrome via `--profile heavy` | Yes — browserless/chromium with anti-fingerprint plugin (`--profile stealth`), +2.5 pt success over vanilla Chrome |
| **Search (`/v1/search`) included** | No — `search_backend_url` not set; returns `search_disabled` until you point it at a backend you run | Yes — search sidecar auto-started; `/v1/search` and `crw_search` MCP tool work out of the box | Yes — same as `config.docker` |
| **Rate limit default** | 10 req/s global | Unlimited (0) — per-host limits still apply | Unlimited (0) |
| **Best for** | Local dev, CI, minimal VPS where you control sidecars yourself | Standard production VPS or server | Production workloads on bot-protected targets; requires SSPL-3.0 license review before exposing to third parties |

> **Chrome vs stealth:** The default `--profile heavy` uses `chromedp/headless-shell` (Apache-2/BSD, no license concerns). The `--profile stealth` uses `browserless/chromium` (SSPL-3.0) and raised success rate from 87.1 % to 89.6 % on a 1 000-URL Firecrawl benchmark. Check the license notice in `docker-compose.yml` before using the stealth profile in any service that exposes scraping to third parties.

## Recommended Rollout

1. start with `scrape` on a real target
2. validate one `map` request
3. run a bounded `crawl` with a low page cap
4. add auth, TLS, and edge controls before broader access
5. add JS rendering only when targets actually need it

## Optional: Camoufox stealth sidecar

For targets that block the CDP renderers on fingerprint / bot-challenge grounds
(e.g. a Cloudflare `403`), you can run an optional [Camoufox](https://github.com/daijro/camoufox)
stealth tier. It is a REST sidecar — separate from the CDP browsers — and is
**off by default**: it requires a `--features camoufox` build and never joins
the `auto` chain unless you opt in.

```bash
# 1. run the camofox-browser sidecar (default port 9377)
docker run -p 9377:9377 -e CAMOFOX_PORT=9377 jo-inc/camofox-browser

# 2. point CRW at it (config.toml)
#    [renderer.camoufox]
#    base_url = "http://127.0.0.1:9377"
#    # include_in_auto = true   # optional: add as the last auto-failover tier
```

Reach it per request with `"renderer": "camoufox"`, pin every request with
`mode = "camoufox"`, or set `include_in_auto = true`. It is a heavier
(Firefox-class) render than the CDP tiers, so enable it only for the requests
that need it. Full details: [JS rendering → Camoufox](#js-rendering).

## When Self-Hosting Is The Right Choice

Choose self-hosting when you want:

- private network placement,
- direct control over operational cost,
- custom auth and routing,
- or a local MCP/server flow without depending on the hosted product.

## When The Hosted API Is Better

Choose the hosted path when:

- you want your first successful request immediately,
- you do not want to manage renderer dependencies,
- or your priority is product velocity rather than infrastructure ownership.

## Common Mistakes

- Starting with browser rendering before validating plain HTTP scraping
- Exposing the service publicly before adding auth, TLS, and rate limits
- Treating one happy-path scrape as enough deployment validation

## What To Read Next

- [Installation](#installation)
- [Configuration](#configuration)
- [Docker](#docker)
- [Self-Hosting Hardening](#self-hosting-hardening)
