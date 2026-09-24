# Build a RAG Pipeline with LangChain and CRW in 5 Minutes

> Use langchain-crw to crawl a docs site, chunk the content, embed into a vector store, and answer questions — all with LangChain's native interface.

**Published:** 2026-04-30  
**Updated:** 2026-04-30  
**Canonical:** https://fastcrw.com/blog/langchain-crw-rag-tutorial

---

## What We're Building

A complete RAG pipeline in Python using [langchain-crw](https://pypi.org/project/langchain-crw/) — the official CRW document loader for LangChain. We'll crawl a documentation site, chunk the content, embed it into FAISS, and answer questions with retrieval-augmented generation.

The entire pipeline is 30 lines of Python. No raw HTTP calls, no custom loaders — just `pip install langchain-crw` and go.

## Prerequisites

- Python 3.10+
- An OpenAI API key (for embeddings and completion)
- CRW running locally or a [fastCRW](https://fastcrw.com) API key

## Step 1: Install Dependencies

```
pip install langchain-crw langchain-openai langchain-community faiss-cpu langchain-text-splitters
```

## Step 2: Start CRW

Pick one:

### Option A: Self-hosted (free)

```
# Install and start CRW
curl -fsSL https://fastcrw.com/install | bash
crw serve  # runs on http://localhost:3000

# Or Docker
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest

# Point the loader at it
export CRW_API_URL=http://localhost:3000
```

### Option B: Cloud (fastCRW)

```
# Set env vars — no server needed
export CRW_API_URL=https://api.fastcrw.com
export CRW_API_KEY=crw_live_...
```

## Step 3: Crawl a Docs Site

Use `CrwLoader` in crawl mode to discover and scrape all pages:

```
from langchain_crw import CrwLoader

# Crawl mode: discovers pages via BFS, returns each as a Document
loader = CrwLoader(
    url="https://docs.example.com",
    mode="crawl",
    params={
        "limit": 100,        # stop after 100 pages
    },
)
docs = loader.load()
print(f"Crawled {len(docs)} pages")
```

Each document has `page_content` (clean markdown) and `metadata` (title, sourceURL, statusCode). CRW strips navigation, footers, and sidebars automatically.

## Step 4: Chunk the Content

```
from langchain_text_splitters import RecursiveCharacterTextSplitter

splitter = RecursiveCharacterTextSplitter(
    chunk_size=1000,
    chunk_overlap=200,
    separators=["\n## ", "\n### ", "\n\n", "\n", " "],  # respect markdown structure
)
chunks = splitter.split_documents(docs)
print(f"Split into {len(chunks)} chunks")
```

The markdown-aware separators keep headings with their content, so retrieval returns coherent sections instead of cut-off paragraphs.

## Step 5: Embed and Store

```
from langchain_openai import OpenAIEmbeddings
from langchain_community.vectorstores import FAISS

embeddings = OpenAIEmbeddings()
vectorstore = FAISS.from_documents(chunks, embeddings)
print(f"Indexed {len(chunks)} chunks into FAISS")
```

## Step 6: Query with RAG

```
from langchain_openai import ChatOpenAI
from langchain_core.prompts import ChatPromptTemplate
from langchain_core.runnables import RunnablePassthrough
from langchain_core.output_parsers import StrOutputParser

retriever = vectorstore.as_retriever(search_kwargs={"k": 5})
llm = ChatOpenAI(model="gpt-4o-mini")

prompt = ChatPromptTemplate.from_template("""Answer the question based only on the following context:

{context}

Question: {question}

Answer:""")

chain = (
    {"context": retriever, "question": RunnablePassthrough()}
    | prompt
    | llm
    | StrOutputParser()
)

answer = chain.invoke("How do I authenticate?")
print(answer)
```

## Complete Script

Here's everything in one copy-paste block:

```
from langchain_crw import CrwLoader
from langchain_openai import OpenAIEmbeddings, ChatOpenAI
from langchain_community.vectorstores import FAISS
from langchain_text_splitters import RecursiveCharacterTextSplitter
from langchain_core.prompts import ChatPromptTemplate
from langchain_core.runnables import RunnablePassthrough
from langchain_core.output_parsers import StrOutputParser

# 1. Crawl
loader = CrwLoader(
    url="https://docs.example.com",
    mode="crawl",
    params={"limit": 100},
)
docs = loader.load()
print(f"Crawled {len(docs)} pages")

# 2. Chunk
splitter = RecursiveCharacterTextSplitter(chunk_size=1000, chunk_overlap=200)
chunks = splitter.split_documents(docs)

# 3. Embed
vectorstore = FAISS.from_documents(chunks, OpenAIEmbeddings())

# 4. Query
retriever = vectorstore.as_retriever(search_kwargs={"k": 5})
prompt = ChatPromptTemplate.from_template(
    "Answer based on context:\n{context}\n\nQuestion: {question}\nAnswer:"
)
chain = (
    {"context": retriever, "question": RunnablePassthrough()}
    | prompt
    | ChatOpenAI(model="gpt-4o-mini")
    | StrOutputParser()
)

answer = chain.invoke("How do I authenticate?")
print(answer)
```

## Using Different Modes

### Scrape a single page

When you know exactly which URL you need:

```
loader = CrwLoader(url="https://docs.example.com/api-reference", mode="scrape")
docs = loader.load()  # returns 1 document
```

### Map first, then scrape selectively

Discover all URLs, filter, then scrape only what you need:

```
# Discover all URLs
mapper = CrwLoader(url="https://docs.example.com", mode="map")
url_docs = mapper.load()
urls = [doc.page_content for doc in url_docs]

# Filter to API docs only
api_urls = [u for u in urls if "/api" in u]

# Scrape each
all_docs = []
for url in api_urls:
    loader = CrwLoader(url=url, mode="scrape")
    all_docs.extend(loader.load())
print(f"Scraped {len(all_docs)} API pages")
```

### With JS rendering

For SPAs that need JavaScript to render content:

```
loader = CrwLoader(
    url="https://spa-docs.example.com",
    mode="scrape",
    params={
        "renderJs": True,
        "actions": [{"type": "wait", "milliseconds": 3000}],  # wait 3s after page load
    },
)
docs = loader.load()
```

## Self-hosted vs Cloud

The code is identical — only the backend changes:

```
# Self-hosted: point at your own server
loader = CrwLoader(
    url="https://example.com",
    api_url="http://localhost:3000",
    mode="scrape",
)

# Cloud: pass api_url and api_key
loader = CrwLoader(
    url="https://example.com",
    api_url="https://api.fastcrw.com",
    api_key="crw_live_...",
    mode="scrape",
)

# Or set env vars and use no args everywhere
# export CRW_API_URL=https://api.fastcrw.com
# export CRW_API_KEY=crw_live_...
loader = CrwLoader(url="https://example.com", mode="scrape")
```

## Why CRW for RAG?

- **Clean markdown by default.** CRW strips nav, footers, and boilerplate. Your chunks contain actual content, not HTML noise — which means better embeddings and more relevant retrieval.
- **Low-latency, local-first crawling.** Running the engine next to your pipeline avoids remote API round trips, so crawling a large docs site stays quick. See the full latency distribution on our [public benchmark](/benchmarks).
- **Native LangChain integration.** `CrwLoader` implements `BaseLoader` with `lazy_load()` — works with every LangChain component out of the box.
- **Self-hosted for free.** No API key, no rate limits, no per-page costs during development. Switch to [fastCRW](https://fastcrw.com) cloud when you need production reliability.

## Next Steps

- [langchain-crw on PyPI](https://pypi.org/project/langchain-crw/) — full API reference
- [GitHub: us/langchain-crw](https://github.com/us/langchain-crw)
- [Use CRW with CrewAI](/blog/crewai-web-scraping) for multi-agent scraping workflows
- [Use CRW's MCP server](/blog/mcp-web-scraping) for Claude Code / Cursor integration

## Get Started

```
pip install langchain-crw
```

Run CRW locally:

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

Or sign up for [fastCRW](https://fastcrw.com) to skip infrastructure setup.
