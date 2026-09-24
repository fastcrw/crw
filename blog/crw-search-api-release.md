# Introducing Search: Find, Scrape, and Extract in One API Call

> CRW now includes a search endpoint. Search the web, get structured results, and optionally scrape every result page — all in a single API call.

**Published:** 2026-04-03  
**Updated:** 2026-04-03  
**Canonical:** https://fastcrw.com/blog/crw-search-api-release

---

Every AI agent that interacts with real-time information hits the same wall: you need to *find* the right pages before you can *read* them. Until now, that meant wiring up a search provider separately from your scraping pipeline. Today, CRW closes that gap.

`POST /v1/search` is live on [fastcrw.com](https://fastcrw.com) and available in every SDK and integration we ship.

## Why Search Belongs in a Scraping API

The typical agent workflow looks like this:

1. Search the web for a query
2. Pick the most relevant URLs
3. Scrape each URL for clean content
4. Feed the content to an LLM

Steps 1 and 3 used to require different APIs, different auth, different error handling. With CRW's search endpoint, you collapse steps 1–3 into a single call. Pass `scrapeOptions` and CRW will search *and* scrape in one request:

```
curl -X POST https://api.fastcrw.com/v1/search \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "machine learning papers 2026",
    "limit": 5,
    "scrapeOptions": {
      "formats": ["markdown"]
    }
  }'
```

Each result comes back with the usual search metadata — title, URL, description, score — plus the full scraped markdown if you asked for it. One request, one API key, one billing model.

## How It Works

CRW's search engine aggregates results from multiple sources, ranks them by relevance, and normalizes everything into a consistent schema.

The pipeline:

1. Your query hits CRW's `/v1/search` endpoint
2. CRW validates the request and checks your credits
3. The search engine queries multiple upstream sources and returns ranked results
4. CRW normalizes results into a consistent schema (position, score, category, publishedDate)
5. If `scrapeOptions` is set, CRW scrapes each result URL through the same engine used by `/v1/scrape`
6. Combined results are returned in one response

## Parameters

| Parameter | Type | Description |
| --- | --- | --- |
| `query` | string | Search query (required) |
| `limit` | number | Max results, 1–20 (default: 5) |
| `lang` | string | Language code (e.g. `en`, `tr`, `de`) |
| `tbs` | string | Time filter: `qdr:h`, `qdr:d`, `qdr:w`, `qdr:m`, `qdr:y` |
| `sources` | string[] | Result types: `web`, `news`, `images` |
| `categories` | string[] | Specialized search: `github`, `research`, `pdf` |
| `scrapeOptions` | object | If set, scrape each result URL (same options as `/v1/scrape`) |

## Credit Model

Search costs **1 credit** per request. If you include `scrapeOptions`, each result that gets scraped costs an additional **1 credit** — same as a regular `/v1/scrape` call. A search with `limit: 5` and `scrapeOptions` costs 6 credits total (1 search + 5 scrapes).

## SDK Support

Search is available in every CRW integration from day one:

### Python SDK

```
from crw import CrwClient

client = CrwClient(
    api_url="https://api.fastcrw.com",
    api_key="YOUR_KEY",
)

results = client.search("web scraping tools 2026", limit=5)
for r in results:
    print(f"{r['title']} — {r['url']}")
```

### LangChain

```
from langchain_crw import CrwLoader

loader = CrwLoader(
    query="latest AI research",
    mode="search",
    api_url="https://api.fastcrw.com",
    api_key="YOUR_KEY",
    params={"limit": 5},
)

docs = loader.load()  # Returns LangChain Documents
```

### CrewAI

```
from crewai_crw import CrwSearchWebTool

search_tool = CrwSearchWebTool(
    api_url="https://api.fastcrw.com",
    api_key="YOUR_KEY",
)

# Use as a CrewAI tool in your agents
result = search_tool._run(query="best web scraping tools 2026")
```

### TypeScript

```
const res = await fetch("https://api.fastcrw.com/v1/search", {
  method: "POST",
  headers: {
    Authorization: "Bearer YOUR_API_KEY",
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    query: "machine learning papers",
    limit: 3,
    scrapeOptions: { formats: ["markdown"] },
  }),
});

const { data } = await res.json();
```

Search is also available in the **n8n**, **OpenClaw**, and **Dify** integrations.

## Cloud-Only (For Now)

Search is a cloud feature — it's available out of the box on [fastcrw.com](https://fastcrw.com). The self-hosted CRW binary doesn't include a search backend, so `/v1/search` is only available through the cloud API or a CRW instance with search configured.

The Python SDK enforces this clearly: calling `client.search()` without `api_url` raises a `CrwError` with an explanation instead of silently failing.

## Try It

Search is available right now in the [playground](/playground) — no API key needed for basic testing. For production use, [sign up](/login) and get 1000 free credits to start.

- [Search endpoint docs](https://docs.fastcrw.com/search)
- [SDK examples](https://docs.fastcrw.com/sdk-examples)
- [Credit costs](https://docs.fastcrw.com/credit-costs)
- [GitHub (open-source core)](https://github.com/fastcrw/crw)
