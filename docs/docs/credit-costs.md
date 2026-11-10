# Credit Costs

:::note
Cloud only (fastcrw.com) -- self-hosted instances do not have credit-based billing.
:::

## Current billing rules

| Operation | Credit cost |
| --- | --- |
| `scrape` (any render — HTTP, lightpanda, chrome, or `chrome_proxy`) | 1 credit |
| `scrape` with `playwright` render | SaaS billing only; engine `data.creditCost` is omitted (0) |
| `scrape` with structured extraction (`formats: ["json"]` / `summary`) | 1 credit + LLM cost for that call |
| `scrape` with `mode: "auto"` | 1 credit when the normal ladder gets the page; 1 credit + the traffic the unblocking path used when the page is walled. Capped by `maxCredits` |
| `scrape` with `mode: "max"` | 1 credit + the traffic the unblocking path used. Capped by `maxCredits` |
| `extract` (`POST /v1/extract`) | The base scrape credit plus the actual LLM cost for that call, per URL. Dynamically metered: there is no flat credit number for extract |
| `map` | 1 credit |
| `crawl` start | 1 credit |
| `crawl` polling | New pages discovered since the previous poll |
| `search` | 2 credits |
| `search` + scrape | 2 credits + 1 per scraped result |
| `monitor` create / list / get | 0 credits |
| `monitor` check (per run) | 1 credit per scraped page, plus 1 more credit per page judged changed |
| `browse` | Not a billed cloud endpoint. It is a local CLI/companion capability, free either way |

Every renderer costs 1 credit per page: there is no Chrome or proxy surcharge. The one exception is the opt-in [auto and max mode](/scraping#auto-and-max-mode), which you ask for per request and which is billed by traffic. `data.creditCost` matches the charge regardless of which renderer (HTTP, lightpanda, Chrome, chrome_proxy) actually served the page.

`extract` and structured-extraction scrapes are never a flat fee. They cost the base 1-credit render plus the real LLM cost of that call, metered per request. The open-source engine itself adds no surcharge beyond the 1-credit render cost; the LLM portion is applied by the managed cloud billing layer and tracked separately in `creditsUsed`.

A monitor's check runs one scrape per tracked page, so its cost scales with pages watched, not with how often you poll the monitor's own status. Creating, listing, or reading a monitor's configuration never consumes credits, only an executed check does.

:::note
**Managed-LLM billing (fastcrw.com cloud only)**: token usage for managed LLM features (`extract`, `json`/`summary` scrape extraction, `answer`, `summarizeResults`) is billed on top of the base credit by the cloud platform, not by the open-source engine. Self-hosted deployments have no billing layer and are unaffected.
:::

## Billing on failure

A request is refunded, not charged, when the upstream response is a 4xx, a 5xx, or an HTTP 200 whose envelope carries `success: false` (an anti-bot wall or a target error page with nothing extractable). None of these count as billable work.

A response that comes back HTTP 200 with `success: true` and content, even if it also carries a `warning` (for example a partially blocked page that still returned usable content), is charged normally. A warning on a successful result is not the same as a failure.

## Top-up credits

Purchased top-up credits never expire and are not tied to your current billing cycle. Cancelling your subscription zeroes only the plan's included/monthly credit allowance; any purchased top-up balance survives cancellation and stays spendable.

## Free tier

The FREE plan grants 500 credits **once, for the lifetime of the account**, not monthly, and it never resets, plus up to 500 more from onboarding tasks. No card is required to get them. Once they are spent, a FREE account either upgrades to a paid plan or stops.

## Why crawl billing looks different

The crawl start reserves the job. Subsequent polls charge only for newly materialized pages, not for the total accumulated page count each time.

That prevents the same already-seen pages from being charged again and again just because you are checking progress.

## Simple Examples

| Scenario | Credit effect |
| --- | --- |
| One `scrape` request | 1 credit |
| One `map` request | 1 credit |
| Start one crawl job | 1 credit |
| Poll a crawl and receive 7 new pages | 7 additional credits |
| Poll again with no new pages | No new page credits |
| Search for "AI tools" with 5 results | 2 credits |
| Search + scrape 3 results | 2 + 3 = 5 credits |
| Search + scrape, 1 scrape fails | 2 + 2 = 4 credits (failed scrape refunded) |

## What Usually Does Not Consume Permanent Credits

The billing logic is designed to avoid charging you for requests that never become real usable work. See [Billing on failure](#billing-on-failure) above for the exact rule: 4xx, 5xx, and `success: false` responses are refunded; a successful response with a warning is not.

The safest way to confirm actual consumption is still the balance endpoint before and after a test.

## The `creditCost` response field

Every successful scrape response (v1 `/v1/scrape`) includes a `creditCost` field inside the `data` object:

```json
{
  "success": true,
  "data": {
    "markdown": "...",
    "metadata": { ... },
    "creditCost": 1
  }
}
```

The value reflects the renderer cost only (a flat 1 credit for every renderer). On the managed cloud the SaaS billing layer may charge additional credits for LLM features (extraction, summary), which are tracked separately in `creditsUsed` on v2 responses and in your account billing dashboard.

The field is omitted when its value would be 0 (internal paths that have not yet been priced).

## Hiding credit fields on self-hosted MCP

Self-hosted instances have no billing layer, so `creditCost` / `creditsUsed` in MCP tool responses are pure context overhead (a handful of tokens per response). Set `[mcp] hide_credits = true` (or `CRW_MCP__HIDE_CREDITS=true`, or `--hide-credits` for a one-off) to strip them from every tool result on the `/mcp` endpoint, embedded `crw-mcp`, `crw mcp`, and proxy mode. Fields you extracted yourself are never stripped, even if your schema happens to name one `creditCost`. The REST API response shape is unchanged. See [Configuration](/docs/configuration).

## Balance check

Use `GET /api/v1/account/balance` (cloud only — fastcrw.com) with your API key to inspect included credits, purchased balance, and total available credits.

## Example Monitoring Pattern

A simple integration-safe pattern is:

1. read balance before a new workflow rollout,
2. run a bounded test batch,
3. read balance again,
4. compare expected consumption with actual consumption.

That is especially useful for crawl jobs because the start request and the page-materialization charges happen at different times.

## When To Watch Credits Closely

Watch credits closely when:

- you are polling crawl jobs at high frequency,
- many workers share one account balance,
- or you are benchmarking output quality across multiple target sites.

If request rate is the only thing you monitor, you can still be surprised by crawl-heavy usage. Pair billing checks with [rate limits](/docs/rate-limits) so throughput and credit consumption are interpreted together.

## Operational Advice

- Use small test batches before large crawls.
- Check balance before and after integration changes.
- Separate "request volume" monitoring from "credit consumption" monitoring; they are related but not identical.

## Common Mistakes

- Assuming every crawl poll re-bills the full job instead of only newly materialized pages.
- Launching large crawls before validating cost on a much smaller limit.
- Treating validation failures and refunded work as if they were successful billable jobs.
