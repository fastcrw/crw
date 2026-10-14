# Pergamon

:::note
Cloud only (fastcrw.com). Self-hosted instances have no provider registry and no credit wallet.
:::

## Overview

Pergamon is the fastCRW data API. It puts official APIs, public registries and
statistics offices behind the same key, the same credit wallet and the same error
shape as the rest of fastCRW.

The name is the library that rivalled Alexandria. Alexandria cut off its papyrus
supply to starve it; Pergamon invented parchment instead, which is where the word
comes from.

You do not learn a provider's quirks. You read the catalog, call a capability, and
get back a normalized envelope with the data and the provenance you need to use it
legally.

Browse what is live at [fastcrw.com/pergamon](https://fastcrw.com/pergamon).

## The two endpoints

```
GET https://api.fastcrw.com/v1/data                          the catalog, free
GET https://api.fastcrw.com/v1/data/{provider}/{capability}  the data, 1 credit
```

Both take the standard header:

```
Authorization: Bearer YOUR_API_KEY
```

## Discover what is available

The catalog is free to call and is generated from the same registry the API serves,
so it is never out of date.

```bash
curl https://api.fastcrw.com/v1/data \
  -H "Authorization: Bearer YOUR_API_KEY"
```

```json
{
  "success": true,
  "credits_used": 0,
  "providers": [
    {
      "slug": "npm",
      "name": "npm Registry",
      "description": "Package metadata from the public npm registry...",
      "category": "Developer",
      "country": null,
      "licence": "Public npm registry metadata, served without a key...",
      "licence_url": "https://docs.npmjs.com/policies/open-source-terms",
      "attribution_text": "Source: npm registry",
      "commercial_use": true,
      "redistribution": false,
      "capabilities": [
        {
          "slug": "package",
          "description": "Full packument for a package: every version, dist-tags, maintainers.",
          "path": "/v1/data/npm/package",
          "required_params": ["name"],
          "optional_params": [],
          "credits": 1
        }
      ]
    }
  ]
}
```

`required_params` and `optional_params` are the whole contract. There is nothing
else to know about a capability before calling it.

## Call a capability

```bash
curl "https://api.fastcrw.com/v1/data/npm/package?name=react" \
  -H "Authorization: Bearer YOUR_API_KEY"
```

Every successful response has the same shape:

```json
{
  "success": true,
  "provider": "npm",
  "capability": "package",
  "credits_used": 1,
  "request_id": "fP4Nxx4Urn-H06AxscbM_",
  "provenance": {
    "source": "npm Registry",
    "upstream_url": "https://registry.npmjs.org/react",
    "licence": "Public npm registry metadata, served without a key...",
    "licence_url": "https://docs.npmjs.com/policies/open-source-terms",
    "attribution_text": "Source: npm registry",
    "retrieved_at": "2026-10-11T16:44:02.117Z",
    "adapter": "rest",
    "cache_age_seconds": 0,
    "is_stale": false
  },
  "data": { }
}
```

`data` is the provider's own payload, passed through. We normalize the envelope
around it, not the records inside it — so you keep every field the source
publishes.

### The provenance block

| Field | What it is for |
| --- | --- |
| `source` | The provider's name, as it should be credited |
| `upstream_url` | The exact URL we requested, so you can verify us |
| `licence` / `licence_url` | The source's own terms, quoted |
| `attribution_text` | The line to display wherever you show the data |
| `retrieved_at` | When the body was fetched upstream |
| `cache_age_seconds` | Seconds since that fetch. `0` on a live call |
| `is_stale` | `true` when the body came from our cache rather than a live call |

If a source requires attribution, `attribution_text` is how you satisfy it without
hard-coding anything per provider.

## Batch and discovery: the `pergamon` call

Alongside the two REST endpoints there is a batch surface, and it is deliberately
the same shape Firecrawl uses for Alexandria: `POST /v2/scrape` with a `pergamon`
array instead of a `url`. Up to **ten** calls per request, each naming a provider,
a capability and its options.

```bash
curl https://api.fastcrw.com/v2/scrape \
  -H "Authorization: Bearer $FASTCRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
  "pergamon": [
    { "provider": "openalex", "capability": "works",
      "options": { "search": "web scraping", "per-page": 2 } },
    { "provider": "sec", "capability": "companyconcept",
      "options": { "cik": "0000320193", "concept": "Revenues" } }
  ]
}'
```

```json
{
  "success": true,
  "request_id": "fP4Nxx4Urn-H06AxscbM_",
  "data": {
    "pergamon": [
      { "provider": "openalex", "capability": "works", "pergamonId": "…",
        "creditsCost": 1, "data": { }, "provenance": { } },
      { "provider": "sec", "capability": "companyconcept", "pergamonId": "…",
        "creditsCost": 1, "data": { }, "provenance": { } }
    ],
    "creditsCost": 2
  }
}
```

A failing call does not fail the batch. That item carries
`error: { message, status, code }` instead of `data`, costs nothing, and the rest
of the batch still returns — one bad provider must not throw away the nine that
worked. The exception is running out of credits, which ends the batch with a
single `402`.

Calls run one after another, not in parallel. Ten simultaneous requests to a small
government endpoint from one IP is not a client we want to be.

:::note
If you already call Firecrawl's Alexandria, the request is the same with one word
changed: `alexandria` becomes `pergamon`, and the pseudo-provider `firecrawl`
becomes `fastcrw`. Provider and capability names are our own.
:::

### find-tools

The same call carries discovery, through the pseudo-provider `fastcrw`. It is free
and it costs no credits.

```bash
curl https://api.fastcrw.com/v2/scrape \
  -H "Authorization: Bearer $FASTCRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"pergamon":[{"provider":"fastcrw","capability":"find-tools",
       "options":{"level":"providers","categories":["Research"],"limit":5}}]}'
```

Three levels, each item carrying a `next` you can feed straight back in:

| `level` | Returns | Each item's `next` |
| --- | --- | --- |
| `categories` (default) | Subjects, with how many providers each has | the providers in it |
| `providers` | Provider, description, licence, attribution, tool count | its tools |
| `tools` | Capability, credit cost, required and optional options, provenance | **the call itself** |

Options: `level`, `limit` (max 200), `offset`, `categories`, `providers`,
`capabilities`, `query`. The response's top-level `next` pages; it is `null` on the
last page. `catalogueVersion` is a hash of the whole registry, so you can tell
whether anything moved between two walks without diffing the pages.

A tool item's `next` is an executable call with its required options stubbed, so
walking the catalogue to the bottom hands you the request to make.

## Credits

One credit per call, flat, whatever the provider. The catalog endpoint is free.

Nothing is billed for a failure: if the upstream returns an error, times out, or
answers with something unusable, the credit is refunded automatically and the
response says so.

A cache hit **is** billed. The credit pays for the record; the cache buys you the
latency. `provenance.is_stale` tells you which one you got.

## Caching

Each provider carries its own TTL, and the **licence sets it, not latency**. Where a
source permits retention we cache hard; where a source says responses may not be
retained, the TTL is `0` and every call goes upstream. Each provider page under
[fastcrw.com/pergamon](https://fastcrw.com/pergamon) states its TTL.

## Errors

```json
{
  "success": false,
  "error": "Missing required parameter \"name\"",
  "request_id": "fP4Nxx4Urn-H06AxscbM_"
}
```

| Status | Meaning |
| --- | --- |
| `400` | A parameter is missing, too long, or not valid |
| `401` | Missing or invalid API key |
| `402` | Out of credits |
| `404` | No such provider or capability, or it is not currently enabled |
| `429` | Rate limited — see [Rate limits](/rate-limits) |
| `501` | The provider needs your own upstream credential (not available yet) |
| `502` | The provider answered, but with an error or an unusable body |
| `504` | The provider did not answer in time |

A `404` is deliberately indistinguishable between "never existed" and "switched
off". Call the catalog to see what is live right now.

### Why a 200 can still be a 502

Several official data sources answer `200` with an HTML error page, a WAF
challenge, or an empty body. We treat the content type and the JSON parse as the
real status check, so you get a `502` and your credit back rather than a page of
HTML where a dataset should be.

## Parameters and safety

Required parameters come from the capability's path; optional ones are an explicit
allowlist. Anything you send that is in neither is dropped before the upstream call,
and your values can never change which host is contacted — only what is asked of it.

## Worked examples

```bash
# A package and its weekly downloads
curl "https://api.fastcrw.com/v1/data/npm/package?name=react" \
  -H "Authorization: Bearer $FASTCRW_API_KEY"

curl "https://api.fastcrw.com/v1/data/npm/downloads?period=last-week&name=react" \
  -H "Authorization: Bearer $FASTCRW_API_KEY"

# A Python project's releases
curl "https://api.fastcrw.com/v1/data/pypi/project?name=requests" \
  -H "Authorization: Bearer $FASTCRW_API_KEY"
```

```javascript
const res = await fetch(
  "https://api.fastcrw.com/v1/data/npm/package?name=react",
  { headers: { Authorization: `Bearer ${process.env.FASTCRW_API_KEY}` } },
);
const body = await res.json();

console.log(body.data["dist-tags"].latest);
console.log(body.provenance.attribution_text); // display this
```

```python
import os, requests

r = requests.get(
    "https://api.fastcrw.com/v1/data/pypi/project",
    params={"name": "requests"},
    headers={"Authorization": f"Bearer {os.environ['FASTCRW_API_KEY']}"},
    timeout=30,
)
body = r.json()
print(body["data"]["info"]["version"])
print(body["provenance"]["attribution_text"])
```

## Asking for a source

The catalog grows by request. If a source you need is missing, [tell us which
one](https://fastcrw.com/support) — including the URL of its API or its open-data page, which is what
decides how quickly it can be added.

A source only appears in the catalog once someone has read its terms. If an answer
is "not without a written agreement", it stays out rather than shipping quietly.
