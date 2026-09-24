# Self-Hosting Hardening

## Minimum hardening baseline

- Terminate TLS in front of the API.
- Run the service as a non-root user.
- Restrict inbound access to required ports only.
- Isolate renderer sidecars from unnecessary network paths.

That baseline is the starting point, not the finish line. A self-hosted scraper talks to untrusted public pages and can sit close to valuable internal systems, so it deserves the same discipline as any other internet-facing API.

## Network and Access Control

- Put a reverse proxy or gateway in front of the service.
- Restrict who can reach the API by network, identity, or both.
- Avoid exposing internal health or admin surfaces to the public internet.
- If browser rendering is enabled, isolate the renderer from internal systems it does not need to reach.

## SSRF Protection

fastCRW validates every outbound URL before fetching it (`crw-core/src/url_safety.rs`). The guard is fail-closed and multi-layered:

- **Scheme**: only `http` and `https` are permitted; `file://`, `ftp://`, and all other schemes are rejected.
- **Hostname blocklist**: `localhost`, `metadata.google.internal`, and wildcard aliases (`*.localhost`, `*.localtest.me`, `*.lvh.me`, `*.nip.io`, `*.xip.io`, `*.sslip.io`) are all blocked before DNS resolution.
- **IP blocklist (pre-DNS)**: literal IPs in loopback (127.x, ::1), private (10.x, 172.16-31.x, 192.168.x), link-local (169.254.x — AWS/GCP metadata), unspecified (0.0.0.0, ::), broadcast, carrier-grade NAT (100.64-127.x), multicast/reserved (≥224.x), and IPv4-mapped IPv6 equivalents (e.g. `::ffff:169.254.169.254`) are all rejected.
- **Post-DNS resolution**: the resolved IP of every hostname is checked against the same blocklist, preventing DNS-rebinding. DNS failure is fail-closed — if the hostname cannot be resolved, the request is rejected.
- **Redirect chains**: every hop in an HTTP redirect is re-validated through the same checks (`safe_redirect_policy`). A redirect chain is capped at 10 hops.
- **URL length**: URLs longer than 2 048 characters and URLs containing null bytes are rejected.

### `CRW_ALLOW_LOOPBACK_FOR_TESTS` — never enable in production

The env var `CRW_ALLOW_LOOPBACK_FOR_TESTS=1` disables the loopback and private-range checks so integration tests can target mock servers at `127.0.0.1:<random>`. It is **not a runtime configuration option** — it exists solely to make integration tests pass in CI without a public network. Setting it in a production process removes the SSRF guard for all outbound fetches and is never safe to do.

## Admin and Ops Endpoints

Three operational endpoints expose internal state:

| Endpoint | Method | Purpose |
|---|---|---|
| `/metrics` | `GET` | Prometheus text format metrics |
| `/metrics/renderer-breakers` | `GET` | JSON snapshot of all circuit-breaker states |
| `/admin/breakers/reset` | `POST` | Resets all circuit breakers; returns count of host entries cleared |

These endpoints sit **inside** the authentication boundary. When you configure `[auth].api_keys`, they require a valid `Authorization: Bearer <key>` header, exactly like the scraper API. With **no** keys configured (the default self-host case), they are open — same as every other route — so restrict the service by network in that mode.

**Breaking change (key-secured deployments):** if you scrape `/metrics` with a Prometheus job and have `api_keys` set, the scrape now needs the token. Add it to your scrape config:

```yaml
scrape_configs:
  - job_name: crw
    authorization:
      type: Bearer
      credentials: "<your-api-key>"
    static_configs:
      - targets: ["crw:3000"]
```

**Defense in depth (recommended regardless of auth):**

- Network policy: bind fastCRW to a non-public interface or port and allow only your monitoring system to reach it. In Docker Compose publish only to localhost (`127.0.0.1:<port>:<port>`), never `0.0.0.0`.
- Reverse-proxy access control: block or require HTTP Basic / mutual-TLS on the `/metrics` and `/admin` path prefixes before traffic reaches fastCRW.
- Firewall rule: drop inbound traffic to the fastCRW port except from trusted source IPs.

## Cross-Origin (CORS)

By default the engine sends **no** CORS headers, so browsers block cross-origin JavaScript from reading API responses — the safe posture for a server-to-server API. If a browser app must call the engine directly, set an explicit allowlist:

```toml
[server]
cors_allowed_origins = ["https://app.example.com"]
```

or `CRW_SERVER__CORS_ALLOWED_ORIGINS="https://app.example.com,https://admin.example.com"`. A literal `"*"` is rejected — wildcard CORS is exactly what this setting replaces. (The `/openapi.json` schema endpoint is intentionally readable cross-origin so SDK generators can fetch it.)

## Runtime Isolation

Treat page fetching and browser rendering as higher-risk components than your application logic.

- run them with the least privilege possible,
- keep filesystem access narrow,
- and isolate sidecars so a renderer problem does not automatically become a broader platform problem.

## Secrets and Keys

- Keep API keys, proxy credentials, and LLM keys out of image builds.
- Inject secrets at runtime through your platform's secret store.
- Rotate keys during environment changes or incident response, not only on a fixed calendar.

### Rotating `SEARXNG_SECRET_KEY` and `BROWSERLESS_TOKEN`

Both variables ship with **publicly-known placeholder defaults** in the repository. These defaults are intentional for local dev but must be replaced before any internet-facing or shared deployment.

**`SEARXNG_SECRET_KEY`** (`docker-compose.yml` line: `${SEARXNG_SECRET_KEY:-change-me-with-openssl-rand-hex-32-please}`):
The search backend uses this key to sign session cookies. Anyone who knows the default value can forge cookies against your instance. Generate a real key and inject it:

```bash
echo "SEARXNG_SECRET_KEY=$(openssl rand -hex 32)" >> .env
```

**`BROWSERLESS_TOKEN`** (`docker-compose.yml` and `docker-compose.stealth.yml`: `${BROWSERLESS_TOKEN:-crwtest}`):
Browserless v2 requires this token in every CDP WebSocket URL (`?token=...`). The default `crwtest` is public. Without a real token, any process that can reach the browserless port can open Chrome sessions against your host. Generate and inject:

```bash
echo "BROWSERLESS_TOKEN=$(openssl rand -hex 24)" >> .env
```

Both commands append to an `.env` file that Docker Compose reads automatically. After setting the variables, restart the affected containers:

```bash
docker compose up -d --force-recreate searxng chrome-stealth
```

Rotate again whenever you rotate other credentials (deployment cutover, suspected exposure, team member offboarding).

## AGPL-3.0 §13 — Network Use Obligations

fastCRW is licensed under AGPL-3.0 (`Cargo.toml`: `license = "AGPL-3.0"`). AGPL §13 extends the GPL copyleft to network use: **if you run a modified version of fastCRW and allow third parties to interact with it over a network (including your own users calling your API), you must offer them the complete corresponding source code** under the same license.

What this means in practice:

- **Unmodified self-hosting**: no source-offer obligation beyond what AGPL always requires. Point users at the upstream repository.
- **Modified self-hosting exposed to third parties**: you must make the modified source available. The standard approach is a public fork or a `GET /source` endpoint that redirects to a tagged archive.
- **Internal use only** (your own employees, no third-party network access): the network-use trigger does not apply. Standard GPL obligations still apply if you distribute binaries.
- **Commercial carve-out**: if AGPL compliance is impractical for your use case (embedding in a proprietary product, offering a managed service without source disclosure), a commercial license is available — see the [LICENSE](https://github.com/fastcrw/crw/blob/main/LICENSE) file and contact information in the repository.

**What counts as a modification**: any change to the Rust source in `crates/` or the build scripts that alters runtime behavior. Configuration-only changes (`.toml` files, `.env` files, `docker-compose.yml` overrides) do not constitute a modification of the software itself.

## LLM features and trust boundary

CRW's `summary` format and `/v1/search` answer/summarize features need an LLM key. There are two deployment shapes; pick one and lock it down with the runtime guards listed below.

**Solo / self-hosted (key in server config):**

```toml
[extraction.llm]
provider = "openai"
api_key  = "sk-..."
model    = "gpt-4o-mini"
```

Anyone who can reach your opencore can spend on that key. Front it with auth, network policy, or a private network.

**SaaS / multi-tenant (per-request keys):**

- Set `CRW_DISABLE_SERVER_LLM_KEY=1` in opencore's environment. With this env var set, opencore refuses to boot if `[extraction.llm].api_key` is also configured — the most common operator mistake.
- Set `[extraction.llm].require_byok_header = "X-CRW-Tenant"` (or similar). CRW rejects LLM-touching requests that lack that header AND do not pass a per-request `llmApiKey`. Your SaaS layer adds the header on every forwarded request; direct public callers cannot.
- Don't expose opencore on a public address; keep it behind your SaaS proxy.
- Use `GET /v1/capabilities` on boot from the SaaS layer to verify the opencore version's feature set before showing LLM toggles in your UI.

**Per-request budget:**

- `[extraction.llm].max_html_bytes` (default `100000`) caps content sent to the LLM.
- Per-request `maxContentChars` and `maxCharsPerSource` are clamped server-side (200 KB and 32 KB respectively) regardless of value.
- `summaryPrompt` and `answerPrompt` are truncated at 500 chars and cannot override the safety wrapper.
- The citation list is capped at 20 entries; fabricated `source_id`s are dropped.

## Operational guidance

- Rotate API keys during deployment cutovers.
- Keep browser-rendering dependencies on the smallest possible surface area.
- Expose `/health` only where your load balancer or monitoring needs it.
- Review warning-heavy targets separately; they often indicate anti-bot defenses rather than renderer bugs.

## Monitoring and Auditability

At minimum, watch:

- API error rate,
- warning frequency,
- crawl job duration,
- renderer availability,
- and resource spikes on the browser sidecar.

Keep enough logs to answer three questions after an incident:

1. what URL or workload triggered the issue,
2. whether it was an engine problem or a target-site problem,
3. and what data, if any, was still returned.

## Example Hardening Sequence

If you are moving from a dev VM to a real environment, the order should usually be:

1. put a reverse proxy and TLS in front,
2. add auth and external rate limiting,
3. move secrets into runtime injection,
4. restrict network access around the API and any renderer sidecar,
5. then enable monitoring and alerting on warnings, failures, and resource spikes.

That order keeps the riskiest exposure points under control early instead of treating hardening as a final cleanup step.

## When To Isolate the Renderer More Aggressively

Stronger isolation is worth it when:

- your targets are highly dynamic and require frequent JS rendering,
- the service runs close to internal systems with sensitive access,
- or many tenants or workloads share the same cluster.

In those cases, a renderer problem should not become an easy pivot into the rest of your infrastructure.

## Common Mistakes

- Leaving `/health` broadly exposed when only an internal load balancer needs it.
- Exposing `/metrics`, `/metrics/renderer-breakers`, or `/admin/breakers/reset` to the public internet — these are unauthenticated and require network-level protection.
- Deploying with the default `SEARXNG_SECRET_KEY` (`change-me-with-openssl-rand-hex-32-please`) or `BROWSERLESS_TOKEN` (`crwtest`) — both are publicly known.
- Setting `CRW_ALLOW_LOOPBACK_FOR_TESTS=1` outside of a test environment — this removes SSRF protection for all outbound fetches.
- Running the service with broader filesystem or network access than the scraping workload requires.
- Keeping incident logs too thin to separate target-site anti-bot issues from engine regressions.

Pair this page with [rate limits](/docs/rate-limits) and [error codes](/docs/error-codes) so operational hardening and runtime diagnostics are documented together.
