# When Chrome Gets a 403: CRW's Opt-In Camoufox Stealth Tier

> Some sites block headless Chrome on its fingerprint, not its IP. CRW now has an optional Camoufox stealth renderer for exactly those targets — off by default, opt-in per request, and it never slows down the fast path.

**Published:** 2026-06-18  
**Canonical:** https://fastcrw.com/blog/camoufox-stealth-tier

---

## The problem: a clean IP that still gets blocked

CRW's renderer chain is built for speed. A request starts as plain HTTP, escalates to LightPanda (a lightweight browser engine) if the page needs JavaScript, and falls through to Chrome for the heavy SPAs. For IP-flagged targets there's an optional residential-proxy Chrome tier on top of that.

That chain handles the vast majority of the web. But every so often you hit a site that returns a `403` even though your IP is fine, your proxy is residential, and Chrome rendered the page perfectly in your own browser. The block isn't about *where* the request came from — it's about *what* the browser looks like. Modern anti-bot systems fingerprint the browser itself: navigator properties, screen metrics, WebGL and canvas readbacks, font lists, timing quirks. Headless Chrome has tells, and some WAFs key on them.

You can't proxy your way out of a fingerprint block. You need a browser that doesn't look like headless Chrome.

## Why not just swap the whole renderer?

The obvious move is "use a stealth browser for everything." We didn't do that, on purpose.

CRW's identity is *fast and small* — a single Rust binary, low RAM, HTTP-first. A full Firefox-class stealth browser is the opposite of that: it's a heavier launch, more memory per instance, and an extra process to run. Paying that cost on every request — including the 95%+ that a plain HTTP fetch or LightPanda already handles — would throw away the thing that makes CRW worth using.

So the design goal was narrower: **add stealth as a tool you reach for, without touching the fast path or changing anything for people who don't need it.**

## The design: opt-in, and it means it

The new tier is [Camoufox](https://github.com/daijro/camoufox) — a Firefox-based, anti-fingerprint browser — driven through the [`camofox-browser`](https://github.com/jo-inc/camofox-browser) REST sidecar. Three things keep it from leaking into anyone's setup:

1. **It's a compile-time feature.** The default build doesn't include it at all. You opt in with `--features camoufox`. A normal build is byte-for-byte unchanged.
2. **A configured endpoint does not join `auto`.** This is the part people expect to be a lie, and it isn't. Even after you point CRW at a running sidecar, it stays *out* of the automatic failover chain. You have to ask for it.
3. **REST, not CDP.** Camoufox doesn't expose the Chrome DevTools Protocol surface CRW uses for proxy auth and resource interception, so it's reached over a small HTTP API instead — it lives entirely outside the CDP code path.

There are exactly three ways to actually use it:

| How | What it does |
|-----|--------------|
| `"renderer": "camoufox"` on a request | Hard-pin a single request to the stealth tier. |
| `mode = "camoufox"` in config | Pin every request to it. |
| `include_in_auto = true` | Add it as the **last** tier of the `auto` chain — tried only after the cheaper renderers fail. |

If you set none of them, Camoufox never runs. That's the whole point.

## Turning it on

First, run the sidecar (default port `9377`):

```bash
docker run -p 9377:9377 -e CAMOFOX_PORT=9377 jo-inc/camofox-browser
```

Then point CRW at it in `config.toml`:

```toml
[renderer.camoufox]
base_url = "http://127.0.0.1:9377"
# api_key = "..."             # sent as `Authorization: Bearer` — only if you
#                             # put the sidecar behind your own auth proxy
# include_in_auto = false     # default: stays out of the auto chain
# camoufox_timeout_ms = 60000 # per-request REST budget (default 60s)
```

Build with the feature on, and pin it per request:

```bash
curl -X POST http://localhost:3000/v1/scrape \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com", "renderer": "camoufox"}'
```

If the sidecar isn't reachable, CRW health-probes it and skips the tier rather than failing every request.

## What happens under the hood

The renderer is a small REST client, not a CDP connection. Per request it:

1. `POST /tabs` — opens a fresh, isolated session and navigates to the URL.
2. `POST /tabs/{id}/evaluate` — runs `document.documentElement.outerHTML` to pull the fully JS-rendered DOM back as a string.
3. `DELETE /sessions/{id}` — always tears the session down, even on error, so nothing leaks server-side.

That HTML string then flows through the *same* post-render pipeline as every other tier — `onlyMainContent`, include/exclude tags, markdown conversion. Nothing downstream knows or cares that the bytes came from a stealth browser, so your output shape is identical. Each request gets its own session with no cookie carry-over, and if the page comes back as a challenge wall, the tier raises a retryable error so the chain can move on instead of handing you a "Just a moment..." page.

## The honest trade-offs

This tier is not free, which is exactly why it's opt-in:

- **It's slower and heavier.** A Firefox-class render plus a REST round-trip costs more than LightPanda or Chrome, and CRW prices it as the most expensive renderer internally. You only pay it on the requests you choose.
- **It's another process.** Running the sidecar breaks the clean single-binary story for deployments that enable it — that's a real operational cost to weigh.
- **You lose CDP resource blocking on this tier.** The sidecar loads the page itself, so the `Fetch`-based blocking the Chrome path uses doesn't apply here.

## When to reach for it

Keep `auto` as your default. Pin `camoufox` on the specific hosts where Chrome keeps coming back blocked on its fingerprint — and leave everything else on the fast path. That's the whole idea: stealth on demand, speed by default, and nothing changes for anyone who never turns it on.

CRW is open source. If you want to see how the tier slots into the renderer chain without disturbing the rest, the code lives in [`crates/crw-renderer`](https://github.com/fastcrw/crw/tree/main/crates/crw-renderer), and the docs are at [JS rendering → Camoufox](https://docs.fastcrw.com/#js-rendering).
