# How to Self-Host a Firecrawl-Like API with a Single Binary

> Run a Firecrawl-compatible scraping API on your own server in under 60 seconds using CRW's single Docker image.

**Published:** 2026-03-09  
**Updated:** 2026-03-09  
**Canonical:** https://fastcrw.com/blog/self-host-firecrawl-api

---

## Why Self-Host a Scraping API?

Three reasons developers self-host their scraping infrastructure:

1. **Cost control:** At scale, managed scraping APIs charge per request. Self-hosting with CRW has a fixed server cost and zero per-request fees.
2. **Data privacy:** URLs you scrape stay on your infrastructure. No third-party sees your data access patterns.
3. **Customization:** You control rate limits, auth, routing, and can integrate with your existing services.

The traditional barrier to self-hosting was complexity. Firecrawl's self-hosted version requires Node.js, Redis, Playwright, Chromium, and a compose file with multiple services. CRW removes that barrier entirely.

## Prerequisites

- A Linux server with Docker installed (any cloud provider, $5/month DigitalOcean works)
- Port 3000 open in your firewall (or any port you prefer)

## Deploy in 60 Seconds

```
docker run -d \
  --name crw \
  --restart unless-stopped \
  -p 3000:3000 \
  ghcr.io/fastcrw/crw:latest
```

That's it. CRW is now running. Test it:

```
curl -X POST http://your-server:3000/v1/scrape \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com", "formats": ["markdown"]}'
```

## Adding API Key Authentication

For production, add an API key to prevent unauthorized access:

```
docker run -d \
  --name crw \
  --restart unless-stopped \
  -p 3000:3000 \
  -e CRW_API_KEY=your-secret-key \
  ghcr.io/fastcrw/crw:latest
```

Now all requests require the `Authorization: Bearer your-secret-key` header. Requests without it return 401.

## Using with an Nginx Reverse Proxy

For HTTPS and a cleaner URL, put Nginx in front:

```
server {
    listen 443 ssl;
    server_name scraper.yourdomain.com;

    ssl_certificate /etc/letsencrypt/live/scraper.yourdomain.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/scraper.yourdomain.com/privkey.pem;

    location / {
        proxy_pass http://localhost:3000;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_read_timeout 120s;
    }
}
```

## Using docker-compose

For a more maintainable setup:

```
version: "3.8"
services:
  crw:
    image: ghcr.io/fastcrw/crw:latest
    restart: unless-stopped
    ports:
      - "3000:3000"
    environment:
      - CRW_API_KEY=${CRW_API_KEY}
    healthcheck:
      test: ["CMD", "wget", "-q", "--spider", "http://localhost:3000/health"]
      interval: 30s
      timeout: 10s
      retries: 3
```

## Migrate from Firecrawl in One Line

If you're already using Firecrawl, changing your base URL is the entire migration:

```
// Before: Firecrawl cloud
const client = new FirecrawlApp({
  apiKey: "crw_live_...",
  apiUrl: "https://api.firecrawl.dev",
});

// After: Your self-hosted CRW
const client = new FirecrawlApp({
  apiKey: "your-crw-api-key",
  apiUrl: "https://scraper.yourdomain.com",
});
```

Same SDK, same method calls, same response format. Zero code changes beyond the URL.

## What CRW Handles Out of the Box

- `POST /v1/scrape` — Single page to markdown, HTML, or JSON
- `POST /v1/crawl` — Multi-page site crawl with depth/limit controls
- `GET /v1/crawl/:id` — Crawl status polling
- `POST /v1/map` — Site URL discovery
- `GET /health` — Health check endpoint

JavaScript rendering for SPAs is available via CRW's LightPanda integration — no separate browser configuration required.

## Resource Requirements

| Scenario | RAM | CPU | Recommended VPS |
| --- | --- | --- | --- |
| Light usage (100 req/min) | ~500 MB | High | $24/mo+ (4 vCPU, 4 GB) |

These are substantially smaller requirements than Firecrawl's self-hosted stack, which recommends 4 GB minimum for stable operation.

## Monitoring and Logs

```
# View live logs
docker logs -f crw

# Check resource usage
docker stats crw
```

CRW logs each request with method, URL, status code, and latency. Structured JSON logging is available with the `--log-format json` flag.

## When to Use fastCRW Instead

Self-hosting is ideal when you want full control and predictable costs. But if you need:

- Managed proxy rotation for blocked sites
- Auto-scaling for spiky traffic
- Zero maintenance overhead

…then [fastCRW](https://fastcrw.com) — the hosted version — is the better choice. You get the same API, same response format, and 1000 free credits to start without a credit card.
