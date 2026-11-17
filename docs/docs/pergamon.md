# Pergamon

:::note
Cloud only (fastcrw.com). Self-hosted instances have no provider registry and no credit wallet.
:::

## Overview

Pergamon is the fastCRW data API. It puts official APIs, public registries and
statistics offices behind the same key, the same credit wallet and the same error
shape as the rest of fastCRW.

The name is the library of Pergamon, one of the great collections of the ancient
world. Its scribes gave us parchment, and the word still carries the city's name.

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
      "logo_url": "https://fastcrw.com/logos/pergamon/npm.png",
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

Alongside the two REST endpoints there is a batch surface, and it is the same
shape Firecrawl uses for Alexandria: `POST /v2/scrape` with a `pergamon` array
instead of a `url`. Up to **ten** calls per request, each naming a provider, a
capability and its options.

**Both body keys are read.** `pergamon` is ours, `alexandria` is theirs, and the
pseudo-provider may be `fastcrw`, `pergamon` or their own `firecrawl`. The
response answers in whichever vocabulary you used: a request keyed `alexandria`
gets `data.alexandria` and an `alexandriaId` on each result, a request keyed
`pergamon` gets `data.pergamon` and a `pergamonId`, and every `next` points back
at the pseudo-provider you named. An Alexandria client needs no edit at all.

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
If you already call Firecrawl's Alexandria, point your base URL here and send the
request unchanged — the body key, the pseudo-provider, the option names and every
response field match theirs. Only the provider and capability names are our own,
because the catalogue is.

One value is deliberately different: with a `query`, `mode` is `"text"` and not
`"semantic"`. Ranking here is lexical, and saying otherwise would misdescribe it.
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

| `level` | Each item carries | Its `next` |
| --- | --- | --- |
| `categories` (default) | `id`, `description` | the providers in it |
| `providers` | `id`, `provider`, `name`, `description`, `logoUrl`, `rank`, `toolCount` | its tools |
| `tools` | `id`, `provider`, `capability`, `name`, `description`, `creditsCost`, `perRecord` | **this tool, fully expanded** |

A discovery listing is deliberately lean. It carries what you need to *choose* a
tool; `expand` carries what you need to *call* one. Licence and attribution are
not here because they are owed on delivery, and they ride on every execution
response instead.

At `tools` level `next` drills into this one capability with every expansion
turned on, and `execute` — present once `expand` is in the request — is the
handle that actually runs it.

Options: `level`, `limit` (max 200), `offset`, `categories`, `providers`,
`capabilities`, `query`, `expand`. The response's top-level `next` pages; it is
`null` on the last page. `catalogueVersion` is a fingerprint of the registry, so
you can tell whether anything moved between two walks without diffing the pages.

A tool item's `next` is an executable call with its required options stubbed, so
walking the catalogue to the bottom hands you the request to make.

#### Searching with `query`

`query` is a ranked full-text search over provider and capability text, with
stemming — `companies` and `company` find the same rows. Multiple words are
matched independently and scored together, so a query reads as a description of
what you want rather than a string that has to appear somewhere verbatim.

```bash
curl https://api.fastcrw.com/v2/scrape \
  -H "Authorization: Bearer $FASTCRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"pergamon":[{"provider":"fastcrw","capability":"find-tools",
       "options":{"level":"tools","query":"clinical trial results","limit":5}}]}'
```

Two fields come back with a query: `mode` is `"text"` instead of `"browse"`, and
each item carries a `relevance` score. Rank order is the ranking — sort by
`relevance` only if you merge pages. Rarer words count for more than common ones,
so in *clinical trial results* the word `results` moves the ranking far less than
`clinical` does.

Ranking is lexical. It matches the words you wrote and their stems, not their
meaning, so a word the catalogue never uses for a thing will not find it. If you
know the subject's vocabulary, `query` is the fastest way in; if you do not,
walking `categories` is more reliable.

#### `expand`: what a tool needs before you call it

By default a tool item carries enough to choose between tools. `expand` adds what
you need to actually make the call, and only at `level: "tools"`.

| `expand` value | Adds |
| --- | --- |
| `options` | Every parameter: `name`, `type`, `in` (`path` or `query`), `about`, `required`, and `default`/`example`/`enum` where there is one |
| `response` | The response contract: `about`, `key` (the property holding the records), `fields` with their JSON types, and `paginated` |
| `examples` | A ready-to-run request with each parameter's example value filled in |

`expand` also adds `execute`, the `{provider, capability}` handle for running the
tool. Without `expand`, `options` is absent entirely.

```bash
curl https://api.fastcrw.com/v2/scrape \
  -H "Authorization: Bearer $FASTCRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"pergamon":[{"provider":"fastcrw","capability":"find-tools",
       "options":{"level":"tools","providers":["usgs"],"capabilities":["count"],
                  "expand":["options","response","examples"]}}]}'
```

```json
{
  "id": "usgs/count",
  "provider": "usgs",
  "capability": "count",
  "creditsCost": 1,
  "perRecord": false,
  "options": [
    { "name": "starttime", "type": "date", "in": "query",
      "about": "ISO date or datetime; only events at or after this instant. Defaults to 30 days ago.",
      "required": false, "example": "2026-09-01" }
  ],
  "response": {
    "about": "Returns just {count, maxAllowed} for the same filters the search accepts.",
    "key": "", "fields": [], "paginated": false
  },
  "execute": { "provider": "usgs", "capability": "count" }
}
```

## Tools in search results

`POST /v2/search` can return catalogue tools next to web pages. Add `pergamon` to
`sources`:

```bash
curl https://api.fastcrw.com/v2/search \
  -H "Authorization: Bearer $FASTCRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"query":"earthquake magnitude data","sources":["web","pergamon"],"limit":3}'
```

`data.web` holds the pages as before and `data.tools` holds the catalogue hits.

A tool in a **search** result is a pointer, not a contract: `provider`,
`capability` and `description`, and nothing else. That is Alexandria's shape and
it is ours. Follow it with a `find-tools` call on those two names — with
`expand` — to get the parameters, the response contract and the price.

```bash
curl https://api.fastcrw.com/v2/scrape \
  -H "Authorization: Bearer $FASTCRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"pergamon":[{"provider":"fastcrw","capability":"find-tools","options":{
       "level":"tools","providers":["weathergov"],"capabilities":["alerts"],
       "expand":["options","response","examples"]}}]}'
```

A **scrape** with `domainTools` returns the fuller item — `id`, `name`,
`creditsCost`, `perRecord`, `next`, plus `matchedBy` and `matchedUrls`. The two
surfaces differ because theirs do.

`sources: ["pergamon"]` on its own returns tools only, costs **no credits**, and
never runs a web search. Discovery is free here as it is everywhere else on this
surface.

`alexandria` is accepted as a synonym for `pergamon`, so a client written against
Firecrawl's surface works unchanged.

### Matching tools to a website

Domain matching is on by default when `sources` names tools; `domainTools: false`
turns it off. It also works on its own, on a search or on an ordinary URL scrape:

```bash
curl https://api.fastcrw.com/v2/scrape \
  -H "Authorization: Bearer $FASTCRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url":"https://www.usgs.gov","domainTools":true,"formats":["markdown"]}'
```

`data.tools` arrives beside `data.markdown`. Matching is a domain lookup, so it
is free and adds no upstream call; the page scrape bills exactly as it would
without it.

Not every provider can be matched this way. Where a portal is hosted on a shared
platform the API host names the platform, not the agency — 90 of our providers
answer on one vendor's domain — and those are deliberately left unmatched. A
missing match is better than a confident wrong one.

:::note
`data.tools` is always present when you asked for tools, even when it is empty. If
the tool leg itself fails, the response still carries your web results and a
top-level `warning` says what went wrong. A search you paid for is never lost to
a free leg beside it.
:::

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
