# What I Learned Benchmarking CRW Against Firecrawl and Crawl4AI

> How we benchmark CRW against Firecrawl and Crawl4AI — methodology, dataset breakdown, what the metrics mean, and a one-command reproducible script you can run against your own URLs.

**Published:** 2026-03-11  
**Updated:** 2026-05-23  
**Canonical:** https://fastcrw.com/blog/benchmark-crw

---

## Why I Ran This Benchmark

When I started building CRW, I needed to understand where it actually stood relative to established tools. Not to "win" a benchmark — that's a useless goal — but to understand which workloads it handles well and where it falls short. Honest benchmarks shape better product decisions.

This post shares what we observed, how we measured, and what the numbers actually mean in practice. I've also included the scripts we used so you can run your own version against your own target URLs.

## What We're Measuring and Why

Before looking at numbers, it's worth being precise about what the metrics mean.

### Latency Percentiles: p50, p95, Mean

**p50 (median):** The latency at which 50% of requests completed faster. This is the "typical" experience. It's more robust than mean because it ignores extreme outliers.

**p95:** The latency at which 95% of requests completed faster. This captures tail latency — the slow cases that happen regularly enough to matter in production. A high p95 means roughly 1 in 20 requests is meaningfully slower than the median, which is exactly the kind of variance that hurts interactive use.

**Mean:** The arithmetic average. Useful for cost calculations (total time / total requests) but can be misleading when outliers skew the distribution.

We report all three because they tell different stories. A tool with great p50 but terrible p95 might be fine for batch processing but unacceptable for interactive use. A tool with similar p50 and p95 has more predictable behavior.

### Wall-Clock Time

We measured wall-clock time: the elapsed real time from sending the HTTP request to receiving the complete response body. This includes:

- DNS resolution
- TCP connection establishment
- TLS handshake
- Server-side processing (fetch, parse, convert)
- Network transfer of the response

We chose wall-clock over CPU time because wall-clock reflects what users actually experience. A tool that's CPU-efficient but has high network overhead still feels slow.

### Coverage: What It Precisely Means

Coverage = (URLs returning non-empty, parseable content) / (total URLs attempted) × 100.

A URL "passes" coverage if: the response has HTTP 200, the response body contains at least 100 characters of text, and the text is parseable (not garbled encoding, not just HTML boilerplate). A URL "fails" if: it times out, returns 4xx/5xx, returns an empty body, or returns only whitespace/navigation elements.

Coverage is a rough measure of practical usefulness — a result that technically returns 200 but contains only a JavaScript loading spinner isn't useful.

## Dataset Composition

We used 500 URLs sampled from Scrapeway's public benchmark dataset with adjustments to match our expected production workload distribution.

### Breakdown by Site Type

| Category | Count | % of corpus | JS required |
| --- | --- | --- | --- |
| Documentation/technical blogs | 150 | 30% | ~10% |
| News articles | 125 | 25% | ~15% |
| E-commerce product pages | 100 | 20% | ~40% |
| Company/SaaS marketing pages | 75 | 15% | ~50% |
| Wikipedia / encyclopedia pages | 50 | 10% | <5% |

Roughly 25–30% of URLs in the corpus required JavaScript execution for meaningful content retrieval. The rest were static HTML or server-rendered pages. This ratio is intentional — it mirrors the distribution we see in real RAG pipeline workloads.

### Why Dataset Composition Matters for Interpretation

A benchmark corpus biased toward SPAs would heavily favor Playwright-based tools (Firecrawl, Crawl4AI). A corpus biased toward static HTML would favor lightweight tools like CRW. Our corpus reflects a mixed workload — which is honest for most real-world use cases but means results shouldn't be extrapolated to all-SPA or all-static scenarios.

## Benchmark Setup

**Environment:** All tools ran in Docker containers on the same hardware: 4 vCPU (AMD EPYC), 8 GB RAM, Ubuntu 22.04. Same network, same source IPs, same DNS resolver.

**Test mode:** Sequential (not parallel) to isolate per-request latency. Parallel throughput is a different measurement covered in the Throughput section below.

**Repetitions:** Each URL was scraped 3 times; we took the median of the 3 runs to reduce measurement noise from transient network conditions.

**Warmup:** All services were given a 2-minute warmup period (10 warmup requests) before timed runs, to ensure connection pools were populated and caches warm.

## Benchmark Setup Scripts

Here's the core benchmarking script we used. You can run a similar test against your own URL list:

```
#!/usr/bin/env python3
# benchmark.py — run against any Firecrawl-compatible API

TOOLS = {
    "crw":       "http://localhost:3000",
    "firecrawl": "http://localhost:3001",
}

def scrape_url(base_url: str, url: str, api_key: str = "test") -> tuple[float, bool]:
    start = time.perf_counter()
    try:
        r = httpx.post(
            f"{base_url}/v1/scrape",
            json={"url": url, "formats": ["markdown"]},
            headers={"Authorization": f"Bearer {api_key}"},
            timeout=30.0,
        )
        elapsed = time.perf_counter() - start
        ok = r.status_code == 200 and len(r.json().get("data", {}).get("markdown", "")) > 100
        return elapsed, ok
    except Exception:
        return time.perf_counter() - start, False

def percentile(data: list[float], p: int) -> float:
    data.sort()
    k = (len(data) - 1) * p / 100
    f = int(k)
    c = f + 1
    return data[f] + (data[c] - data[f]) * (k - f) if c < len(data) else data[f]

urls = [line.strip() for line in open(sys.argv[1]) if line.strip()]

for name, base in TOOLS.items():
    latencies, successes = [], 0
    for url in urls:
        elapsed, ok = scrape_url(base, url)
        latencies.append(elapsed * 1000)  # ms
        if ok:
            successes += 1
        time.sleep(0.1)  # polite delay

    print(f"
{name}:")
    print(f"  p50:      {percentile(latencies, 50):.0f} ms")
    print(f"  p95:      {percentile(latencies, 95):.0f} ms")
    print(f"  mean:     {statistics.mean(latencies):.0f} ms")
    print(f"  coverage: {successes}/{len(urls)} ({100*successes/len(urls):.1f}%)")
```

Run it with a text file of URLs (one per line):

```
python3 benchmark.py urls.txt
```

## Latency Results

Rather than freeze a single point-in-time latency table here — numbers that drift with every release of every tool — we publish the full latency distribution (p50/p95/mean, per tool, per run) alongside the exact dataset on our public [/benchmarks](https://fastcrw.com/benchmarks) page, with a one-command repro so you can regenerate it yourself.

The durable, defensible finding from that run: **63.74% truth-recall (522 of 819 labeled URLs), ~92% scrape success of reachable URLs, 0 errors**. CRW's Rust implementation is lower-latency than the Node.js and Python-based alternatives on standard HTML content because there's no headless-browser process in the hot path. The gap narrows on JavaScript-heavy pages — when a browser render is required, rendering time dominates regardless of the wrapper language.

The tail behavior is what matters most for interactive use: CRW's p95 stays close to its median, so occasional slowness is rare. Browser-render-first tools show a much wider p50→p95 spread, which is visible to users in latency-sensitive applications.

## Crawl Coverage Results

On the labeled public dataset, CRW reached **91.8% scrape success of reachable URLs with 0 errors**, and a truth-recall of 63.74% (522 of 819 labeled URLs). The per-tool, per-category coverage breakdown — including timeout vs. empty-body failure modes — is published with the dataset on [/benchmarks](https://fastcrw.com/benchmarks) so it stays current as every tool evolves.

Coverage surprised us. We expected a browser-render-first stack to perform better here. In our dataset, lol-html's aggressive streaming parser handled malformed HTML more gracefully than a full rendering pipeline — which occasionally timed out or returned empty responses for slow-loading pages.

Browser-render-first tools tend to have a higher timeout rate, which is largely a function of headless Chromium taking longer per page under a stricter timeout budget. When pages don't load within the timeout window, the request fails completely.

## Memory Usage

The structural memory difference is the durable point, not a single benchmark figure. CRW is a single static binary with no headless browser in its default path, so its resident footprint is a small fraction of a browser-render-first stack — and, critically, it has no large unreclaimable baseline. Browser-render-first tools carry a heavy idle baseline (the headless engine's private heap) that cannot be reclaimed regardless of traffic, and they grow further under load as renderer processes spawn.

## Memory Profiling Details

We measure memory using two tools: `docker stats` for RSS (Resident Set Size) and `pmap -x` for heap breakdown. "Idle" is measured after a 60-second warmup with zero active requests. "Under load" is measured at peak during a 50-concurrent-request burst sustained for 30 seconds. The full per-tool memory table is published with the rest of the run on [/benchmarks](https://fastcrw.com/benchmarks).

CRW's memory profile is dominated by connection buffers, parse state, and response buffers, plus the static binary's own code/data and shared libs — there is no browser heap. A browser-render-first tool's profile has a fundamentally different shape: a large share of its idle footprint is the headless engine's private heap, which can't be reclaimed regardless of traffic, and under load it spawns additional renderer processes that each add a substantial increment.

## JavaScript-Heavy Pages: Separate Analysis

We isolate the subset of corpus URLs that require JavaScript execution for meaningful content (SPAs, lazy-loaded articles, client-rendered product pages) and report it separately on [/benchmarks](https://fastcrw.com/benchmarks), because mixing it into the headline number would misrepresent both workloads.

For JavaScript-heavy pages, CRW's latency advantage largely disappears — rendering time dominates — and its coverage is lower on this subset than its overall figure. LightPanda is still maturing and doesn't yet implement the full browser API surface that Playwright (Chromium) covers.

The honest takeaway: if your workload is predominantly SPAs, Crawl4AI or Firecrawl's Playwright-based rendering gives better coverage today. CRW is a better fit for HTML-primary content.

## Throughput vs. Latency: Different Workloads

The latency table above measures sequential requests — one at a time, measuring per-request duration. This is the right metric for interactive use cases where a user is waiting for a single result.

For batch pipelines, parallel throughput is what matters: how many pages can you process per second when running many requests concurrently?

Because CRW has no per-request browser process, parallel throughput scales with available CPU and connection limits rather than with renderer memory. Browser-render-first tools become memory-constrained at high concurrency — renderer processes are the bottleneck — so their pages/sec plateaus much earlier on the same hardware. The full pages/sec-by-worker-count table is published with the run on [/benchmarks](https://fastcrw.com/benchmarks).

Note that throughput measurements are system-dependent. On a machine with more RAM, a browser-render-first tool's numbers improve. On a memory-constrained server, CRW maintains its throughput while browser-based stacks degrade faster.

## How to Run Your Own Benchmark

The most meaningful benchmark is one run against your own target URLs. Here's a complete self-contained script:

```
#!/bin/bash
# run_benchmark.sh — requires Docker, Python 3, httpx
# Usage: ./run_benchmark.sh your_urls.txt

set -e
export URLS_FILE=${1:-urls.txt}

echo "Starting CRW..."
docker run -d --name bench-crw -p 3002:3000   -e CRW_API_KEY=test ghcr.io/fastcrw/crw:latest

echo "Starting Firecrawl (requires docker compose)..."
echo "See https://github.com/mendableai/firecrawl for self-host setup"
echo "Firecrawl needs Redis + workers — single docker run won't work."
echo "Assuming Firecrawl is already running on port 3001."

sleep 5  # wait for CRW to be ready

echo "Running benchmark..."
python3 - <<'PYEOF'

TOOLS = {
    "crw":       ("http://localhost:3000", "test"),
    "firecrawl": ("http://localhost:3001", "test"),
}

def scrape(base, key, url):
    start = time.perf_counter()
    try:
        r = httpx.post(f"{base}/v1/scrape",
            json={"url": url, "formats": ["markdown"]},
            headers={"Authorization": f"Bearer {key}"},
            timeout=30.0)
        ms = (time.perf_counter() - start) * 1000
        ok = r.status_code == 200 and len(r.json().get("data",{}).get("markdown","")) > 100
        return ms, ok
    except Exception:
        return (time.perf_counter() - start) * 1000, False

urls_file = os.environ.get("URLS_FILE", "urls.txt")
with open(urls_file) as f:
    urls = [l.strip() for l in f if l.strip()][:100]

for name, (base, key) in TOOLS.items():
    lats, hits = [], 0
    for u in urls:
        ms, ok = scrape(base, key, u)
        lats.append(ms)
        hits += ok
        time.sleep(0.05)
    lats.sort()
    p = lambda p: lats[int(len(lats)*p/100)]
    print(f"
{name}: p50={p(50):.0f}ms p95={p(95):.0f}ms mean={sum(lats)/len(lats):.0f}ms coverage={hits}/{len(urls)}")
PYEOF

echo "Stopping CRW container..."
docker rm -f bench-crw
```

## What Changed Since We First Ran This

Benchmarks are point-in-time snapshots. Our first run was in late 2025; the results above reflect early 2026.

Changes since the first run:

- **CRW p50 improved** — primarily from reqwest connection pool tuning and lol-html selector optimization
- **Firecrawl coverage improved** — Firecrawl v1.5 added better timeout handling; its coverage was lower in our original test
- **Crawl4AI added async mode** — their batch throughput improved significantly with async browser pooling

These results will continue to change as all tools evolve. If you're making a significant infrastructure decision based on performance, run your own test against your actual workload. We try to re-run our benchmark with each major release.

## Where the Results Surprised Us

**Coverage was higher than expected.** We anticipated CRW's simpler HTML parser to miss content a full browser would catch. For standard HTML pages, lol-html's streaming approach actually handled malformed HTML more reliably than headless Chrome, which hit rendering timeouts more often.

**Firecrawl's latency was higher than remembered from hosted API tests.** Self-hosted Firecrawl performs differently than the hosted API, which uses proxy routing and optimized infrastructure. Don't conflate hosted-API benchmarks with self-hosted ones.

## What These Numbers Mean in Practice

The practical implication of a lower-latency, no-browser-in-the-hot-path design is simple: a large sequential scrape job finishes in a fraction of the wall-clock time of a browser-render-first stack, and at high concurrency the gap widens further because CRW isn't memory-bound by renderer processes. Run the one-command repro on [/benchmarks](https://fastcrw.com/benchmarks) against your own URL list to see the exact wall-clock numbers for your workload.

For memory budgets, the difference is structural: you can pack many CRW instances onto a small server because each is a lightweight static binary, whereas the same number of browser-render-first instances needs a far larger machine just for the headless-engine baseline.

## Limitations of This Benchmark

- **Anti-bot performance:** We only tested publicly accessible pages. For CAPTCHA-protected or fingerprint-checking targets, results differ substantially.
- **SPA coverage:** Our corpus was biased toward HTML-heavy content. An all-SPA corpus would show different rankings.
- **Content quality:** We measured whether content was returned, not whether it was clean. Qualitative comparison is harder.
- **Hosted vs. self-hosted:** We tested self-hosted versions. The fastCRW hosted API and Firecrawl's hosted API have different latency profiles.

## Try It Yourself

Self-host CRW and run your own benchmark:

```
docker run -p 3000:3000 -e CRW_API_KEY=your-key ghcr.io/fastcrw/crw:latest
```

Or use [fastCRW](https://fastcrw.com) — the managed version with a one-time lifetime 1000 credits (not a monthly meter), no credit card required.

## FAQ

### What did the 3-way scrape benchmark actually find?

On Firecrawl's public scrape-content-dataset-v1 (1,000 URLs, 819 with labeled ground truth, harness diagnose_3way.py, run 2026-05-08), CRW reached the highest truth-recall of the three tools at 63.74% (522 of 819 labeled URLs), ahead of Crawl4AI at 59.95% (491) and Firecrawl at 56.04% (459). CRW also recorded 91.8% scrape success of reachable URLs with 0 thrown errors across 3,000 requests — and recovers 34 URLs the other two tools both miss, 70% more unique recoveries than crawl4ai (10) and Firecrawl (10) combined. It is the durable, defensible finding from that run.

### How does CRW's latency compare on standard HTML pages?

On standard HTML pages, CRW is consistently lower-latency than browser-render-first tools because there is no headless browser in the request path — its p50 of 1914 ms beats Firecrawl's 2305 ms and is effectively tied with Crawl4AI's 1916 ms. On JavaScript-heavy pages that require full browser rendering, the gap narrows because render time dominates every tool. For mixed workloads, CRW favors teams prioritizing latency and throughput over SPA coverage.

### Does CRW perform better on all pages?

No. CRW performs best on HTML-primary content such as news articles, documentation, blog posts, and server-rendered pages. On JavaScript-heavy SPAs, CRW's LightPanda integration is functional but less complete than Playwright-based tools, so its coverage on that subset is lower than its overall figure. The isolated JS-subset breakdown is published on /benchmarks.

### How accurate are these benchmarks?

They are directionally accurate for standard HTML workloads but should be treated with caution for all-SPA or all-protected-site scenarios. Benchmarks are point-in-time and tool versions matter, so we try to re-run with each major CRW release. The most accurate benchmark is always one you run yourself against your own target URLs.

### Can I reproduce these results myself?

Yes. The benchmark setup script in this post runs CRW and any Firecrawl-compatible API against your own URL list — provide a plain-text file of URLs and the script handles spinning up containers, running tests, and reporting results. The full latency distribution and a one-command repro are also published alongside the exact dataset on the public /benchmarks page. Differences in your results are expected based on your network, target URLs, and server hardware.
