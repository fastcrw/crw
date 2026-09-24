---
name: crw-research
description: |
  Find ALL the arXiv papers that answer a research question, using fastCRW's
  Firecrawl-compatible Research API. Use when the ask is to survey a literature,
  enumerate papers on a topic, find what a paper compares against or builds on,
  list the best models on a benchmark, or recover a paper from a vague
  description — "papers that do X", "what does X benchmark against", "best
  open model on Y", "find the paper that ...". Reaches 61.0% recall on the
  ArXivQA benchmark vs Firecrawl's Research Index 53.3%.
license: AGPL-3.0
metadata:
  author: us
  version: "0.2.0"
  homepage: https://fastcrw.com
  repository: https://github.com/fastcrw/crw
allowed-tools: Bash(curl:*) Bash(jq:*) Read
---

# fastCRW Research

Find EVERY arXiv paper that answers a research query. Recall = union of arXiv
ids; extra ids never hurt, so cast wide but on-topic. The
[Research API](https://docs.fastcrw.com/research-api) is a live, drop-in
Firecrawl-research-compatible surface — your job is **query strategy + intent
routing**, the endpoints do the retrieval.

This skill reaches 61.0% recall on the 191-question ArXivQA benchmark, a number
fastCRW measured directly. Firecrawl publishes 53.3% for its own Research Index
on the same benchmark in their [Research Index launch
post](https://www.firecrawl.dev/blog/research-index-launch) (2026-06-17): "On
arXivQA, the index hits 53.3% recall at $0.32 per task, against 45.4% for the
next best provider."

Set `FASTCRW_API_KEY` (a `crw_live_…` key from https://fastcrw.com/dashboard).
Base URL `https://api.fastcrw.com`. Every endpoint is a GET; pull arXiv ids out
of `results[].ids.arxiv` / `results[].primaryId`.

```bash
# search: ranked papers for one query
curl -s -H "Authorization: Bearer $FASTCRW_API_KEY" \
  "https://api.fastcrw.com/v1/search/research/papers?query=$(jq -rn --arg q "QUERY" '$q|@uri')&k=40"

# references / citers / similar of a seed paper (citation graph)
curl -s -H "Authorization: Bearer $FASTCRW_API_KEY" \
  "https://api.fastcrw.com/v1/search/research/papers/arxiv:1706.03762/similar?intent=related%20work&mode=references&k=40"
```

## The whole game: classify the query, apply the matching method

**A) ALWAYS (base):** write 8–12 **exact-name** queries — specific method, model,
dataset, and benchmark NAMES, not broad phrases ("MoleculeNet benchmark",
"Uni-Mol", "ChemBERTa", not "molecular embeddings"). Call `search` on each,
union the arXiv ids, rank by how many queries surfaced each id. Exact-name
decomposition is the #1 recall lever — one broad query misses the niche papers.
Then **expand**: take the 5 ids the most queries agreed on and pull
`/papers/arxiv:<id>/similar?mode=references` for each, appending any new ids
below the union. This second pass is not optional and is not the same as (B):
it runs on EVERY query type, and it is where the papers that no query names
directly come from.

**B) COMPARE-AGAINST** ("what does X compare to / build on / baseline against")
→ resolve X to its arXiv id, then `/papers/arxiv:<X>/similar?mode=references`.
The answer lives in X's own bibliography.

**C) USING / EXTENDING X** ("models that USE/adopt X") →
`/similar?mode=citers` (forward citations) + exact-name searches for known
adopters.

**D) BEST-ON-BENCHMARK** ("which models score best on X", "largest open model")
→ search the leaderboard, read the OPEN model names
(DeepSeek/Qwen/GLM/Kimi/MiniMax/Llama/Mistral/Gemma — ignore Claude/GPT/Gemini,
no papers), then `search "<model family> technical report"` for each.

**E) NICHE ENUMERATION** ("papers that do X") → exact-name queries (A) are
primary. A tight survey or awesome-list, when on-topic, adds its ids.

## Rules

- Recent ids (25xx / 26xx) are REAL — keep them, never discard as "future-dated".
- A query that sounds specific usually still has a *family* of papers — surface
  the family, don't stop at one. Only a query naming a paper by title is single.
- Merge ALL ids from every step; method-targeted (references/leaderboard) and
  exact-name hits first, broad-search tail after. Never invent ids.
