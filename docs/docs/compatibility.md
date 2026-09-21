# Compatibility Matrix

## What "Compatible" Means Here

CRW's recommended API for new integrations is native `/v1`. For existing Firecrawl v2 SDK projects, a base-URL swap to `/firecrawl/v2` covers the request-shape overlap with minimal code changes. Validate the documented behavioral differences below before moving production traffic.

How you point the SDK depends on the language: the **JS/TS SDK** honours a `/firecrawl` path in `apiUrl` (so it reaches `/firecrawl/v2/*`), while the **Python SDK** drops the path and lands on the equivalent root `/v2/*` — same engine. See [Migrate from Firecrawl](/docs/migrate-from-firecrawl) for the exact `apiUrl` / `api_url` values.

## Supported alignment

| Area | Status |
| --- | --- |
| `/v1/scrape`, `/v1/crawl`, `/v1/map` route shape | Supported |
| `limit`, `maxPages`, `max_pages` for crawl caps | Supported |
| Numeric `waitFor` for JS rendering | Supported |
| `cssSelector`, `xpath`, `chunkStrategy`, `filterMode` | Supported |

## Known differences

| Area | Current behavior |
| --- | --- |
| Screenshot output | Supported, but only on an instance with a capture-capable browser tier (Chrome or Playwright). Check `screenshot.supported` on `GET /v1/capabilities` |
| `success` semantics | `success: false` when target returns HTTP 4xx/5xx with minimal content; `success: true` with `warning` when target returns error status but has real content |
| Anti-bot walls | A wall no renderer tier can clear is HTTP 200 with `success: false` and `error_code: "anti_bot"`, not a 5xx. Firecrawl-shaped code that branches on the HTTP status will read it as a success, so branch on `success` |
| JS waiting | Numeric delay only; no selector-based wait primitive |
| `extract` format | Accepted as alias for `json`. Use `formats: ["json"]` with `jsonSchema` for structured extraction |
| SDKs | Official packages: `crw` (Python 0.16.0) and `crw-sdk` (TypeScript 0.16.0) |

Treat this page as the source of truth during migrations.

If you are not migrating Firecrawl code, start with [Choose Your Endpoint](/docs/choose-endpoint) and the native `/v1` routes instead.

## Migration Checklist

If you are moving an existing Firecrawl-style integration:

1. verify `scrape`, `crawl`, and `map` request bodies against real targets,
2. confirm how your code interprets `success`, `warning`, and target-side HTTP statuses,
3. check `GET /v1/capabilities` for the features you depend on (screenshots, for example, need a capture-capable browser tier),
4. and compare output quality, not just endpoint shape.

Compatibility at the request level is useful, but output semantics and operational behavior matter just as much.

## Example Evaluation Workflow

A practical migration test usually looks like this:

1. take one production URL that already works in the old integration,
2. run it through [scrape](/docs/scraping) with the same high-level options,
3. compare markdown quality, warnings, and target status behavior,
4. repeat the test on one JS-heavy page and one failure-prone page,
5. only then update the calling code.

That sequence catches the gap between request-shape compatibility and output-quality compatibility.

## When Compatibility Is "Good Enough"

Compatibility is good enough when your migration goal is:

- preserving the existing mental model,
- keeping endpoint names and common options familiar,
- and minimizing changes in the application layer.

It is not good enough if your current system depends on features this page already marks as unsupported or behaviorally different.

## Common Mistakes

- Treating `/firecrawl/v2` as the default API for a new CRW build. Use `/v1` unless you are migrating Firecrawl v2 SDK code.
- Assuming route-name compatibility means output semantics are identical.
- Migrating a whole workload before testing `warning` handling and failure cases.
- Assuming a capability is present without calling `GET /v1/capabilities`, and then discovering the gap in production.

Use this page with [output formats](/docs/output-formats) and [error codes](/docs/error-codes) so your migration covers both payload shape and operational behavior.
