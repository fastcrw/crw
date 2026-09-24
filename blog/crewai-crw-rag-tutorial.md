# Build a RAG-Powered Research Agent with CrewAI and CRW

> Combine crewai-crw web scraping tools with a vector store to build a CrewAI agent that crawls sites, builds a knowledge base, and answers questions with RAG.

**Published:** 2026-04-18  
**Updated:** 2026-04-18  
**Canonical:** https://fastcrw.com/blog/crewai-crw-rag-tutorial

---

## What We're Building

A CrewAI crew that: (1) crawls a documentation site using `crewai-crw`, (2) builds a FAISS vector store from the scraped content, (3) uses a retrieval tool to answer questions with RAG. The scraping agent gathers knowledge, and the research agent uses that knowledge base to produce accurate, source-backed answers.

This is different from a simple scraping crew — here the agents build **persistent knowledge** they can query, not just pass raw content between tasks.

## Prerequisites

- Python 3.11+
- An OpenAI API key (for embeddings and completion)
- CRW running locally or a [fastCRW](https://fastcrw.com) API key

## Step 1: Install Dependencies

```
pip install crewai crewai-crw langchain-openai langchain-community faiss-cpu langchain-text-splitters
```

## Step 2: Start CRW

### Option A: Self-hosted (free)

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

### Option B: Cloud (fastCRW)

```
export CRW_API_URL=https://api.fastcrw.com
export CRW_API_KEY=crw_live_...
```

## Step 3: Build the Knowledge Base

First, crawl a site and build a vector store. This runs once — the agents then query it repeatedly.

```
import requests
from langchain_openai import OpenAIEmbeddings
from langchain_community.vectorstores import FAISS
from langchain_text_splitters import RecursiveCharacterTextSplitter
from langchain_core.documents import Document

def build_knowledge_base(url: str, api_url: str = "http://localhost:3000") -> FAISS:
    """Crawl a site with CRW and build a FAISS vector store."""
    # Start crawl
    resp = requests.post(f"{api_url}/v1/crawl", json={
        "url": url,
        "limit": 50,
        "scrapeOptions": {
            "formats": ["markdown"],
            "onlyMainContent": True,
        },
    }, timeout=30)
    job_id = resp.json()["id"]

    # Poll until done

    while True:
        status = requests.get(f"{api_url}/v1/crawl/{job_id}", timeout=30).json()
        if status["status"] == "completed":
            break
        if status["status"] == "failed":
            raise RuntimeError("Crawl failed")
        time.sleep(2)

    # Convert to LangChain documents
    docs = []
    for page in status.get("data", []):
        content = page.get("markdown", "")
        if content:
            metadata = page.get("metadata", {})
            docs.append(Document(page_content=content, metadata=metadata))

    print(f"Crawled {len(docs)} pages")

    # Chunk and embed
    splitter = RecursiveCharacterTextSplitter(chunk_size=1000, chunk_overlap=200)
    chunks = splitter.split_documents(docs)
    vectorstore = FAISS.from_documents(chunks, OpenAIEmbeddings())
    print(f"Indexed {len(chunks)} chunks")
    return vectorstore

# Build it
vectorstore = build_knowledge_base("https://docs.example.com")
```

## Step 4: Create a RAG Retrieval Tool

Wrap the vector store in a CrewAI tool so agents can query the knowledge base:

```
from crewai.tools import BaseTool
from pydantic import BaseModel, Field

class SearchInput(BaseModel):
    query: str = Field(description="The question to search for in the knowledge base")

class KnowledgeBaseSearchTool(BaseTool):
    name: str = "Search Knowledge Base"
    description: str = (
        "Search the crawled documentation knowledge base. "
        "Returns the most relevant passages for a given question. "
        "Use this to find specific information from the docs."
    )
    args_schema: type[BaseModel] = SearchInput

    def _run(self, query: str) -> str:
        results = vectorstore.similarity_search(query, k=5)
        passages = []
        for i, doc in enumerate(results, 1):
            source = doc.metadata.get("sourceURL", "unknown")
            passages.append(f"[{i}] Source: {source}\n{doc.page_content}")
        return "\n\n---\n\n".join(passages)
```

## Step 5: Create the Agents

```
from crewai import Agent
from crewai_crw import CrwScrapeWebsiteTool

# Tool for live scraping (when the knowledge base doesn't have the answer)
scrape_tool = CrwScrapeWebsiteTool()

# Agent 1: Researcher — uses the knowledge base + live scraping as fallback
researcher = Agent(
    role="Documentation Expert",
    goal="Answer questions accurately using the knowledge base, with live scraping as fallback",
    backstory="""You are an expert at finding information in documentation.
    Always search the knowledge base first. If the answer isn't there,
    use the scrape tool to fetch specific pages directly.""",
    tools=[KnowledgeBaseSearchTool(), scrape_tool],
    verbose=True,
)

# Agent 2: Writer — produces polished output
writer = Agent(
    role="Technical Writer",
    goal="Transform research findings into clear, well-structured answers",
    backstory="""You write clear technical documentation. You take raw research
    and turn it into polished, accurate answers with proper source attribution.""",
    verbose=True,
)
```

## Step 6: Define Tasks and Run

```
from crewai import Task, Crew, Process

research_task = Task(
    description="""Answer this question: {question}

    Steps:
    1. Search the knowledge base for relevant information
    2. If the knowledge base doesn't have enough info, scrape specific pages
    3. Compile all findings with source URLs

    Be thorough — check multiple search queries if the first doesn't give good results.""",
    expected_output="Comprehensive research findings with source URLs for each piece of information.",
    agent=researcher,
)

writing_task = Task(
    description="""Take the research findings and produce a clear answer.

    Requirements:
    - Start with a direct answer to the question
    - Include relevant details and examples
    - Cite sources as [Source: URL] at the end
    - If info was contradictory, note the discrepancy""",
    expected_output="A clear, well-structured answer with source citations.",
    agent=writer,
    context=[research_task],
)

crew = Crew(
    agents=[researcher, writer],
    tasks=[research_task, writing_task],
    process=Process.sequential,
    verbose=True,
)

result = crew.kickoff(inputs={"question": "How do I set up authentication?"})
print(result)
```

## Complete Script

```
import time

from crewai import Agent, Task, Crew, Process
from crewai.tools import BaseTool
from crewai_crw import CrwScrapeWebsiteTool
from langchain_openai import OpenAIEmbeddings
from langchain_community.vectorstores import FAISS
from langchain_text_splitters import RecursiveCharacterTextSplitter
from langchain_core.documents import Document
from pydantic import BaseModel, Field

# --- Step 1: Build knowledge base ---
CRW_URL = "http://localhost:3000"  # or https://api.fastcrw.com

resp = requests.post(f"{CRW_URL}/v1/crawl", json={
    "url": "https://docs.example.com",
    "limit": 50,
    "scrapeOptions": {"formats": ["markdown"], "onlyMainContent": True},
}, timeout=30)
job_id = resp.json()["id"]

while True:
    status = requests.get(f"{CRW_URL}/v1/crawl/{job_id}", timeout=30).json()
    if status["status"] == "completed": break
    if status["status"] == "failed": raise RuntimeError("Crawl failed")
    time.sleep(2)

docs = [Document(page_content=p["markdown"], metadata=p.get("metadata", {}))
        for p in status.get("data", []) if p.get("markdown")]
chunks = RecursiveCharacterTextSplitter(chunk_size=1000, chunk_overlap=200).split_documents(docs)
vectorstore = FAISS.from_documents(chunks, OpenAIEmbeddings())
print(f"Knowledge base: {len(docs)} pages, {len(chunks)} chunks")

# --- Step 2: RAG tool ---
class SearchInput(BaseModel):
    query: str = Field(description="Search query")

class KnowledgeBaseSearchTool(BaseTool):
    name: str = "Search Knowledge Base"
    description: str = "Search crawled docs knowledge base for relevant information"
    args_schema: type[BaseModel] = SearchInput
    def _run(self, query: str) -> str:
        results = vectorstore.similarity_search(query, k=5)
        return "\n\n---\n\n".join(
            f"[Source: {r.metadata.get('sourceURL', '?')}]\n{r.page_content}"
            for r in results
        )

# --- Step 3: Agents ---
researcher = Agent(
    role="Documentation Expert",
    goal="Answer questions using the knowledge base",
    backstory="Expert at finding info in docs. Search knowledge base first, scrape as fallback.",
    tools=[KnowledgeBaseSearchTool(), CrwScrapeWebsiteTool()],
)
writer = Agent(
    role="Technical Writer",
    goal="Produce clear answers with source citations",
    backstory="Turns raw research into polished, accurate answers.",
)

# --- Step 4: Run ---
research = Task(
    description="Answer: {question}\nSearch knowledge base, scrape if needed, cite sources.",
    expected_output="Research findings with sources",
    agent=researcher,
)
writing = Task(
    description="Write a clear answer from the research. Cite sources.",
    expected_output="Polished answer with citations",
    agent=writer,
    context=[research],
)

crew = Crew(agents=[researcher, writer], tasks=[research, writing], process=Process.sequential)
print(crew.kickoff(inputs={"question": "How do I authenticate?"}))
```

## Why This Pattern?

**Knowledge base > raw scraping.** Without RAG, your agent gets raw page content and has to reason over thousands of tokens of noise. With a vector store, the agent gets the 5 most relevant passages — faster, cheaper, more accurate.

**Live scraping as fallback.** The knowledge base might be stale or missing a page. The `CrwScrapeWebsiteTool` lets the agent fetch live content when the vector store doesn't have the answer.

**Reusable knowledge.** Build the vector store once, query it from multiple crews and tasks. Add new pages incrementally as you discover them.

## Self-hosted vs Cloud

All code works identically with both:

```
# Self-hosted
CRW_URL = "http://localhost:3000"
scrape_tool = CrwScrapeWebsiteTool()

# Cloud
CRW_URL = "https://api.fastcrw.com"
scrape_tool = CrwScrapeWebsiteTool(
    api_url="https://api.fastcrw.com",
    api_key="crw_live_...",
)
```

## Next Steps

- [crewai-crw on PyPI](https://pypi.org/project/crewai-crw/)
- [CrewAI + CRW multi-agent workflow](/blog/crewai-web-scraping) (without RAG)
- [LangChain + CRW RAG tutorial](/blog/langchain-crw-rag-tutorial)
- [Use CRW's MCP server](/blog/mcp-web-scraping) for Claude Code / Cursor

## Get Started

```
pip install crewai crewai-crw
```

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

Or sign up for [fastCRW](https://fastcrw.com) to skip infrastructure setup.
