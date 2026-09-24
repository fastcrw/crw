# Docker

Run crw with Docker Compose for the easiest setup with JS rendering included.

## Pre-built Image

```bash
docker pull ghcr.io/fastcrw/crw:0.16.0
docker run -p 3000:3000 ghcr.io/fastcrw/crw:0.16.0
```

Available tags: `latest`, `0.16` (tracks the current minor), `0.16.0` (pinned).
Use a pinned tag in production — `latest` rolls forward on every release.

:::note
500 credits to start, up to 500 more from onboarding tasks, no card.
<a href="https://fastcrw.com/register?ref=docs-docker-inline" target="_blank" rel="noopener">Get your API key</a>
:::

## Docker Compose

```bash
git clone https://github.com/fastcrw/crw.git
cd crw
docker compose up
```

> **Build memory.** The container build uses thin LTO across 16 codegen units
> (`CARGO_PROFILE_RELEASE_LTO=thin`, `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16`),
> which keeps peak link memory well below the workspace default (fat LTO + 1 unit).
> Give Docker **≥ 4 GB RAM** for the build. If you're capped lower, serialize
> the link step: `CARGO_BUILD_JOBS=1 docker compose build crw`, then
> `docker compose up -d`. This is a one-time build cost — the runtime image is small.

The bundled `docker-compose.yml` starts these services:

| Service | Internal Port | Published to host? | Default? | Description |
|---------|---------------|--------------------|----------|-------------|
| **crw** | 3000 | ✅ `0.0.0.0:3000`³ | ✅ | API server (loads `config.docker.toml`) |
| **searxng** | 8080 | ❌ internal only² | ✅ | Meta-search backend for `/v1/search` |
| **lightpanda** | 9222 | ❌ internal only | ✅ | Lightweight headless browser for JS rendering |
| **chrome** | 9222 | ❌ internal only | `--profile heavy` | Full Chromium fallback for complex SPAs |
| **chrome-stealth** | 3000¹ | ✅ `127.0.0.1:9224` (loopback) | `--profile stealth` | Anti-fingerprint Chromium (browserless, SSPL-licensed) |

¹ `chrome-stealth` listens on container port 3000 (browserless default) and is published to the host as `127.0.0.1:9224` — loopback only, for a host-run `crw-server` during development. The composed `crw` service reaches it via the Docker bridge as `chrome-stealth:3000` and does not use the host port.

² `searxng:8080` is internal to the Compose network only — `localhost:8080` on the host will not connect. Search traffic flows exclusively through crw's `/v1/search` endpoint. For direct debug access, add `ports: ["127.0.0.1:8888:8080"]` in `docker-compose.override.yml`.

³ The host bind address and port are overridable without editing the Compose file: set `CRW_HOST_PORT` in `.env` (e.g. `CRW_HOST_PORT=3055`) if something else already holds 3000 on the box, and `CRW_BIND_ADDRESS` (e.g. `CRW_BIND_ADDRESS=127.0.0.1`) to stop publishing on public interfaces. Both default to today's behavior (`0.0.0.0:3000`) and the container port stays `3000`. Note: Compose interpolates these on the host into the port mapping only; unlike the `CRW_*` config keys elsewhere they are not delivered into the container, so the engine never reads them.

The `crw` service reads its configuration from the mounted `config.docker.toml` (via
`CRW_CONFIG=config.docker`), which already points each renderer and the search backend at the matching
service name on Compose's default bridge network (`lightpanda:9222`, `chrome:9222`, `searxng:8080`). You
don't need to wire renderer URLs through environment variables — they're in the config file.

The optional `chrome` / `chrome-stealth` tiers are opt-in so small hosts skip the ~500 MB Chromium image:

```bash
docker compose --profile heavy up -d      # add the vanilla Chromium fallback
docker compose --profile stealth up -d    # add the anti-fingerprint tier (review the SSPL license first)
```

## Search backend

`/v1/search` (and the `crw_search` MCP tool) is backed by the bundled **searxng** service, reachable inside
the Compose network as `searxng:8080`. This is configured by default in `config.docker.toml`:

```toml
[search]
search_backend_url = "http://searxng:8080"
```

To point CRW at an **external** search backend instead of the sidecar, override it without editing the file:

```yaml
services:
  crw:
    environment:
      - CRW_SEARCH__SEARCH_BACKEND_URL=http://your-backend-host:8080   # env wins over the config file
```

> **Two different URLs — don't confuse them.** `SEARXNG_BASE_URL` (set on the `searxng` service) is the
> backend's *own* self-reference for the links it renders. `[search].search_backend_url` /
> `CRW_SEARCH__SEARCH_BACKEND_URL` is the host **CRW** calls. They happen to share the value `searxng:8080`
> in the bundled stack, but they serve different roles.

> **Cold start.** `crw` waits for `searxng` to report healthy (`depends_on: condition: service_healthy`),
> so the first search after `docker compose up` can take ~15–30 s once the images are present (longer on the
> first pull). A `target_unreachable` or `timeout` error in the first few seconds usually just means the
> backend hasn't finished booting yet — the server logs the configured search host at startup so you can
> confirm it.

## Dockerfile

Multi-stage cross-compilation build for minimal image size. The builder runs natively
on the build platform (no QEMU for compiles) and cross-compiles to `linux/arm64` via
the `aarch64-linux-gnu-gcc` cross linker. Only the final runtime layer (ca-certificates)
uses QEMU, so multi-arch builds are fast:

```dockerfile
FROM --platform=$BUILDPLATFORM rust:1.93-bookworm AS builder
ARG TARGETARCH
# Install Rust target + cross linker for arm64, then record the triple.
# Override LTO to thin + 16 codegen units (lower peak RAM vs. workspace fat LTO).
ENV CARGO_PROFILE_RELEASE_LTO=thin \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
RUN cargo build --release --target "$(cat /rust_target)" \
      -p crw-server --features cdp -p crw-mcp -p crw-cli

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /out/crw /out/crw-server /out/crw-mcp /usr/local/bin/
COPY config.default.toml config.docker.toml /app/
WORKDIR /app
EXPOSE 3000
CMD ["crw-server"]
```

## Container Security

The bundled `docker-compose.yml` applies defense-in-depth hardening to every service.
Here is what is in place and what you must change before production:

### What the Compose file already enforces

```yaml
services:
  crw:
    read_only: true            # immutable root FS — nothing a parser drops survives a restart
    tmpfs:
      - /tmp                   # the only writable scratch (sandbox children land here)
    cap_drop:
      - ALL                    # engine needs no Linux capabilities
    security_opt:
      - no-new-privileges:true # child processes can never elevate via setuid
    mem_limit: 2g
    memswap_limit: 2g          # no swap escape hatch
    pids_limit: 512            # bounds fork-bombs and runaway worker spawning
```

The same `read_only`, `cap_drop: ALL`, and `no-new-privileges` flags are applied to the
`searxng` service as well.

> **Non-root user.** The runtime image runs as root by default (the Dockerfile does not
> add a dedicated user). Combined with `cap_drop: ALL` and `no-new-privileges:true`, the
> process has no usable Linux capabilities even as UID 0 inside the container. For strict
> CIS/hardening requirements, add a `USER crw` layer in a derived Dockerfile and mount
> writable paths explicitly.

### Secrets you must change before production

Two secrets ship with publicly-known placeholder values. They are safe for local
development (the affected ports are not published to the host by default), but must be
rotated before any internet-facing deployment:

| Secret | Service | Default (do not use in prod) | How to override |
|--------|---------|-------------------------------|-----------------|
| `SEARXNG_SECRET_KEY` | `searxng` | `change-me-with-openssl-rand-hex-32-please` | See below |
| `BROWSERLESS_TOKEN` | `chrome-stealth` | `crwtest` | See below |

Generate strong values and write them to a `.env` file in the project root (never
commit this file):

```bash
# Run once; .env is git-ignored by default
echo "SEARXNG_SECRET_KEY=$(openssl rand -hex 32)" >> .env
echo "BROWSERLESS_TOKEN=$(openssl rand -hex 24)" >> .env
```

Docker Compose reads `.env` automatically. The Compose file picks up the values via:

```yaml
# searxng service
- SEARXNG_SECRET_KEY=${SEARXNG_SECRET_KEY:-change-me-with-openssl-rand-hex-32-please}

# chrome-stealth service
- TOKEN=${BROWSERLESS_TOKEN:-crwtest}
```

The `:-fallback` syntax means an unset variable falls back to the placeholder — the
warning above is intentional, not a bug. Once `.env` is present, the placeholder is
never used.

### Additional hardening tips

- **Read-only bind mounts**: `config.docker.toml` and `settings.yml` are already
  mounted `:ro`. Keep all host mounts read-only unless a service explicitly requires
  write access.
- **Drop published ports you don't need**: `searxng:8080` is not published to the host
  by default. The `chrome-stealth` service binds only to `127.0.0.1:9224`. Audit
  `ports:` entries before exposing to a public interface.
- **Reverse proxy TLS**: Run crw behind nginx/Caddy with TLS termination and
  `CRW_AUTH__API_KEYS` set. Do not expose port 3000 directly to the internet. When
  a proxy fronts crw, set `CRW_BIND_ADDRESS=127.0.0.1` in `.env` so the API only
  publishes on loopback instead of every host interface (see footnote ³).

## LightPanda Restart Policy

LightPanda uses `restart: unless-stopped` and this is **load-bearing**. LightPanda can
OOM or segfault on adversarial pages (heavy SPA bundles, Cloudflare Turnstile). Without
auto-restart the circuit breaker stays open permanently and every JS request falls
through to Chrome — defeating the lightweight tier.

If you remove `restart: unless-stopped` from the `lightpanda` service, expect all JS
rendering to be handled by Chrome (or fail entirely if Chrome is not enabled).

## Custom Configuration

Override settings via environment variables:

```yaml
services:
  crw:
    build: .
    ports:
      - "8080:8080"
    environment:
      - RUST_LOG=debug
      - CRW_SERVER__PORT=8080
      - CRW_CRAWLER__REQUESTS_PER_SECOND=5.0
      - CRW_CRAWLER__USER_AGENT=MyBot/1.0
      - CRW_AUTH__API_KEYS=["sk-production-key"]
      - CRW_RENDERER__LIGHTPANDA__WS_URL=ws://lightpanda:9222
```

## Production Tips

### Resource Limits

Use `mem_limit` / `memswap_limit` / `pids_limit` — these are the standalone Compose
(non-Swarm) knobs and work with `docker compose up` without a Swarm cluster. The
`deploy.resources` block (Swarm syntax) is silently ignored by standalone Compose.

```yaml
services:
  crw:
    mem_limit: 2g
    memswap_limit: 2g    # prevents swap escape — set equal to mem_limit
    pids_limit: 512
```

The bundled `docker-compose.yml` already includes these limits (sized for the PDF
sandbox: `base + 2 × 512 MiB sandbox children ≈ 1.3 GiB < 2 GiB`). Adjust down
if the PDF parser (`[document]`) is disabled or `max_concurrent_parses` is reduced.

crw uses ~14 MB idle and ~66 MB under heavy load (50 concurrent requests). The 2 GB
ceiling in the bundled file is dominated by the PDF sandbox children, not the engine
itself.

### Health Check

```yaml
services:
  crw:
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:3000/health"]
      interval: 30s
      timeout: 5s
      retries: 3
```

### Persistent Config

```yaml
services:
  crw:
    volumes:
      - ./my-config.toml:/app/config.docker.toml:ro
```

### Logging

```yaml
services:
  crw:
    environment:
      - RUST_LOG=crw_server=info,crw_renderer=warn
    logging:
      driver: json-file
      options:
        max-size: "10m"
        max-file: "3"
```
