# crw-sdk

TypeScript/JavaScript SDK for [CRW](https://github.com/fastcrw/crw) — the fast,
Rust-native web data API. Use native `/v1` methods for new CRW integrations;
Firecrawl v2 compatibility is available separately for migration work.

Zero runtime dependencies (Node 18+ `fetch`). Dual ESM + CommonJS.

## Install

```bash
npm install crw-sdk
```

## Quick start — Cloud (default)

CRW is **cloud-first**. [Sign up for 1000 free credits](https://fastcrw.com/dashboard)
— no payment, no monthly reset (GitHub/Google, ~10s) — then set `CRW_API_KEY`:

```ts
import { CrwClient } from "crw-sdk";

const crw = new CrwClient(); // reads CRW_API_KEY from the env
const res = await crw.scrape("https://example.com", { formats: ["markdown"] });
console.log(res.markdown);
```

```ts
// ...or pass the key explicitly
const crw = new CrwClient({ apiKey: "crw_live_..." });
```

## Self-hosting

```ts
// A self-hosted server:
const crw = new CrwClient({ apiUrl: "http://localhost:3000" });
```

```bash
# Local zero-config engine (no server, no key): set CRW_LOCAL=1.
# Requires the `crw-mcp` binary on PATH (or set CRW_BINARY).
CRW_LOCAL=1 node app.js
```

## Methods

| Method | Description | Mode |
|---|---|---|
| `scrape(url, opts?)` | Scrape one URL | both |
| `crawl(url, opts?)` | Crawl a site (async, polled) | both |
| `map(url, opts?)` | Discover URLs (+ the site's `sitemaps`) | both |
| `search(query, opts?)` | Web search (+ optional scrape) | both¹ |
| `parseFile(bytes, opts?)` | PDF → markdown / structured JSON | both |
| `extract({urls, schema?})` | Structured LLM extraction | HTTP |
| `startExtract(opts)` / `getExtract(id)` / `cancelExtract(id)` | Typed extract lifecycle | HTTP |
| `batchScrape(urls, opts?)` | Scrape many URLs (async) | HTTP |
| `capabilities()` | Feature-detect the engine | HTTP |
| `changeTrackingDiff(cur, prev?)` | Diff vs a prior snapshot | HTTP |
| `findData(opts?)` / `getData(provider, capability, options?)` | Structured data from named public APIs | HTTP |
| `close()` | Shut down local subprocesses (returns a promise) | both |

¹ Local search needs a search backend configured on the engine.

```ts
// Structured extraction (async job → per-URL results array):
const results = await crw.extract({
  urls: ["https://example.com"],
  schema: { type: "object", properties: { title: { type: "string" } } },
});
// results: [{ url, status, data, error, llmUsage }]
for (const r of results) if (r.status === "completed") console.log(r.url, r.data);

// Explicit lifecycle. startExtract always sends Prefer: respond-async.
const accepted = await crw.startExtract({
  urls: ["https://a.example", "https://b.example"],
  schema: { type: "object", properties: { title: { type: "string" } } },
});
const status = await crw.getExtract(accepted.id);
await crw.cancelExtract(accepted.id); // idempotent

// Parse a PDF:
import { readFileSync } from "node:fs";
const doc = await crw.parseFile(readFileSync("invoice.pdf"), { formats: ["markdown"] });
```

## Data sources

Structured records from named public APIs (papers, packages, registries). Find a
source, then call it with the options it lists:

```ts
const { items, nextOffset } = await crw.findData({ query: "packages" });
// items: [{ provider, capability, name, requiredOptions, optionalOptions, creditsCost, licence, ... }]

try {
  const result = await crw.getData("depsdev", "package", { system: "npm", name: "react" });
  console.log(result.data, result.provenance.licence);
} finally {
  await crw.close();
}
```

A few sources run on your own machine rather than on the API. For those,
`getData()` starts a local `crw-mcp` (found via `CRW_BINARY` or `PATH`; install with
`npm i -g crw-mcp`) and calls it with your API URL and key. That `crw-mcp` child
stays alive for later calls until you call `close()`.
Data sources are not available with `CRW_LOCAL=1`.

## Parity

This SDK mirrors the Python [`crw`](https://pypi.org/project/crw/) client method-for-method,
and both are conformance-tested against the engine's OpenAPI spec.
