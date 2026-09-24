# JS Rendering

crw supports JavaScript rendering for single-page applications (SPAs) and JS-heavy sites via the Chrome DevTools Protocol (CDP).

## Rendering Modes

Set the rendering mode in your config:

```toml
[renderer]
mode = "auto"          # auto | lightpanda | playwright | chrome | none
page_timeout_ms = 30000
pool_size = 4
```

| Mode | Behavior |
|------|----------|
| `auto` | Use HTTP first, detect SPAs, fall back to CDP if needed |
| `lightpanda` | Always use LightPanda for JS rendering |
| `playwright` | Always use Playwright for JS rendering |
| `chrome` | Always use Chrome for JS rendering |
| `camoufox` | Opt-in stealth tier (REST). Requires `--features camoufox` + a configured `[renderer.camoufox]` endpoint |
| `none` | HTTP only, never render JS |

## Per-request control

Override the rendering mode per request using `renderJs`:

```json
{
  "url": "https://spa-app.com",
  "renderJs": true,
  "waitFor": 3000
}
```

| Value | Behavior |
|-------|----------|
| `null` (default) | Auto-detect based on heuristics, or fall back to the global `render_js_default` if set |
| `true` | Force CDP rendering |
| `false` | HTTP only |

:::note
500 credits to start, up to 500 more from onboarding tasks, no card.
<a href="https://fastcrw.com/register?ref=docs-js-rendering-inline" target="_blank" rel="noopener">Get your API key</a>
:::

## Global default

To force JS rendering for every request that doesn't specify `renderJs` explicitly, set `render_js_default` in your config:

```toml
[renderer]
mode = "chrome"
render_js_default = true   # alias: force_js = true
```

```bash
# Or via environment variables
CRW_RENDERER__MODE=chrome
CRW_RENDERER__RENDER_JS_DEFAULT=true
# Backward-compat alias:
CRW_RENDERER__FORCE_JS=true
```

Precedence: a per-request `renderJs` always wins over the global default. Unset (`null`) on the request falls back to the default; if the default is also unset, the auto-detection heuristics below apply.

## Per-request renderer override

When `mode = "auto"` and you have multiple renderers configured (e.g., LightPanda + Chrome), the auto-detect chain decides which one to use. Sometimes you already know that a specific site needs Chrome (Cloudflare-protected SPAs) or LightPanda (fast static-JS sites). Pin the renderer per request with the `renderer` field:

```json
{
  "url": "https://x.com/elonmusk",
  "renderer": "chrome"
}
```

| Value | Behavior |
|-------|----------|
| omitted / `auto` | Use the configured fallback chain (existing behavior) |
| `lightpanda` | Hard-pin to LightPanda — no fallback |
| `chrome` | Hard-pin to Chrome — no fallback |
| `chrome_proxy` | Hard-pin to residential-proxy Chrome tier — no fallback |
| `playwright` | Hard-pin to Playwright — no fallback |
| `camoufox` | Hard-pin to the opt-in Camoufox stealth tier (REST) — no fallback. Requires a build with `--features camoufox` and a configured `[renderer.camoufox]` endpoint |
| `impersonated-http` | Hard-pin to the Chrome-impersonating HTTP tier (real Chrome TLS/JA3/HTTP2 fingerprint via wreq, no JS engine), no fallback. Requires a build with `--features impersonated` (`GET /v1/capabilities` lists it when present); a wall-shaped result surfaces as an error rather than a billed success, while ordinary non-wall answers (a 404, a PDF, a thin page) come back as results. See [TLS-fingerprint walls](/docs/tls-fingerprint-walls) for which site classes this helps with and how to tell a fingerprint wall from a JS gate or an IP-rate block |

### Pinned implies JS

A non-`auto` browser `renderer` value implies `renderJs:true`. The one exception is `impersonated-http`: it never executes JS, so it implies nothing, and an explicit `renderJs:true` alongside it is rejected as contradictory.

For browser pins, if you set `renderJs:false` explicitly, the request stays HTTP-only and the pin is silently ignored; `renderJs:false` always wins. This means the availability check is also skipped when `renderJs:false` is set with a browser pin, so combinations like `{"mode":"none","renderJs":false,"renderer":"chrome"}` are accepted. An `impersonated-http` pin is validated regardless of `renderJs`, because it selects a transport rather than a rendering mode.

### Errors and validation

If the named renderer isn't available in the server's pool, the request returns HTTP 400 immediately with `error_code: "invalid_request"` and a message listing the configured renderers:

```json
{
  "success": false,
  "error": "renderer 'chrome' not available; configured renderers: [lightpanda]. Update server config or omit the 'renderer' field.",
  "error_code": "invalid_request"
}
```

For `/v1/crawl`, this validation runs **once at job acceptance** — bad combinations return 400 before the job is queued.

### Pinning reduces resilience

Hard-pinning a renderer means transient failures of that renderer surface as errors instead of silently falling back to HTTP. If you need maximum resilience, omit `renderer` (or set it to `auto`) so the auto-detect chain can fall back. If you need determinism — "I know this site needs Chrome and a LightPanda success would be a wrong answer" — pin it.

For crawls, per-page failures of a pinned renderer are still logged and skipped; the rest of the crawl continues. Pages that fail will not appear in the results.

## Per-request country (residential proxy tier)

When the operator configures a `chrome_proxy` tier backed by a residential proxy provider you configure, each request can route through a specific country by setting `country` in the request body:

```json
{
  "url": "https://geo-restricted.example.com",
  "country": "us"
}
```

| Value | Behavior |
|-------|----------|
| omitted | Uses `CRW_RENDERER__PROXY_DEFAULT_COUNTRY` if configured, otherwise the provider's global pool |
| 2-letter code (`us`, `gb`, `de`, `fr`, `jp`, …) | Lowercased and appended to the base proxy username as a country selector |
| Invalid (non-alpha, wrong length) | Silently ignored — falls back to the default country, then the global pool |

The country selector is composed at request time and supplied to Chrome via the CDP `Fetch.authRequired` event — no per-country container restart needed, and concurrent requests can target different countries without cross-contamination.

This field has no effect when the `chrome_proxy` tier is not configured. It is a renderer-tier routing hint, not a content filter — the engine does not translate or localize the response.

See [Configuration — Residential proxy tier](#configuration) for the operator-side env vars (`CRW_RENDERER__PROXY_BASE_USER`, `CRW_RENDERER__PROXY_BASE_PASS`, `CRW_RENDERER__PROXY_DEFAULT_COUNTRY`).

## When To Turn It On

Enable JS rendering when the page content is not present in the initial HTML response. Typical examples:

- single-page applications,
- pages that fetch content after hydration,
- and sites where the meaningful body is assembled client-side.

Do not enable it blindly for every request. HTTP-only fetches are faster and cheaper.

## Choosing a `waitFor` Value

Start with the smallest value that works:

- `500` to `1000` for lightly hydrated pages,
- `2000` for typical JS-heavy pages,
- `3000` to `5000` only when you have confirmed the target hydrates slowly.

:::warning
Long waits are not automatically safer. They increase latency and can hide the fact that the page is blocked rather than merely slow.
:::

## Auto-detection

When `renderJs` is `null`, crw fetches the page via HTTP first, then checks for SPA signals:

**Triggered when body text < 200 chars and:**
- Contains `id="root"`, `id="app"`, `id="__next"`, `id="__nuxt"`, `id="__gatsby"`, `id="svelte"`
- Contains `ng-app`, `data-reactroot`
- Contains `window.__initial_state__`, `__next_data__`, `window.__remixcontext`, `window.__astro`

**Triggered when:**
- `<noscript>` contains "enable javascript"
- Body text < 500 chars and URL matches Framer, Webflow, Wix, or Squarespace domains

## CDP Backends

### LightPanda (Recommended)

Fastest option. Lightweight browser engine purpose-built for scraping.

```bash
# Auto-install
crw-server setup

# Manual start
lightpanda serve --host 127.0.0.1 --port 9222 --block-private-networks &
```

```toml
[renderer.lightpanda]
ws_url = "ws://127.0.0.1:9222/"
```

### Playwright

```toml
[renderer.playwright]
ws_url = "ws://playwright:9222"
```

### Chrome / Chromium

```toml
[renderer.chrome]
ws_url = "ws://chrome:9222"
```

## How CDP rendering works

1. Open a new browser tab via `Target.createTarget`
2. Navigate to the URL
3. Attach to the target via `Target.attachToTarget`
4. Wait for the configured time (`waitFor` or default 2000ms)
5. Execute `Runtime.evaluate("document.documentElement.outerHTML")` to get rendered HTML
6. Close the tab via `Target.closeTarget`

## Camoufox stealth tier (opt-in)

Camoufox is an **optional** stealth renderer for targets that block the CDP
tiers on **fingerprint / bot-challenge** grounds (e.g. a Cloudflare `403` that
Chrome can't get past). It is a [Camoufox](https://github.com/daijro/camoufox)
(Firefox-based, anti-fingerprint) browser driven through the
[`camofox-browser`](https://github.com/jo-inc/camofox-browser) REST sidecar —
**not** CDP.

It is **off by default and changes nothing** unless you opt in. Even a
configured endpoint stays out of the `auto` failover chain until you ask for it.

### Requirements

1. A build with the feature compiled in: `--features camoufox` (the default
   build does not include it).
2. A running camofox-browser sidecar (default port `9377`):

   ```bash
   docker run -p 9377:9377 -e CAMOFOX_PORT=9377 jo-inc/camofox-browser
   ```

3. The endpoint in your config:

   ```toml
   [renderer.camoufox]
   base_url = "http://127.0.0.1:9377"
   # api_key = "..."             # sent as `Authorization: Bearer` — only needed
   #                             # if you front the sidecar with an auth proxy
   # include_in_auto = false     # default: stay OUT of the auto ladder
   ```

   The two camoufox budgets go in your existing `[renderer]` table (the one
   that holds `mode`), not in `[renderer.camoufox]` and not in a second
   `[renderer]` header. Under `[renderer.camoufox]` they are silently ignored
   and the defaults stay in force.

   ```toml
   [renderer]
   mode = "auto"
   # camoufox_timeout_ms = 60000 # per-request REST budget (default 60s)
   # camoufox_challenge_wait_ms = 20000
   #                             # ceiling for polling a tab while a Cloudflare-style
   #                             # JS challenge clears itself (default 20s; 0 disables
   #                             # the poll and reports the interstitial immediately)
   ```

### Three ways to use it

| How | What it does |
|-----|--------------|
| Per-request `"renderer": "camoufox"` | Hard-pin a single request to Camoufox. Works as soon as the endpoint is configured. |
| Config `mode = "camoufox"` | Pin **every** request to Camoufox. |
| `include_in_auto = true` | Add Camoufox as the **last** tier of the `auto` failover chain (tried only after the cheaper tiers fail). |

With the default `include_in_auto = false`, a configured endpoint is **only**
reachable via an explicit pin (`renderer = "camoufox"`) or `mode = "camoufox"` —
it never silently joins `auto`.

### How it works

The renderer talks to the sidecar over HTTP, not CDP:

1. `POST /tabs` — create a fresh, isolated session and navigate to the URL.
2. `POST /tabs/{id}/evaluate` — run `document.documentElement.outerHTML` to get
   the fully JS-rendered DOM.
3. `DELETE /sessions/{id}` — always tear the session down (even on error), so
   no server-side session leaks.

The returned HTML flows through the **same** post-render pipeline as every other
tier (`only_main_content`, tag filters, markdown), so your output shape is
unchanged. Each request gets a fresh session (no cookie carry-over). If the page
comes back as a bot wall / challenge, the tier reports a retryable error so the
chain can react.

### Trade-offs

- **Cost:** a full Firefox-class render plus a REST round-trip — heavier and
  slower than the CDP tiers, and it counts as the most expensive renderer
  internally. You only pay this on the requests you opt in.
- **Ops:** it's a separate (Node) process to run and monitor — it breaks the
  pure single-binary story for deployments that enable it.
- **Resilience:** the `Fetch`-based resource blocking the CDP path uses does not
  apply on this tier (the sidecar loads the page itself).

If the sidecar is unreachable, the tier reports unavailable (health-probed) and
is skipped rather than failing every request.

## Cloud vs Self-hosted

- **Cloud**: JS rendering is always available. The managed infrastructure runs a LightPanda sidecar alongside the engine. Available on [fastcrw.com](https://fastcrw.com) (cloud).
- **Self-hosted**: You must run `crw-server setup` or configure a CDP browser (LightPanda, Chrome, or Playwright) in your `config.toml` under `[renderer]`. If no JS renderer is configured, requests with `renderJs: true` will fall back to HTTP-only fetching and include a warning.

## What To Inspect in the Response

When rendered output looks wrong, check:

- `metadata.renderedWith` to verify a browser was actually used,
- `metadata.elapsedMs` to understand the cost of the request,
- and `warning` to catch anti-bot or fallback situations.

## Troubleshooting

- **Empty content from JS-heavy sites**: Increase `waitFor` (e.g., `3000`-`5000`). Some SPAs need extra time to hydrate.
- **`renderedWith: "http_only_fallback"` in metadata**: JS rendering was requested but no renderer is available. Check your deployment configuration.
- **`renderedWith: "http"` plus a `js_escalation_failed:` warning**: a renderer was configured and attempted, but every tier failed. The HTTP body is returned instead of an error so the request still yields content. The usual cause is a `timeout` too short for a browser — a cold Chrome navigation can take 15s or more on a heavy page.
- **Internal error on `renderJs: true`**: Verify the LightPanda sidecar is running and reachable. Check `/health` for renderer status.
- **Still poor output after increasing `waitFor`**: The issue may be anti-bot protection or authentication flow, not rendering delay.

## Docker Compose

The included `docker-compose.yml` runs crw with a LightPanda sidecar:

```yaml
services:
  crw:
    image: ghcr.io/fastcrw/crw:latest
    ports:
      - "3000:3000"
    environment:
      - CRW_RENDERER__LIGHTPANDA__WS_URL=ws://lightpanda:9222
  lightpanda:
    image: lightpanda/lightpanda:latest
    command: ["serve", "--host", "0.0.0.0", "--port", "9222"]
```
