# Build an LLM Training-Data Pipeline With CRW (2026): Crawl, Clean, Dedupe to JSONL

> Turn the web into clean fine-tuning data: crawl with CRW, strip boilerplate, quality-filter, near-dedupe with MinHash, and emit JSONL. Full runnable Python — self-host free under AGPL-3.0.

**Published:** 2026-05-24  
**Updated:** 2026-05-24  
**Canonical:** https://fastcrw.com/blog/llm-training-data-pipeline-crw

---

## What We're Building

A dataset pipeline that crawls target sites with CRW, cleans the text, drops low-quality documents, removes near-duplicates with MinHash, and writes a sharded JSONL corpus ready for pretraining or fine-tuning. Garbage web text produces garbage models — the value here is the filtering and dedupe, and CRW removing boilerplate at the source makes every later stage cheaper.

## Pipeline Stages

- **Crawl** — CRW returns onlyMainContent markdown per page
- **Clean** — strip residual markup, normalize whitespace
- **Quality filter** — length, language, symbol-ratio heuristics
- **Dedupe** — MinHash LSH for near-duplicate removal
- **Shard** — write compressed JSONL shards with provenance

## Prerequisites

- CRW running: `docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`
- Python 3.10+

```
pip install firecrawl-py datasketch
```

## Step 1: Connect and Crawl

```
from firecrawl import FirecrawlApp

app = FirecrawlApp(api_key="crw_live_YOUR-KEY", api_url="http://localhost:3000")
# fastCRW cloud: api_url="https://api.fastcrw.com"

def crawl(base_url: str, limit: int = 500) -> list[dict]:
    job = app.crawl_url(base_url, params={
        "limit": limit, "maxDepth": 4,
        "scrapeOptions": {"formats": ["markdown"], "onlyMainContent": True},
    })
    docs = []
    for p in job.get("data", []):
        md = p.get("markdown", "")
        url = p.get("metadata", {}).get("sourceURL", "")
        if md and url:
            docs.append({"url": url, "text": md})
    return docs
```

`onlyMainContent` is doing heavy lifting: it removes nav/footer/cookie text that would otherwise dominate token frequency and bias the model toward boilerplate.

## Step 2: Clean

```
import re

def clean(text: str) -> str:
    text = re.sub(r"```.*?```", " ", text, flags=re.DOTALL)  # drop code fences
    text = re.sub(r"!\[[^\]]*\]\([^)]*\)", " ", text)          # images
    text = re.sub(r"\[([^\]]+)\]\([^)]*\)", r"\1", text)        # links -> anchor text
    text = re.sub(r"[#*_>`]+", " ", text)                          # md symbols
    text = re.sub(r"\s+", " ", text)
    return text.strip()
```

## Step 3: Quality Filters

Cheap heuristics catch most junk — nav-only pages, listicles of links, encoding garbage:

```
def passes_quality(text: str) -> bool:
    if len(text) < 400:                       # too short to be a document
        return False
    words = text.split()
    if len(words) < 80:
        return False

    alpha = sum(c.isalpha() or c.isspace() for c in text) / max(len(text), 1)
    if alpha < 0.7:                           # too many symbols/markup residue
        return False

    avg_wlen = sum(len(w) for w in words) / len(words)
    if not (3 <= avg_wlen <= 12):             # gibberish / token soup
        return False

    # mostly-English heuristic via ASCII letter ratio
    ascii_letters = sum("a" <= c.lower() <= "z" for c in text)
    if ascii_letters / max(len(text), 1) < 0.5:
        return False
    return True
```

## Step 4: Near-Duplicate Removal With MinHash

The web is full of mirrored and templated pages. MinHash LSH removes near-dupes far faster than pairwise comparison:

```
from datasketch import MinHash, MinHashLSH

def shingles(text: str, k: int = 5) -> set[str]:
    toks = text.lower().split()
    return {" ".join(toks[i:i + k]) for i in range(len(toks) - k + 1)}

def dedupe(docs: list[dict], threshold: float = 0.8) -> list[dict]:
    lsh = MinHashLSH(threshold=threshold, num_perm=128)
    kept = []
    for i, d in enumerate(docs):
        m = MinHash(num_perm=128)
        for sh in shingles(d["text"]):
            m.update(sh.encode())
        if lsh.query(m):           # a near-duplicate already kept
            continue
        lsh.insert(f"doc-{i}", m)
        kept.append(d)
    print(f"dedupe: {len(docs)} -> {len(kept)}")
    return kept
```

## Step 5: Write Sharded JSONL

```
import json, gzip, pathlib, hashlib
from datetime import datetime, timezone

def write_shards(docs: list[dict], out_dir: str, shard_size: int = 1000):
    root = pathlib.Path(out_dir)
    root.mkdir(parents=True, exist_ok=True)
    shard, idx, count = [], 0, 0

    def flush(buf, n):
        path = root / f"shard-{n:05d}.jsonl.gz"
        with gzip.open(path, "wt", encoding="utf-8") as f:
            for rec in buf:
                f.write(json.dumps(rec, ensure_ascii=False) + "\n")
        print(f"wrote {len(buf)} records -> {path}")

    for d in docs:
        rec = {
            "text": d["text"],
            "meta": {
                "source_url": d["url"],
                "id": hashlib.sha256(d["text"].encode()).hexdigest()[:24],
                "collected_at": datetime.now(timezone.utc).isoformat(),
            },
        }
        shard.append(rec)
        count += 1
        if len(shard) >= shard_size:
            flush(shard, idx)
            shard, idx = [], idx + 1
    if shard:
        flush(shard, idx)
    print(f"total kept records: {count}")
```

## Step 6: Run the Pipeline

```
def build_dataset(seeds: list[str], out_dir: str = "corpus"):
    raw = []
    for s in seeds:
        raw.extend(crawl(s, limit=300))
    print(f"crawled {len(raw)} raw docs")

    cleaned = []
    for d in raw:
        t = clean(d["text"])
        if passes_quality(t):
            cleaned.append({"url": d["url"], "text": t})
    print(f"after quality filter: {len(cleaned)}")

    deduped = dedupe(cleaned)
    write_shards(deduped, out_dir)

if __name__ == "__main__":
    build_dataset([
        "https://docs.example.com",
        "https://blog.example.com",
    ])
```

## Why Data Quality Dominates Model Quality

It is now well established across the literature and practitioner experience that, past a baseline, dataset quality moves model performance more than marginal architecture or hyperparameter changes. The expensive failure mode is not too little data — it is a corpus full of templated boilerplate, near-duplicate mirrors, and machine-generated junk that the model dutifully learns to reproduce. Every stage in this pipeline exists to attack that. Boilerplate is removed at ingestion by CRW's `onlyMainContent`, which is strictly better than stripping it later because the noise never enters the token statistics in the first place. The quality filters remove documents that are technically text but carry no signal. MinHash dedupe removes the redundancy that would otherwise cause the model to over-memorize whatever gets mirrored most. The ordering matters: clean before you filter (so filters judge real content), filter before you dedupe (so you do not waste dedupe work on junk), and dedupe before you shard (so shard sizes reflect the final corpus).

## Tuning the Filters Without Flying Blind

Hard-coded thresholds are a starting point, not an answer. The right values depend on your domain — a corpus of API documentation has a different symbol ratio and average word length than literary prose, and the same filter that cleans one will decimate the other. Instrument the pipeline so you can see what each filter rejects before you trust it:

```
from collections import Counter

def diagnose(raw_docs: list[dict]):
    reasons = Counter()
    for d in raw_docs:
        t = clean(d["text"])
        if len(t) < 400:
            reasons["too_short"] += 1
        elif len(t.split()) < 80:
            reasons["too_few_words"] += 1
        elif sum(c.isalpha() or c.isspace() for c in t) / max(len(t), 1) < 0.7:
            reasons["symbol_heavy"] += 1
        else:
            reasons["kept"] += 1
    for reason, n in reasons.most_common():
        print(f"  {reason}: {n}")
```

Run `diagnose` on a sample and read the rejection histogram before a full run. If "symbol_heavy" is rejecting half your corpus, your threshold is wrong for this domain or your cleaning step is leaving markup behind — either way you want to know that on a sample, not after processing a million pages. Spot-check a random handful of rejected and kept documents by eye; filters that look reasonable in code are routinely wrong in practice, and ten minutes of reading samples saves a corrupted corpus.

## Decontamination and Why It Matters

If you will evaluate the trained model on any public benchmark, you must remove benchmark text from the training corpus or your eval numbers are fiction — the model will have memorized the answers. This "decontamination" step belongs in the same pipeline, right before sharding: maintain a set of n-gram signatures from your eval sets and drop any training document with a substantial overlap, using the same shingle machinery already built for dedupe. It is the same MinHash/n-gram tooling pointed at a different reference set. Skipping it is one of the most common and most embarrassing mistakes in applied LLM work, and it is cheap to prevent once the dedupe infrastructure exists. Treat the eval sets as just another duplicate source to exclude.

## Provenance and Licensing

- **Keep `source_url`** — the pipeline stamps every record so you can audit and respect site terms.
- **Respect robots and ToS** — only crawl content you are permitted to use for training.
- **Hash IDs** — content-hash IDs make exact-dupe removal across runs trivial.

## Why CRW for Dataset Building

- **Boilerplate removed at the source** — `onlyMainContent` means cleaner input and cheaper downstream filtering.
- **Throughput** — open-core Rust, small single binary, lower-latency than browser-based scrapers; large crawls finish sooner.
- **No per-page cost** — AGPL-3.0 self-host is unlimited, which matters at corpus scale; the fastCRW cloud free tier is a one-time lifetime 1000 credits, never a monthly meter.

## Corpus Statistics You Should Always Compute

Never ship a corpus you have not measured. A few cheap aggregate statistics catch the disasters that a spot-check misses — a single source dominating the mix, a token distribution skewed by one giant page, or far less data surviving the pipeline than you assumed. Compute and log them before training:

```
from collections import Counter
from urllib.parse import urlparse

def corpus_stats(docs: list[dict]):
    n = len(docs)
    total_words = sum(len(d["text"].split()) for d in docs)
    by_host = Counter(urlparse(d["url"]).netloc for d in docs)
    lengths = sorted(len(d["text"].split()) for d in docs)

    print(f"documents:        {n}")
    print(f"total words:      {total_words:,}")
    print(f"avg words/doc:    {total_words // max(n, 1):,}")
    print(f"median words/doc: {lengths[n // 2] if n else 0:,}")
    print("top sources (should NOT be one-host-dominated):")
    for host, c in by_host.most_common(5):
        print(f"  {host}: {c} ({100*c//max(n,1)}%)")
```

The source-concentration line is the one that saves you. If one domain is 80% of the corpus, the model will overfit that site's voice and conventions no matter how good the rest of the pipeline is — and that is invisible without this check. Run `corpus_stats` at the end of `build_dataset` and treat a wildly skewed distribution as a stop-and-rebalance signal, not a "ship it anyway." Measuring the dataset is not optional bookkeeping; it is the cheapest insurance against a training run that fails for a reason you could have seen in ten lines of code.

## Next Steps

- See [Crawl an Entire Website to Markdown](/blog/crawl-entire-website-sitemap-crw) for the collection layer
- Read [Scrape-to-RAG With LlamaIndex](/blog/scrape-to-rag-pipeline-llamaindex) for the RAG variant

Self-host CRW from [GitHub](https://github.com/fastcrw/crw) for free, or use [fastCRW](https://fastcrw.com) for managed cloud scraping.

## FAQ

### Why does onlyMainContent matter for training data?

Navigation, footers, and cookie banners repeat across thousands of pages. Left in, they dominate token frequency and bias the model toward boilerplate. CRW's onlyMainContent removes them at the source, so quality filtering and dedupe operate on actual article text and are far cheaper.

### Why use MinHash LSH instead of exact deduplication?

Exact hashing only catches byte-identical duplicates. The web has many near-duplicates — templated pages, mirrors, minor edits. MinHash LSH finds documents above a Jaccard similarity threshold in near-linear time, removing the near-dupes that exact hashing misses without an O(n^2) pairwise comparison.
