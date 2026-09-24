# $5 VPS Web Scraping: Run CRW Where Firecrawl Can't

> Deploy a full Firecrawl-compatible scraping API on a $5/month VPS with 512 MB RAM. CRW's tiny single-binary memory footprint makes it possible — here's the complete guide.

**Published:** 2026-04-29  
**Updated:** 2026-04-29  
**Canonical:** https://fastcrw.com/blog/crw-on-5-dollar-vps

---

## The $5 VPS Challenge

The cheapest VPS plans from providers like Hetzner, DigitalOcean, and Vultr give you 512 MB to 1 GB of RAM for about $4–6/month. That's enough to run a blog, a small API, or a static site. But self-hosted web scraping? Most people assume you need much more.

Firecrawl, the most popular open-source scraping API, requires Node.js, Redis, PostgreSQL, and RabbitMQ — a stack that consumes 500 MB+ of RAM at idle. Before you scrape a single page, you've already maxed out a $5 VPS. Their own documentation recommends 4–8 GB of RAM.

CRW is different. It's a single Rust binary with a tiny idle footprint — many CRW instances fit in the memory Firecrawl needs for one. This means a $5 VPS isn't just viable — it's comfortable.

In this guide, we'll set up a complete Firecrawl-compatible scraping API on the cheapest possible server, including JS rendering, HTTPS, and authentication.

## Why Memory Matters for Self-Hosted Scraping

When you're paying for cloud servers, RAM is usually the bottleneck and the primary cost driver. Here's what different scraping stacks consume at idle — before processing any requests:

| Stack | Idle RAM | Min VPS | Monthly Cost |
| --- | --- | --- | --- |
| Firecrawl (full stack) | ~500 MB+ | 4 GB | $24–48/mo |
| Crawl4AI + Playwright | ~200 MB+ | 2 GB | $12–24/mo |
| Scrapy + Splash | ~150 MB | 1 GB | $6–12/mo |
| **CRW** | **Tiny (single binary)** | **512 MB** | **$4–6/mo** |

The math is simple: if your scraper uses less memory at idle, you can use a smaller server, which costs less money. Over a year, the difference between a $6/mo and a $48/mo server is $504 — and that's before you factor in the mental overhead of managing a multi-container stack.

## Step 1: Provision the Server

Pick any VPS provider. Here are solid options at the $5 price point:

| Provider | Plan | RAM | Price |
| --- | --- | --- | --- |
| Hetzner (CX22) | Shared | 2 GB | €3.29/mo |
| DigitalOcean | Basic | 512 MB | $4/mo |
| Vultr | Cloud Compute | 512 MB | $2.50/mo |
| Linode (Akamai) | Nanode | 1 GB | $5/mo |
| Oracle Cloud | Always Free | 1 GB | $0/mo |

For this guide, we'll use Ubuntu 24.04 LTS. Any Linux distribution works — CRW has no OS-specific dependencies.

SSH into your fresh server:

```
ssh root@your-server-ip
```

## Step 2: Install CRW

CRW is distributed as a single binary. You have three installation options:

### Option A: Pre-built binary (fastest)

Download the latest release directly:

```
# Download the latest release
curl -L https://github.com/fastcrw/crw/releases/latest/download/crw-server-x86_64-unknown-linux-gnu -o /usr/local/bin/crw-server
chmod +x /usr/local/bin/crw-server

# Verify it works
crw-server --version
```

### Option B: Install via Cargo

```
# Install Rust if needed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install CRW
cargo install crw-server
```

### Option C: Docker (single container)

```
docker run -d --name crw -p 3000:3000 --restart unless-stopped ghcr.io/fastcrw/crw:latest
```

Even with Docker overhead, CRW uses under 30 MB of RAM — well within budget.

## Step 3: Configure Authentication

If your server is internet-facing, you must enable API key authentication. Without it, anyone can use your server to scrape the web.

Create a configuration file:

```
mkdir -p /etc/crw
cat > /etc/crw/config.local.toml << 'EOF'
[server]
host = "0.0.0.0"
port = 3000

[auth]
api_keys = ["your-secret-api-key-here"]
EOF
```

Generate a secure key:

```
openssl rand -hex 32
```

Now start CRW with the config:

```
CRW_CONFIG=/etc/crw/config.local.toml crw-server
```

## Step 4: Set Up as a systemd Service

For production, run CRW as a systemd service so it auto-restarts on crash and starts on boot:

```
cat > /etc/systemd/system/crw.service << 'EOF'
[Unit]
Description=CRW Web Scraping API
After=network.target

[Service]
Type=simple
User=nobody
Environment=CRW_CONFIG=/etc/crw/config.local.toml
ExecStart=/usr/local/bin/crw-server
Restart=always
RestartSec=5
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable crw
systemctl start crw
```

Check it's running:

```
curl http://localhost:3000/health
# {"status":"ok"}
```

## Step 5: Add HTTPS with Caddy

You need HTTPS for production use. Caddy is the easiest reverse proxy — it handles TLS certificates automatically:

```
# Install Caddy
apt install -y caddy

# Configure
cat > /etc/caddy/Caddyfile << 'EOF'
scraper.yourdomain.com {
    reverse_proxy localhost:3000
}
EOF

systemctl restart caddy
```

Point your domain's DNS A record to your server IP, and Caddy will automatically provision a Let's Encrypt certificate. Your scraping API is now available at `https://scraper.yourdomain.com`.

## Step 6: Enable JS Rendering (Optional)

For static websites and most content pages, CRW's HTTP-based scraping works perfectly. But for single-page applications (React, Vue, Angular), you need a JavaScript rendering engine.

CRW supports LightPanda, a lightweight headless browser that uses about 50 MB of RAM — still well within budget on a 512 MB VPS.

```
# Download and install LightPanda
crw-server setup
```

This downloads the LightPanda binary and updates your config to use it. Start LightPanda as a sidecar:

```
# Add a systemd service for LightPanda
cat > /etc/systemd/system/lightpanda.service << 'EOF'
[Unit]
Description=LightPanda JS Renderer
After=network.target

[Service]
Type=simple
User=nobody
ExecStart=/usr/local/bin/lightpanda serve --host 127.0.0.1 --port 9222 --block-private-networks
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable lightpanda
systemctl start lightpanda
```

CRW auto-detects SPAs by analyzing the initial HTML response. When it finds an empty body or framework markers (React root divs, Vue app containers), it automatically renders the page through LightPanda before extracting content.

## Step 7: Test Your Setup

From your local machine, test the API:

```
# Basic scrape
curl -X POST https://scraper.yourdomain.com/v1/scrape \
  -H "Authorization: Bearer your-secret-api-key-here" \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com"}'

# Scrape with specific formats
curl -X POST https://scraper.yourdomain.com/v1/scrape \
  -H "Authorization: Bearer your-secret-api-key-here" \
  -H "Content-Type: application/json" \
  -d '{"url": "https://news.ycombinator.com", "formats": ["markdown", "links"]}'

# Start a crawl
curl -X POST https://scraper.yourdomain.com/v1/crawl \
  -H "Authorization: Bearer your-secret-api-key-here" \
  -H "Content-Type: application/json" \
  -d '{"url": "https://docs.example.com", "limit": 10}'
```

## Memory Usage Under Load

The idle footprint is tiny, but what happens under load? We tested CRW on a 512 MB VPS (DigitalOcean Basic) with concurrent requests:

| Concurrent Requests | RAM Usage | Avg Latency |
| --- | --- | --- |
| 1 | ~25 MB | ~30ms (HTTP) |
| 5 | ~15 MB | ~35ms |
| 10 | ~25 MB | ~45ms |
| 20 | ~45 MB | ~80ms |
| 50 | ~90 MB | ~150ms |

Even at 50 concurrent requests, CRW uses under 100 MB — leaving plenty of headroom on a 512 MB VPS. With JS rendering enabled (LightPanda sidecar), add about 50–80 MB for the renderer process.

For comparison, Firecrawl at 1 concurrent request already uses 500 MB+. It physically cannot run on a 512 MB server.

## Running CRW on a Raspberry Pi

CRW is so lightweight that it runs comfortably on a Raspberry Pi. If you have a Pi sitting around, you can turn it into a personal scraping server:

```
# On Raspberry Pi (ARM64)
curl -L https://github.com/fastcrw/crw/releases/latest/download/crw-server-aarch64-unknown-linux-gnu -o /usr/local/bin/crw-server
chmod +x /usr/local/bin/crw-server
crw-server
```

A Raspberry Pi 4 with 2 GB of RAM can handle CRW plus LightPanda plus a reverse proxy with memory to spare. It's a zero-cost scraping server that runs on your desk.

## Connecting Your $5 Scraper to AI Agents

The real power of self-hosting is combining a cheap server with MCP for AI agents. Once your server is running, connect it to Claude Code:

```
claude mcp add crw -- crw-mcp --env CRW_API_URL=https://scraper.yourdomain.com --env CRW_API_KEY=your-secret-api-key
```

Now Claude Code uses your $5 VPS for web scraping. No per-request costs, no metered API, no usage limits. Scrape as much as you want for a flat $5/month.

The same server works with Cursor, Windsurf, Cline, and any other MCP client — just point the MCP config at your server URL.

## Cost Comparison: Self-Hosted vs Cloud Scraping

Let's compare the annual cost for different scraping volumes:

| Monthly Volume | CRW Self-Hosted ($5 VPS) | Firecrawl Cloud | fastCRW Cloud |
| --- | --- | --- | --- |
| 1,000 scrapes | **$5/mo ($60/yr)** | $19/mo ($228/yr) | $13/mo ($156/yr) |
| 10,000 scrapes | **$5/mo ($60/yr)** | $99/mo ($1,188/yr) | $69/mo ($828/yr) |
| 50,000 scrapes | **$5–10/mo ($60–120/yr)** | $499/mo ($5,988/yr) | $69/mo ($828/yr) |
| 100,000 scrapes | **$10–20/mo ($120–240/yr)** | Custom pricing | $69/mo ($828/yr) |

At 10,000 scrapes/month, self-hosting CRW saves you $1,128/year compared to Firecrawl Cloud. At 50,000 scrapes/month, the savings jump to $5,868/year. The flat-rate economics of self-hosting are hard to beat.

## Production Hardening Checklist

Before relying on your $5 scraper in production, go through this checklist:

- **Authentication** — API keys configured in `config.local.toml`
- **HTTPS** — TLS via Caddy or Nginx reverse proxy
- **SSH hardening** — `PermitRootLogin no`, `PasswordAuthentication no` in `/etc/ssh/sshd_config`, use key-based auth only
- **Firewall** — `ufw default deny incoming && ufw allow 22,80,443/tcp && ufw enable`
- **Brute-force protection** — install `fail2ban` for SSH and HTTP rate limiting
- **Rate limiting** — set `rate_limit_rps` in config to prevent abuse
- **Auto updates** — `apt install unattended-upgrades` for security patches
- **systemd** — auto-restart on crash, start on boot
- **Monitoring** — health check endpoint at `/health` for uptime monitoring
- **Backups** — not strictly needed (CRW is stateless), but snapshot the VPS monthly
- **Updates** — `cargo install crw-server` to update to the latest version

## When to Upgrade

A $5 VPS handles most use cases. Consider upgrading when:

- **Consistent 50+ concurrent requests** — move to a 2 GB server ($10–12/mo)
- **Heavy JS rendering** — LightPanda + Chrome sidecar needs ~200 MB; get 2 GB
- **Proxy requirements** — if you need residential proxies for anti-bot sites, consider [fastCRW cloud](https://fastcrw.com) which includes a proxy network
- **High availability** — for production SLAs, run two instances behind a load balancer

## Frequently Asked Questions

### Can I really run a web scraping API on a $5 VPS?

Yes. CRW has a tiny idle footprint — far smaller than Firecrawl's multi-service stack. A 512 MB VPS comfortably handles CRW with room for the OS, a reverse proxy, and even JS rendering via LightPanda. We've tested sustained loads of 50 concurrent requests on a $5 DigitalOcean droplet without issues.

### How does CRW achieve such low memory usage?

CRW is written in Rust, which compiles to native machine code with no garbage collector and no runtime overhead. It uses streaming HTML parsing (lol-html) instead of building a DOM tree in memory, and it processes requests through Tokio's async runtime without spawning OS threads per request.

### Is the Firecrawl API compatibility real?

CRW covers the core of Firecrawl's API surface. The same endpoint paths (`/v1/scrape`, `/v1/crawl`, `/v1/map`), the same request body format, and the same response structure. If you have code that calls Firecrawl's API, you can point it at CRW by changing the base URL — no other code changes needed.

### What about anti-bot protection?

CRW v0.0.11 includes stealth mode with browser-like UA rotation, navigator.webdriver spoofing, and Cloudflare challenge auto-retry. For sites with aggressive bot detection, you'll want to add a proxy — either bring your own or use [fastCRW's managed cloud](https://fastcrw.com) which includes a residential proxy network.

### Can I use this with AI agents?

Absolutely. CRW has a built-in MCP server. Install `crw-mcp`, point it at your $5 VPS, and connect it to Claude Code, Cursor, Windsurf, or any MCP-compatible client. Your AI agent gets web scraping capabilities for $5/month flat — no per-request fees. See our [Claude Code setup guide](/blog/claude-code-web-scraping) for step-by-step instructions.

## Related Guides

- [Add Web Scraping to Claude Code in 30 Seconds](/blog/claude-code-web-scraping) — connect your $5 VPS to AI agents via MCP
- [Scraping Cloudflare-Protected Sites](/blog/bypass-cloudflare-scraping) — stealth mode setup for bot-protected sites
- [Full MCP Setup Guide](/blog/mcp-web-scraping) — all MCP clients, SDK usage, advanced patterns
