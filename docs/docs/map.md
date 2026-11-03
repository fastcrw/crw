<div class="page-intro">
  <div class="page-kicker">More API</div>
  <h1>Map</h1>
  <p class="page-subtitle">Discover the URLs a site exposes before you scrape or crawl it. Map is the lightest way to understand scope without paying the cost of a full multi-page extraction job.</p>
  <div class="page-capabilities">
    <div class="page-capability"><strong>Best for:</strong> site discovery</div>
    <div class="page-capability"><strong>Returns:</strong> links only</div>
    <div class="page-capability"><strong>Start with:</strong> sitemap on, low depth</div>
  </div>
  <div class="page-actions">
    <a class="page-btn primary" href="#crawling">View Crawl</a>
    <a class="page-btn secondary" href="#scraping">View Scrape</a>
  </div>
</div>

<div class="playground-panel">
  <div class="playground-kicker">Try it in the Playground</div>
  <div class="playground-title">Inspect reachability before you recurse</div>
  <div class="playground-copy">Use one root URL, keep <code>maxDepth</code> small, and inspect the discovered links. If the map is wrong, the crawl will be wrong too.</div>
  <div class="playground-actions">
    <a class="page-btn primary" href="https://fastcrw.com/playground" target="_blank" rel="noopener">Open Playground</a>
    <a class="page-btn secondary" href="#crawling">Jump to Crawl</a>
  </div>
</div>

**Try it without a key:** run this exact request in the <a href="https://fastcrw.com/playground?url=https%3A%2F%2Fexample.com&mode=map" target="_blank" rel="noopener">playground</a> (rate-limited preview, no signup).

## Mapping a site with CRW

### /v1/map

```http
POST /v1/map
```

Authentication:

- Hosted: send `Authorization: Bearer YOUR_API_KEY`
- Self-hosted: only required when `auth.api_keys` is configured

### Installation

Map is also a plain HTTP route. No dedicated SDK is required.

### Basic usage

Start with this request:

```json
{
  "url": "https://example.com",
  "maxDepth": 1,
  "useSitemap": true
}
```

:::tabs
::tab{title="Python"}
```python
import requests

resp = requests.post(
    "https://api.fastcrw.com/v1/map",
    headers={"Authorization": "Bearer YOUR_API_KEY"},
    json={
        "url": "https://example.com",
        "maxDepth": 1,
        "useSitemap": True,
    },
)

print(resp.json()["data"]["links"])
```
::tab{title="Node.js"}
```javascript
const resp = await fetch("https://api.fastcrw.com/v1/map", {
  method: "POST",
  headers: {
    "Authorization": "Bearer YOUR_API_KEY",
    "Content-Type": "application/json"
  },
  body: JSON.stringify({
    url: "https://example.com",
    maxDepth: 1,
    useSitemap: true
  })
});

const body = await resp.json();
console.log(body.data.links);
```
::tab{title="cURL"}
```bash
curl -X POST https://api.fastcrw.com/v1/map \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://example.com",
    "maxDepth": 1,
    "useSitemap": true
  }'
```
:::

### Response

```json
{
  "success": true,
  "data": {
    "links": [
      "https://example.com",
      "https://example.com/about"
    ],
    "sitemaps": [
      "https://example.com/sitemap.xml",
      "https://example.com/product-sitemap.xml"
    ]
  }
}
```

## Parameters

| Field | Type | Default | Description |
|---|---|---|---|
| `url` | string | required | Site or page URL to start discovery from |
| `maxDepth` | number | `2` | Maximum discovery depth |
| `useSitemap` | boolean | `true` | Read sitemap.xml hints when available |
| `crawlFallback` | boolean | `true` | After the sitemap phase, run a short-budget BFS crawl to fill gaps. Set to `false` for sitemap-only mode (faster on sites with rich sitemaps, but may miss pages the sitemap omits) |
| `filterByPath` | boolean | `true` | When the URL has a path, return only URLs under it (`/docs/api` keeps `/docs/api/*`). A root URL is never scoped. Set `false` to map the whole site from a deep URL |
| `timeout` | number | `120` | Custom timeout in seconds |
| `ignoreQueryParameters` | boolean | `null` | Coarse filter switch. `true` strips every non-preserved query param; `false` disables all URL filtering (raw URLs) |
| `stripTrackingParams` | boolean | `null` | Strip known tracking query params (UTM, fbclid, etc.) from discovered URLs. `null` uses the server default |
| `dropActionUrls` | boolean | `null` | Drop action/mutation URLs (add-to-cart, checkout, logout, etc.) from results entirely. `null` uses the server default |
| `extraTrackingParams` | string[] | `—` | Additional query param names to treat as tracking (stripped by Tier B). Additive on top of the built-in list. Max 64 entries; keys are normalised (lowercase, `-` → `_`) |
| `extraActionParams` | string[] | `—` | Additional query param names to treat as action params (dropped by Tier A). Additive on top of the built-in list. Max 64 entries; keys are normalised |
| `preserveParams` | string[] | `—` | Query param names to always keep, even when tracking/action filters are active. Additive on top of the built-in preserve list. Max 64 entries; keys are normalised |

## Sitemap behavior

With `useSitemap: true`, CRW uses sitemap hints when they are available. That usually makes the first discovery pass faster and more complete on structured sites.

Good default:

- keep sitemap on,
- keep depth low,
- inspect the discovered links,
- then decide whether crawl is worth it.

The response also carries `sitemaps`: every sitemap document CRW actually
fetched and parsed during discovery. That covers the ones declared in
`robots.txt`, the well-known fallback paths (`/sitemap.xml`,
`/sitemap_index.xml`, `/sitemap-index.xml`, `/wp-sitemap.xml`) and every nested
child of a sitemap index. Paths that answered 404 or returned nothing
parseable are not listed, and a valid sitemap that currently lists no URLs
still counts. Sitemap files stay out of `links` because they are not pages. The
list is empty when `useSitemap` is `false`, and partial when discovery stops
early on `limit`, `timeout`, or the internal sitemap budget.

## When map is better than crawl

Use map when:

- you need to understand a site's shape before extracting any content,
- you want a cheap first pass over a large site,
- or you are deciding which section is worth crawling.

Use crawl only after you already trust the section you want to recurse through.

## Common production patterns

- Run map before crawl when you do not yet trust the start URL scope.
- Keep `maxDepth` low first so you can inspect the discovered section.
- Use sitemap hints when you want a faster first pass over structured sites.

## Common mistakes

- Using map when you already know the exact page and only need its content
- Expecting map to return page bodies; it only returns discovered links
- Letting depth grow before inspecting whether the discovered section is useful

## When to use something else

- Use [Scrape](#scraping) when you want the content of a known page
- Use [Crawl](#crawling) when you are ready to recurse through a bounded section
- Use [Search](#search) when you do not know the site or page set yet
