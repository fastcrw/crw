# Research API

A purpose-built research API for scientific agents: search papers, inspect
metadata, read passages, expand the citation graph, and search research-related
GitHub.

On the [ArXivQA benchmark](https://fastcrw.com/benchmarks/arxivqa-research-recall)
this stack reaches **61.0% recall — ahead of Firecrawl's Research Index (53.3%)**.
It's **live**: results merge our own web and research search with live open
scholarly sources, including full-text paper search — no self-hosted paper index.

Firecrawl's 53.3% is published in their own [Research Index launch
post](https://www.firecrawl.dev/blog/research-index-launch) (2026-06-17): "On
arXivQA, the index hits 53.3% recall at $0.32 per task, against 45.4% for the
next best provider." Our 61.0% is fastCRW's own measurement on the same
191-question ArXivQA set.

## Authentication

Use a Bearer token: `Authorization: Bearer <crw_live_…>`. Get a key from the
[dashboard](https://fastcrw.com/dashboard).

## Endpoints

Both surfaces return identical shapes, served at the same `/v1` path whether you
call the cloud API or your own self-hosted engine. There are no research routes
on the `/firecrawl/v2` compatibility surface.

| Task | Path |
|------|------|
| Search papers | `GET /v1/search/research/papers` |
| Inspect metadata / read passages | `GET /v1/search/research/papers/{id}` |
| Find related papers | `GET /v1/search/research/papers/{id}/similar` |
| Search GitHub | `GET /v1/search/research/github` |

The sections below name this native `/v1` path; the curl examples hit the
cloud API (`https://fastcrw.com/api`).

## Search papers

`GET /v1/search/research/papers?query=&k=&authors=&categories=&from=&to=`

```bash
curl -s -H "Authorization: Bearer $FASTCRW_API_KEY" \
  "https://fastcrw.com/api/v1/search/research/papers?query=diffusion%20image%20synthesis&k=20"
```

Returns ranked papers. `paperId` is the canonical id (a stable work id when
known, else `arxiv:<id>`); `primaryId` is the preferred source id
(`arxiv:2105.05233`); `ids` holds the prefix-less source ids.

```json
{ "success": true, "results": [
  { "paperId": "W2105…", "primaryId": "arxiv:2105.05233",
    "ids": { "arxiv": ["2105.05233"] },
    "title": "…", "abstract": "…", "score": 0.42 }
] }
```

Filters: `authors` (substring), `categories`, `from` / `to` (`YYYY-MM-DD`).

## Inspect a paper / read passages

`GET /v1/search/research/papers/{id}` returns metadata (`authors`, `categories`,
`createdDate`, …). Accepts an arXiv id, a work id, or a DOI. Add `?query=` to
return the top passages answering a question:

```bash
curl -s -H "Authorization: Bearer $FASTCRW_API_KEY" \
  "https://fastcrw.com/api/v1/search/research/papers/arxiv:1706.03762?query=what%20is%20the%20attention%20mechanism&k=4"
```

## Find related papers

`GET /v1/search/research/papers/{id}/similar?intent=&mode=similar|citers|references&k=`

`intent` is required. `mode` selects the expansion: `similar` (recommendations),
`citers` (papers that cite the seed), `references` (papers the seed cites).

## Search GitHub

`GET /v1/search/research/github?query=&k=` returns repository/README hits for
implementation notes and engineering prior art.

## SDK

The Firecrawl research SDK works unchanged — set the base URL to
`https://api.fastcrw.com`. Or use the fastCRW SDK:

```python
from crw import CrwClient
c = CrwClient(api_key="crw_live_…")
c.search_papers("diffusion image synthesis", k=20)
c.get_paper("arxiv:1706.03762", query="what is the attention mechanism")
c.related_papers("arxiv:1706.03762", intent="efficient transformers", mode="references")
c.search_github("flash attention implementation notes")
```

```ts
import { CrwClient } from "crw-sdk";
const c = new CrwClient({ apiKey: "crw_live_…" });
await c.research.searchPapers("diffusion image synthesis", { k: 20 });
await c.research.similarPapers("arxiv:1706.03762", { intent: "efficient transformers", mode: "references" });
```

## The research skill

The endpoints are stateless primitives. The intelligence that reaches 61.0% on
ArXivQA lives in the **research skill** over these endpoints, the same way
Firecrawl splits its Research Index endpoints from its research skill. Its base
pass is two steps, and both carry recall: 8-12 exact-name query reframings
ranked by how many of them surfaced an id, and then `mode=references` on the top
5 ids to pull in the papers no query names directly. On top of that sit intent
routing, reading a leaderboard, and pulling a paper's own bibliography for
"compare-against" questions.

Measured on the first 12 benchmark questions with one raw query each, so the
expansion is the only variable: 47.0% for the search union alone, 66.5% with the
reference expansion. Skipping it is the most common way a re-implementation
lands below the published number.

Install it into your agent (Claude Code, Cursor, Codex, Gemini CLI, …):

```bash
npx skills add fastcrw/crw@crw-research
```

## Notes and limits

- **Live, no index.** Recall comes from merging our own web search (web +
  research mode) with live open scholarly sources + full-text paper search.
  Latency is seconds, not the milliseconds of a hot index.
- **Read passages** are abstract-scoped today; full arXiv-body passages are on
  the roadmap.
- **GitHub** results are repo/README-scoped today.
- Powered by open scholarly sources.
