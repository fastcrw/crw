# CRW v0.0.11: Stealth Anti-Bot Bypass, Chrome Failover, and Cloudflare Challenge Retry

> CRW v0.0.11 adds automatic stealth JavaScript injection to bypass bot detection, Chrome as a fallback renderer for complex SPAs, Cloudflare challenge auto-retry, and HTTP-to-CDP auto-escalation.

**Published:** 2026-04-22  
**Updated:** 2026-04-22  
**Canonical:** https://fastcrw.com/blog/crw-v0-0-11-release

---

CRW v0.0.11 tackles the hardest problem in web scraping: bot detection. This release adds stealth JavaScript injection that spoofs browser fingerprints, automatic Chrome failover for pages that crash LightPanda, Cloudflare challenge auto-retry, and HTTP-to-CDP auto-escalation when anti-bot signatures are detected in HTTP responses.

## Stealth Anti-Bot Bypass

Modern bot detection doesn't just check HTTP headers — it runs JavaScript fingerprinting in the browser. The detection scripts check properties that real browsers have but headless browsers don't: `navigator.webdriver`, Chrome runtime objects, plugin arrays, permission APIs, and iframe contentWindow behavior.

CRW v0.0.11 injects a stealth script via `Page.addScriptToEvaluateOnNewDocument` before every CDP navigation. This script runs before any page JavaScript executes, patching the browser environment to look like a real Chrome session:

### What Gets Spoofed

- **`navigator.webdriver`** — set to `undefined` instead of `true`. This is the most basic headless detection check and catches the majority of simple bot detectors.
- **Chrome runtime object** — `window.chrome` and `window.chrome.runtime` are injected with the properties that real Chrome exposes. Missing `window.chrome` is a strong signal for headless detection.
- **Plugins array** — headless browsers report zero plugins. The stealth script adds a realistic plugins array matching Chrome's default plugin set (Chrome PDF Plugin, Chrome PDF Viewer, Native Client).
- **Languages** — `navigator.languages` is set to `["en-US", "en"]` instead of the empty array that headless browsers report.
- **Permissions API** — `navigator.permissions.query` is patched to return realistic responses for permission checks instead of throwing or returning inconsistent states.
- **Iframe contentWindow** — cross-origin iframe `contentWindow` access is patched to match real browser behavior. Some detection scripts create iframes and check whether the contentWindow properties match the parent window.
- **`toString()` proxy** — native function `toString()` calls on patched functions return `"function () { [native code] }"` instead of revealing the proxy wrapper. This catches detection scripts that check whether browser APIs have been tampered with.

This stealth layer is effective against Cloudflare, PerimeterX, and similar JavaScript-based detection systems. It won't bypass advanced CAPTCHA challenges or device fingerprinting that checks WebGL rendering or canvas hashing — those require more specialized techniques.

## Cloudflare Challenge Auto-Retry

Cloudflare's "Just a moment..." interstitial is one of the most common blocks scrapers hit. Some of these are non-interactive JavaScript challenges — the browser runs a computation, Cloudflare verifies it, and the page loads. These challenges are solvable by a real browser; they just need time.

CRW v0.0.11 detects Cloudflare challenge pages after navigation and automatically retries:

1. After page load, CRW checks for Cloudflare challenge signatures: "Just a moment" text, `cf-browser-verification` element, `challenge-platform` scripts
2. If a challenge is detected, CRW waits 3 seconds and re-checks the page content
3. This repeats up to 3 times (9 seconds total wait)
4. If the challenge resolves, CRW extracts the real page content. If it doesn't resolve after 3 attempts, CRW returns what it has with a warning.

For non-interactive Cloudflare challenges, this works reliably. Interactive challenges (CAPTCHAs, Turnstile with explicit mode) still require human intervention or specialized solving services.

## HTTP to CDP Auto-Escalation

CRW's `auto` rendering mode already decides whether to use HTTP or JavaScript rendering based on heuristics. v0.0.11 adds a new heuristic: anti-bot detection in HTTP responses.

When CRW fetches a page via HTTP and the response contains anti-bot challenge signatures (Cloudflare challenge HTML, DataDome verification, PerimeterX block page), it automatically escalates to CDP rendering instead of returning the challenge HTML as content.

This means you don't need to set `renderJs: true` for sites that might be protected — CRW will detect the block and escalate automatically. The response metadata includes `rendered_with` so you know which renderer was used.

## Chrome Failover

LightPanda is fast and lightweight, but it doesn't support every site. Complex React SPAs, Angular applications, and pages with heavy Web API usage can cause LightPanda to crash or return incomplete content.

v0.0.11 adds Chrome as a failover renderer. The rendering chain is now:

1. **HTTP** — try a plain HTTP fetch first (fastest, lowest resource usage)
2. **LightPanda** — if JavaScript rendering is needed, try LightPanda (fast, low memory)
3. **Chrome** — if LightPanda fails, fall back to full Chrome via `chromedp/headless-shell` (slower, but handles everything)

### Docker Setup

Chrome runs as a sidecar container in Docker Compose:

```
services:
  crw:
    image: ghcr.io/fastcrw/crw:0.0.11
    ports:
      - "3000:3000"
    environment:
      - CHROME_WS_URL=ws://chrome:9222

  chrome:
    image: chromedp/headless-shell:latest
    shm_size: 2gb
    ports:
      - "9222:9222"
```

CRW auto-discovers Chrome's WebSocket URL via the `/json/version` endpoint. The connection is lazy-initialized — Chrome is only used when LightPanda can't handle a page. The WebSocket URL is resolved once and cached via `OnceCell`.

The `shm_size: 2gb` is important — Chrome uses shared memory for rendering, and the default 64 MB limit causes crashes on pages with many tabs or heavy JavaScript.

## When to Use What

| Scenario | Renderer | Speed |
| --- | --- | --- |
| Static HTML, documentation, blogs | HTTP | ~200 ms |
| Simple SPAs, light JavaScript | LightPanda | ~800 ms |
| Complex React/Angular, heavy Web APIs | Chrome | ~2,000 ms |
| Cloudflare-protected pages | Chrome + stealth + retry | ~5,000 ms |

In `auto` mode (the default), CRW picks the right renderer automatically. You don't need to configure anything — just set up the Docker Compose stack and CRW handles escalation.

## Upgrade

```
# Docker Compose (recommended for Chrome failover)
docker compose pull
docker compose up -d

# Docker (without Chrome failover)
docker pull ghcr.io/fastcrw/crw:0.0.11

# Cargo
cargo install crw-server
```

If you're not using Docker Compose, Chrome failover won't be available — CRW will use HTTP and LightPanda only. Stealth injection and Cloudflare retry work with any CDP renderer (LightPanda or Chrome).

## What's Next

v0.0.11 significantly improves CRW's ability to handle protected sites. The next focus areas are screenshot support (returning rendered page images alongside markdown) and PDF/DOCX parsing for document extraction workflows. Follow the [GitHub repository](https://github.com/fastcrw/crw) for updates.
