# Benchmarks

3-way scrape benchmark on [Firecrawl's own public 1,000-URL dataset](https://huggingface.co/datasets/firecrawl/scrape-content-dataset-v1)
(`diagnose_3way.py`, 2026-05-08, concurrency 5, timeout 120s, **recall mode** unless noted).

| Metric | **fastCRW** | crawl4ai | Firecrawl |
|---|---|---|---|
| **Truth-recall** (522/819 labeled URLs) | **63.74%** | 59.95% | 56.04% |
| p50 latency | **1914 ms** | 1916 ms | 2305 ms |
| Thrown errors (3,000 requests) | 0 | 0 | 0 |
| Scrape success (fastCRW, reachable URLs) | **877/921 (95.2%)** | not reframed | not reframed |

fastCRW leads on the accuracy metric that matters for agents: **truth-recall (63.74%, +3.79pp over
crawl4ai, +7.7pp over Firecrawl)**, and it uniquely recovers **34 URLs the other two miss** (70% more
than crawl4ai and Firecrawl combined). Its **p50 latency is the fastest of the three** (a statistical
tie with crawl4ai, ahead of Firecrawl), across **3,000 requests with 0 thrown errors and an 877/921
(95.2%) scrape success rate on reachable URLs**. The 63.74% denominator is 819 labeled/matchable URLs.

**Two modes, one config toggle.** *Recall mode* (the full renderer ladder, the numbers above)
maximizes truth-recall. *Fast mode* (LightPanda-only, no Chrome tier) trades the recall tail for
lower latency. Same binary, same API; pick accuracy or latency per workload.

## How the two most-cited alternatives compare

| | **fastCRW** | Firecrawl | Crawl4AI |
|---|---|---|---|
| Language | Rust | Node.js + Playwright | Python + Playwright |
| License | AGPL-3.0 (commercial avail.) | AGPL-3.0 (commercial avail.) | Apache-2.0 |
| Self-host download | Single binary (~10 MB) | Multi-container (~500 MB+) | ~2 GB (browser bundled) |
| Memory baseline (idle) | ~14 MB | Large (Chromium heap) | Large (Chromium heap) |
| Firecrawl migration | Yes, `/firecrawl/v2/*` compat layer | Native | No |
| MCP server | Built-in (`crw-mcp`) | Separate package | Community add-on |
| Hosted option | `api.fastcrw.com` | firecrawl.dev | None official |
| Reproducible public benchmark | Yes | Vendor-published only | Vendor-published only |

fastCRW's idle memory baseline was measured with `docker top <container> -eo rss`, 15 seconds
after `docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest` (crw v0.30.0). `docker stats` under-reports
at this scale and should not be used for this measurement.

## Reproduce it yourself

The canonical harness is `bench/diagnose_3way.py`. It matches truth text against
`md + strip_md_links(md)`, applied identically to all three tools (a fairness control). It runs
crw locally; the competitor steps below assume you have Crawl4AI and Firecrawl running locally
too (adjust the paths/containers to your setup; they reflect ours).

```bash
cd ~/coding/crw/crw-opencore
docker compose -f docker-compose.yml -f docker-compose.override.yml \
               -f docker-compose.stealth.yml --profile stealth up -d
docker start crawl4ai-bench
cd ~/coding/crw/competitors/firecrawl && docker compose up -d

cd ~/coding/crw/crw-opencore
uv run python bench/diagnose_3way.py \
  --max-urls 1000 --tools crw,crawl4ai,firecrawl \
  --concurrency 5 --timeout 120 \
  --out bench/server-runs/diag3w-1000-full.jsonl
```

Full result of record: [`bench/server-runs/RESULT_3WAY_1000_FULL.md`](bench/server-runs/RESULT_3WAY_1000_FULL.md).
Live dashboard: [fastcrw.com/benchmarks](https://fastcrw.com/benchmarks).
