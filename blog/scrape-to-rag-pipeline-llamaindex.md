# Scrape-to-RAG with LlamaIndex and CRW (2026): A Production Ingestion Pipeline

> Build a production scrape-to-RAG pipeline: crawl a docs site with CRW, chunk clean markdown, embed with OpenAI, and query with LlamaIndex. Full runnable Python — self-host for $0 under AGPL-3.0.

**Published:** 2026-05-22  
**Updated:** 2026-05-22  
**Canonical:** https://fastcrw.com/blog/scrape-to-rag-pipeline-llamaindex

---

## What We're Building

A retrieval-augmented generation (RAG) pipeline that ingests an entire documentation site and answers questions about it. The hard part of RAG is rarely the vector store — it's getting *clean* text out of the web. HTML nav bars, cookie banners, and script tags poison your embeddings. CRW's crawl endpoint returns LLM-ready markdown, so the ingestion step becomes trivial.

We'll crawl a site with CRW, chunk the markdown, embed it with OpenAI, store vectors in a local LlamaIndex index, and run a query engine on top. Everything runs locally; CRW self-hosts for free under AGPL-3.0.

## Architecture

- **Crawl** — CRW's `/v1/crawl` walks the site and returns markdown per page
- **Chunk** — LlamaIndex splits markdown into overlapping nodes
- **Embed + Store** — OpenAI embeddings into a persisted vector index
- **Query** — A retriever + LLM answers questions with citations

## Prerequisites

- CRW running locally: `docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`
- Python 3.10+ and an OpenAI API key

```
pip install firecrawl-py llama-index llama-index-llms-openai llama-index-embeddings-openai
```

## Step 1: Point the Firecrawl SDK at CRW

CRW is Firecrawl-API compatible, so the official SDK works after a one-line change to `api_url`:

```
from firecrawl import FirecrawlApp

# Self-hosted CRW
app = FirecrawlApp(api_key="crw_live_YOUR-KEY", api_url="http://localhost:3000")

# Or fastCRW managed cloud
# app = FirecrawlApp(api_key="crw_live_YOUR-KEY", api_url="https://api.fastcrw.com")
```

## Step 2: Crawl the Site

The crawl endpoint discovers links and returns each page as markdown. Limit depth and page count so your first run is cheap:

```
def crawl_site(base_url: str, limit: int = 60) -> list[dict]:
    """Crawl a site and return a list of {url, markdown} records."""
    job = app.crawl_url(
        base_url,
        params={
            "limit": limit,
            "maxDepth": 3,
            "scrapeOptions": {"formats": ["markdown"], "onlyMainContent": True},
        },
    )

    docs = []
    for page in job.get("data", []):
        md = page.get("markdown", "")
        url = page.get("metadata", {}).get("sourceURL", "")
        if md and len(md) > 200:  # skip near-empty pages
            docs.append({"url": url, "markdown": md})
    print(f"Crawled {len(docs)} usable pages from {base_url}")
    return docs
```

`onlyMainContent: True` strips navigation, footers, and boilerplate — the single biggest quality win for RAG. You are embedding the article body, not the chrome around it.

## Step 3: Convert to LlamaIndex Documents

Wrap each page in a LlamaIndex `Document` with metadata so retrieved chunks can cite their source URL:

```
from llama_index.core import Document

def to_documents(records: list[dict]) -> list[Document]:
    return [
        Document(
            text=r["markdown"],
            metadata={"source_url": r["url"]},
            excluded_llm_metadata_keys=["source_url"],
        )
        for r in records
    ]
```

## Step 4: Chunk, Embed, and Build the Index

A markdown-aware splitter keeps headings with their content, which improves retrieval relevance. We persist the index to disk so re-runs skip re-embedding:

```
import os
from llama_index.core import VectorStoreIndex, StorageContext, load_index_from_storage
from llama_index.core.node_parser import MarkdownNodeParser
from llama_index.embeddings.openai import OpenAIEmbedding
from llama_index.llms.openai import OpenAI
from llama_index.core import Settings

Settings.embed_model = OpenAIEmbedding(model="text-embedding-3-small")
Settings.llm = OpenAI(model="gpt-4o-mini", temperature=0)

PERSIST_DIR = "./rag_index"

def build_index(docs: list[Document]) -> VectorStoreIndex:
    if os.path.exists(PERSIST_DIR):
        print("Loading existing index from disk")
        ctx = StorageContext.from_defaults(persist_dir=PERSIST_DIR)
        return load_index_from_storage(ctx)

    parser = MarkdownNodeParser()
    nodes = parser.get_nodes_from_documents(docs)
    print(f"Created {len(nodes)} chunks")

    index = VectorStoreIndex(nodes)
    index.storage_context.persist(persist_dir=PERSIST_DIR)
    return index
```

## Step 5: Query With Citations

The query engine retrieves the top chunks, sends them to the LLM, and returns an answer plus the source nodes so you can show citations:

```
def answer(index: VectorStoreIndex, question: str) -> None:
    engine = index.as_query_engine(similarity_top_k=4, response_mode="compact")
    response = engine.query(question)

    print(f"\nQ: {question}")
    print(f"A: {response}\n")
    print("Sources:")
    seen = set()
    for node in response.source_nodes:
        url = node.metadata.get("source_url", "unknown")
        if url not in seen:
            print(f"  - {url}  (score={node.score:.3f})")
            seen.add(url)
```

## Step 6: Wire It Together

```
def main():
    records = crawl_site("https://docs.example.com", limit=60)
    docs = to_documents(records)
    index = build_index(docs)

    for q in [
        "How do I authenticate API requests?",
        "What is the rate limit on the free tier?",
        "How do I paginate list endpoints?",
    ]:
        answer(index, q)

if __name__ == "__main__":
    main()
```

## Incremental Re-Crawls

Docs sites change. Instead of re-embedding everything nightly, re-crawl and only upsert pages whose content hash changed:

```
import hashlib

def content_hash(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()

def refresh(index: VectorStoreIndex, base_url: str, known: dict[str, str]):
    """known maps url -> last content hash. Returns updated map."""
    records = crawl_site(base_url, limit=60)
    parser = MarkdownNodeParser()
    changed = 0

    for r in records:
        h = content_hash(r["markdown"])
        if known.get(r["url"]) == h:
            continue  # unchanged, skip
        # delete old nodes for this source, then re-insert
        index.delete_ref_doc(r["url"], delete_from_docstore=True)
        doc = Document(text=r["markdown"], metadata={"source_url": r["url"]},
                       id_=r["url"])
        index.insert_nodes(parser.get_nodes_from_documents([doc]))
        known[r["url"]] = h
        changed += 1

    index.storage_context.persist(persist_dir=PERSIST_DIR)
    print(f"Refreshed {changed} changed pages")
    return known
```

## Tuning Chunk Size and Overlap

Chunking is where most RAG quality is won or lost, and it interacts directly with how clean your source text is. The `MarkdownNodeParser` splits on heading boundaries, which is ideal because CRW preserves the document's heading structure — a section about "Authentication" stays together instead of being sliced mid-explanation by a naive fixed-window splitter. If your retrieved chunks are too coarse (the LLM gets a wall of text and the relevant sentence is buried), add a secondary sentence splitter with a smaller window. If they are too fine (answers lack context), increase overlap so adjacent chunks share a sentence or two of boundary text:

```
from llama_index.core.node_parser import SentenceSplitter

# Two-stage: structure-aware first, then size-bounded
md_parser = MarkdownNodeParser()
size_parser = SentenceSplitter(chunk_size=512, chunk_overlap=64)

def chunk(docs):
    coarse = md_parser.get_nodes_from_documents(docs)
    return size_parser.get_nodes_from_documents(
        [n.to_document() if hasattr(n, "to_document") else n for n in coarse]
    )
```

A practical starting point for docs and articles is a 512-token chunk with ~12% overlap. Code-heavy sources benefit from larger chunks because a truncated code block is useless to the model. Always evaluate on real questions — chunking is empirical, not theoretical.

## Why Retrieval Quality Starts at the Crawler

Teams spend weeks tuning rerankers and prompt templates while feeding the index HTML soup. The single highest-leverage change is usually upstream: clean input. When CRW returns `onlyMainContent` markdown, three things improve at once. First, embeddings represent the actual subject matter instead of being dragged toward the navigation and footer text that repeats on every page. Second, you spend fewer tokens per chunk on boilerplate, so each chunk carries more signal and your context budget goes further. Third, citations become trustworthy because the retrieved span is article text the user can verify, not a cookie banner.

This is why the ingestion step in this tutorial is only a few lines: the difficulty was moved into CRW. A selector-based scraper would need per-site rules to strip chrome, and those rules rot every time a site redesigns. The schema-and-markdown approach generalizes across every site you point it at, which is exactly the property a RAG corpus needs as it grows from one source to dozens.

## Evaluating the Pipeline

Do not ship a RAG system on vibes. Build a small fixed question set with known-good answers and known source pages, then assert that the right source URL appears in `response.source_nodes` for each. This catches regressions when you change chunking, swap embedding models, or re-crawl. A minimal harness:

```
EVAL = [
    {"q": "How do I authenticate?", "must_cite": "docs.example.com/auth"},
    {"q": "What is the free-tier limit?", "must_cite": "docs.example.com/pricing"},
]

def evaluate(index):
    engine = index.as_query_engine(similarity_top_k=4)
    passed = 0
    for case in EVAL:
        resp = engine.query(case["q"])
        cited = any(case["must_cite"] in n.metadata.get("source_url", "")
                    for n in resp.source_nodes)
        print(("PASS" if cited else "FAIL"), case["q"])
        passed += cited
    print(f"{passed}/{len(EVAL)} retrieval checks passed")
```

Run this in CI after every ingestion change. Retrieval correctness is a property you can regression-test, and a crawler that returns consistent clean markdown makes those tests stable rather than flaky.

## Why CRW for the Ingestion Layer

- **Clean markdown out of the box** — `onlyMainContent` removes the boilerplate that otherwise pollutes embeddings and wastes context tokens.
- **One crawl call** — no per-URL orchestration; CRW handles link discovery, dedup, and depth limits server-side.
- **No lock-in** — the engine is open-source (AGPL-3.0, small single binary, lower-latency, local-first). Self-host for free or hand ops to the managed cloud with one URL change.

## Serving the Index Behind an API

An index on disk is only useful if something can query it. Wrap the query engine in a tiny FastAPI service so your app, agents, or a chat UI can hit it over HTTP:

```
from fastapi import FastAPI
from pydantic import BaseModel

api = FastAPI()
_index = build_index([])  # loads from PERSIST_DIR on startup
_engine = _index.as_query_engine(similarity_top_k=4)

class Query(BaseModel):
    question: str

@api.post("/ask")
def ask(q: Query):
    resp = _engine.query(q.question)
    return {
        "answer": str(resp),
        "sources": sorted({
            n.metadata.get("source_url", "")
            for n in resp.source_nodes
        }),
    }
# uvicorn serve:api --port 8080
```

This keeps the expensive index in memory once and answers each request in milliseconds, while the ingestion job refreshes the on-disk index on its own schedule. The serving and ingestion concerns stay cleanly separated, which is exactly how a production RAG system should be shaped.

## Next Steps

- Read [How to Build a RAG Pipeline with CRW](/blog/rag-pipeline-with-crw) for a LangChain variant
- See [Website to Markdown with CRW](/blog/website-to-markdown) for extraction options

Self-host CRW from [GitHub](https://github.com/fastcrw/crw) for free, or use [fastCRW](https://fastcrw.com) for managed cloud scraping.

## FAQ

### Why crawl with CRW instead of LlamaIndex's built-in web reader?

LlamaIndex's web readers fetch raw HTML and you still have to strip boilerplate yourself, which directly hurts embedding quality. CRW returns onlyMainContent markdown server-side, so chunks contain article text rather than nav bars and cookie banners. It also handles link discovery and depth limits in a single crawl call.

### How do I keep the RAG index fresh without re-embedding everything?

Re-crawl on a schedule, hash each page's markdown, and only delete + re-insert nodes for pages whose hash changed. The refresh() function in this tutorial shows the pattern — it keeps embedding cost proportional to how much the site actually changed.
