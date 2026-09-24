# How to Build a RAG Pipeline from Websites Using CRW

> Step-by-step guide to scraping websites, converting to clean markdown, and feeding into a RAG pipeline using CRW's API.

**Published:** 2026-03-06  
**Updated:** 2026-03-06  
**Canonical:** https://fastcrw.com/blog/rag-pipeline-with-crw

---

## What We're Building

A Retrieval-Augmented Generation (RAG) pipeline that: (1) crawls a documentation website, (2) converts each page to clean markdown, (3) chunks the content, (4) embeds the chunks into a vector store, and (5) answers user questions by retrieving relevant chunks.

CRW handles steps 1 and 2. We'll use TypeScript throughout, but the same pattern works in any language with an HTTP client. We'll also cover Python examples and show integrations with LangChain, LlamaIndex, and three popular vector databases.

## Prerequisites

- CRW running locally: `docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`
- Node.js 18+ or Bun
- An OpenAI API key (for embeddings and completion)

## Choosing the Right Crawl Strategy

CRW exposes three endpoints with different tradeoffs. Choosing the right one up front saves time and cost:

### /v1/scrape — Single Page, Immediate

Use `/v1/scrape` when you know exactly which URL you need and want a synchronous response. Ideal for: fetching a specific doc page in response to a user query, live content retrieval in an agent, or one-off extractions. Returns immediately with the full page content.

Best for: targeted single-document retrieval, real-time RAG lookups, agent-driven fetching.

### /v1/crawl — Full Site, Async

Use `/v1/crawl` when you want to index an entire site or section. The crawl is asynchronous — you post a start request, get a job ID, and poll for status. CRW discovers and scrapes all linked pages up to your `limit`, respecting depth constraints. Returns partial results as pages complete.

Best for: initial index builds, periodic full re-indexing, documentation sites with predictable link structure.

### /v1/map — URL Discovery, No Content

Use `/v1/map` when you need to know what pages exist before deciding what to scrape. Map returns a JSON array of discovered URLs without fetching full page content — much faster than crawl for URL discovery. Use it to preview a site's structure, filter to relevant sections, then scrape only what you need.

Best for: incremental updates (detect new pages, scrape only deltas), pre-filtering large sites before crawling, building sitemaps.

## Step 1: Crawl the Target Website

Use CRW's `/v1/crawl` endpoint to discover and scrape all pages on a site. CRW returns clean markdown automatically — no HTML parsing required on your end.

```
const BASE_URL = "https://api.fastcrw.com"; // or http://localhost:3000 for self-hosted

async function crawlSite(url: string) {
  const startRes = await fetch(`${BASE_URL}/v1/crawl`, {
    method: "POST",
    headers: {
      "Authorization": "Bearer crw_live_YOUR_API_KEY",
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      url,
      limit: 100,
      scrapeOptions: { formats: ["markdown"] },
    }),
  });
  const { id } = await startRes.json();

  while (true) {
    await new Promise((r) => setTimeout(r, 2000));
    const statusRes = await fetch(`${BASE_URL}/v1/crawl/${id}`);
    const status = await statusRes.json();
    if (status.status === "completed") return status.data;
    if (status.status === "failed") throw new Error("Crawl failed");
    // status.data contains partial results — pages scraped so far
    console.log(`Progress: ${status.data?.length ?? 0} pages scraped`);
  }
}
```

Each item in `status.data` has a `markdown` field — clean text ready for chunking — and a `metadata` field containing `sourceURL`, `title`, and other page metadata.

## Step 2: Scrape a Single Page

If you only need one page — for example, fetching live content in response to a user query — use the `/v1/scrape` endpoint:

```
async function scrapePage(url: string): Promise<{ markdown: string; title: string }> {
  const res = await fetch(`${BASE_URL}/v1/scrape`, {
    method: "POST",
    headers: {
      "Authorization": "Bearer crw_live_YOUR_API_KEY",
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ url, formats: ["markdown"] }),
  });
  const data = await res.json();
  if (!data.success) throw new Error(data.error);
  return {
    markdown: data.data.markdown,
    title: data.data.metadata?.title ?? url,
  };
}
```

CRW strips navigation, ads, footers, and boilerplate automatically. The result is clean prose that embeds well and improves retrieval precision.

## Step 3: Chunk the Markdown

Embedding models have token limits. Split each page's markdown into overlapping chunks:

```
function chunkText(text: string, size = 512, overlap = 64): string[] {
  const words = text.split(/s+/);
  const chunks: string[] = [];
  let i = 0;
  while (i < words.length) {
    chunks.push(words.slice(i, i + size).join(" "));
    i += size - overlap;
  }
  return chunks;
}
```

A chunk size of 400–600 words with 10–15% overlap works well for most content. See the Advanced Chunking Strategies section below for more sophisticated approaches.

## Advanced Chunking Strategies

### Semantic Chunking on Markdown Structure

Markdown's heading hierarchy is a natural chunking signal. Instead of splitting on word count, split on `##` headings — each section becomes a chunk with a coherent topic:

```
function chunkByHeadings(markdown: string): Array<{ heading: string; content: string }> {
  const sections = markdown.split(/
(?=#{1,3} )/);
  return sections
    .map((section) => {
      const firstLine = section.split("
")[0];
      const heading = firstLine.replace(/^#+s+/, "").trim();
      return { heading, content: section.trim() };
    })
    .filter((s) => s.content.length > 50); // drop empty sections
}
```

This produces chunks that are topically coherent — each chunk is about one concept — which improves retrieval relevance compared to arbitrary word-count splits.

### Parent-Child Chunking

Parent-child chunking stores two versions of each chunk: a large "parent" chunk for context and a smaller "child" chunk for embedding precision. The retrieval query matches against child chunks, but the full parent is returned to the LLM:

```
interface ParentChildChunk {
  parentId: string;
  childId: string;
  parentText: string;
  childText: string;
  embedding?: number[];
}

function parentChildChunks(markdown: string, url: string): ParentChildChunk[] {
  // Parents: section-level (heading splits)
  const sections = chunkByHeadings(markdown);
  const result: ParentChildChunk[] = [];

  for (const section of sections) {
    const parentId = `${url}#${section.heading}`;
    // Children: sentence-level within each section
    const sentences = section.content
      .split(/(?<=[.!?])s+/)
      .filter((s) => s.length > 30);

    for (let i = 0; i < sentences.length; i += 2) {
      const childText = sentences.slice(i, i + 2).join(" ");
      result.push({
        parentId,
        childId: `${parentId}:${i}`,
        parentText: section.content,
        childText,
      });
    }
  }
  return result;
}
```

## Step 4: Embed the Chunks

Use OpenAI's embedding API to convert each chunk to a vector:

```
import OpenAI from "openai";

const openai = new OpenAI();

async function embedChunks(chunks: string[]): Promise<number[][]> {
  // OpenAI can embed up to 2,048 inputs per call
  const BATCH = 256;
  const allEmbeddings: number[][] = [];

  for (let i = 0; i < chunks.length; i += BATCH) {
    const batch = chunks.slice(i, i + BATCH);
    const res = await openai.embeddings.create({
      model: "text-embedding-3-small",
      input: batch,
    });
    allEmbeddings.push(...res.data.map((d) => d.embedding));
  }
  return allEmbeddings;
}
```

## Step 5: Store in a Vector Database

For quick prototyping, an in-memory store works. For production, choose a proper vector database.

### In-Memory Store (Development)

```
interface Doc {
  url: string;
  title: string;
  chunk: string;
  embedding: number[];
}

const store: Doc[] = [];

async function indexSite(siteUrl: string) {
  const pages = await crawlSite(siteUrl);

  for (const page of pages) {
    const chunks = chunkText(page.markdown);
    const embeddings = await embedChunks(chunks);

    for (let i = 0; i < chunks.length; i++) {
      store.push({
        url: page.metadata.sourceURL,
        title: page.metadata.title ?? "",
        chunk: chunks[i],
        embedding: embeddings[i],
      });
    }
  }
  console.log(`Indexed ${store.length} chunks from ${pages.length} pages`);
}
```

### Using pgvector (PostgreSQL)

pgvector is a natural choice if you're already running PostgreSQL:

```
import { Pool } from "pg";

const pool = new Pool({ connectionString: process.env.DATABASE_URL });

// One-time setup
await pool.query(`
  CREATE EXTENSION IF NOT EXISTS vector;
  CREATE TABLE IF NOT EXISTS documents (
    id SERIAL PRIMARY KEY,
    url TEXT NOT NULL,
    title TEXT,
    chunk TEXT NOT NULL,
    embedding vector(1536)
  );
  CREATE INDEX ON documents USING ivfflat (embedding vector_cosine_ops)
    WITH (lists = 100);
`);

// Insert chunks
async function insertChunks(
  url: string,
  title: string,
  chunks: string[],
  embeddings: number[][],
) {
  for (let i = 0; i < chunks.length; i++) {
    await pool.query(
      "INSERT INTO documents (url, title, chunk, embedding) VALUES ($1, $2, $3, $4)",
      [url, title, chunks[i], JSON.stringify(embeddings[i])],
    );
  }
}

// Query
async function queryPgVector(questionEmbedding: number[], topK = 5) {
  const result = await pool.query(
    `SELECT url, title, chunk,
       1 - (embedding <=> $1) AS similarity
     FROM documents
     ORDER BY embedding <=> $1
     LIMIT $2`,
    [JSON.stringify(questionEmbedding), topK],
  );
  return result.rows;
}
```

### Using Pinecone

```
import { Pinecone } from "@pinecone-database/pinecone";

const pinecone = new Pinecone({ apiKey: process.env.PINECONE_API_KEY! });
const index = pinecone.index("docs-index");

async function upsertToPinecone(
  url: string,
  chunks: string[],
  embeddings: number[][],
) {
  const vectors = embeddings.map((embedding, i) => ({
    id: `${url}#chunk-${i}`,
    values: embedding,
    metadata: { url, chunk: chunks[i] },
  }));

  // Pinecone recommends batches of 100
  for (let i = 0; i < vectors.length; i += 100) {
    await index.upsert(vectors.slice(i, i + 100));
  }
}

async function queryPinecone(questionEmbedding: number[], topK = 5) {
  const result = await index.query({
    vector: questionEmbedding,
    topK,
    includeMetadata: true,
  });
  return result.matches ?? [];
}
```

### Using Chroma

```
import { ChromaClient } from "chromadb";

const chroma = new ChromaClient({ path: "http://localhost:8000" });
const collection = await chroma.getOrCreateCollection({ name: "docs" });

async function upsertToChroma(
  url: string,
  chunks: string[],
  embeddings: number[][],
) {
  await collection.upsert({
    ids: chunks.map((_, i) => `${url}#${i}`),
    embeddings,
    documents: chunks,
    metadatas: chunks.map(() => ({ url })),
  });
}

async function queryChroma(questionEmbedding: number[], topK = 5) {
  return collection.query({
    queryEmbeddings: [questionEmbedding],
    nResults: topK,
  });
}
```

## Using CRW with LangChain

LangChain's document loader interface makes it easy to plug CRW in as a web loader:

```
import { RecursiveCharacterTextSplitter } from "langchain/text_splitter";

// Custom CRW loader
async function loadWithCRW(urls: string[]): Promise<Document[]> {
  const docs: Document[] = [];
  for (const url of urls) {
    const res = await fetch("https://api.fastcrw.com/v1/scrape", { // or http://localhost:3000 for self-hosted
      method: "POST",
      headers: {
        "Authorization": "Bearer crw_live_YOUR_API_KEY",
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ url, formats: ["markdown"] }),
    });
    const data = await res.json();
    if (data.success) {
      docs.push(
        new Document({
          pageContent: data.data.markdown,
          metadata: { source: url, title: data.data.metadata?.title },
        }),
      );
    }
  }
  return docs;
}

// Build a vector store with LangChain
const rawDocs = await loadWithCRW(["https://docs.example.com/intro", "https://docs.example.com/api"]);

const splitter = new RecursiveCharacterTextSplitter({
  chunkSize: 1000,
  chunkOverlap: 100,
});
const splitDocs = await splitter.splitDocuments(rawDocs);

const vectorStore = await MemoryVectorStore.fromDocuments(
  splitDocs,
  new OpenAIEmbeddings(),
);

// Query
const results = await vectorStore.similaritySearch("How to authenticate?", 5);
console.log(results[0].pageContent);
```

## Using CRW with LlamaIndex

```
import { OpenAI as LlamaOpenAI } from "llamaindex";

// CRW-backed custom reader for LlamaIndex
class CRWReader {
  async loadData(urls: string[]) {
    const documents = [];
    for (const url of urls) {
      const res = await fetch("https://api.fastcrw.com/v1/scrape", { // or http://localhost:3000 for self-hosted
        method: "POST",
        headers: {
          "Authorization": "Bearer crw_live_YOUR_API_KEY",
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ url, formats: ["markdown"] }),
      });
      const data = await res.json();
      if (data.success) {
        documents.push({
          text: data.data.markdown,
          metadata: { url, title: data.data.metadata?.title ?? "" },
        });
      }
    }
    return documents;
  }
}

const reader = new CRWReader();
const docs = await reader.loadData([
  "https://docs.example.com/guide",
  "https://docs.example.com/reference",
]);

const index = await VectorStoreIndex.fromDocuments(docs);
const queryEngine = index.asQueryEngine();

const response = await queryEngine.query({
  query: "What are the rate limits?",
});
console.log(response.toString());
```

## Production-Grade Implementation

### Error Handling and Retries

```
async function scrapeWithRetry(
  url: string,
  maxRetries = 3,
  delayMs = 1000,
): Promise<string | null> {
  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    try {
      const res = await fetch(`${BASE_URL}/v1/scrape`, {
        method: "POST",
        headers: {
          "Authorization": "Bearer crw_live_YOUR_API_KEY",
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ url, formats: ["markdown"] }),
        signal: AbortSignal.timeout(30_000), // 30s timeout
      });

      if (!res.ok) {
        throw new Error(`HTTP ${res.status}: ${res.statusText}`);
      }

      const data = await res.json();
      if (!data.success) throw new Error(data.error ?? "Scrape failed");
      return data.data.markdown;
    } catch (err) {
      console.warn(`Attempt ${attempt}/${maxRetries} failed for ${url}:`, err);
      if (attempt < maxRetries) {
        await new Promise((r) => setTimeout(r, delayMs * attempt)); // exponential backoff
      }
    }
  }
  return null; // give up after maxRetries
}
```

### Concurrent Crawls with Queue Management

```
import PQueue from "p-queue";

async function indexUrlsConcurrently(urls: string[], concurrency = 5) {
  const queue = new PQueue({ concurrency });
  const results: Array<{ url: string; markdown: string }> = [];

  for (const url of urls) {
    queue.add(async () => {
      const markdown = await scrapeWithRetry(url);
      if (markdown) {
        results.push({ url, markdown });
        console.log(`${results.length}/${urls.length} indexed`);
      }
    });
  }

  await queue.onIdle();
  return results;
}
```

## Keeping Your RAG Index Fresh

### Incremental Re-indexing with Map

Instead of re-crawling the full site on every update, use `/v1/map` to detect new or changed pages:

```
async function getKnownUrls(): Promise<Set<string>> {
  // Load from your database or file storage
  return new Set(await db.query("SELECT url FROM documents"));
}

async function incrementalReindex(siteUrl: string) {
  // Get current site structure (fast — no content fetch)
  const mapRes = await fetch(`${BASE_URL}/v1/map`, {
    method: "POST",
    headers: {
      "Authorization": "Bearer crw_live_YOUR_API_KEY",
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ url: siteUrl }),
  });
  const { links: currentUrls } = await mapRes.json();

  const knownUrls = await getKnownUrls();
  const newUrls = currentUrls.filter((u: string) => !knownUrls.has(u));

  if (newUrls.length === 0) {
    console.log("No new pages detected");
    return;
  }

  console.log(`Found ${newUrls.length} new pages to index`);
  const newPages = await indexUrlsConcurrently(newUrls);

  for (const page of newPages) {
    const chunks = chunkText(page.markdown);
    const embeddings = await embedChunks(chunks);
    await insertChunks(page.url, "", chunks, embeddings);
  }
}
```

## Metadata Filtering in RAG

CRW's metadata — `sourceURL`, `title`, `description` — can improve retrieval quality significantly. Store and use metadata for filtering:

```
// Store metadata alongside chunks
interface DocWithMeta {
  url: string;
  title: string;
  section: string; // extracted from URL path
  chunk: string;
  embedding: number[];
}

function extractSection(url: string): string {
  try {
    const path = new URL(url).pathname;
    return path.split("/").filter(Boolean)[0] ?? "root";
  } catch {
    return "unknown";
  }
}

// At query time, filter by section to narrow retrieval
async function queryWithFilter(question: string, section?: string, topK = 5) {
  const [qEmbedding] = await embedChunks([question]);

  let candidates = store;
  if (section) {
    candidates = store.filter((doc) => doc.section === section);
  }

  return candidates
    .map((doc) => ({ ...doc, score: cosineSim(qEmbedding, doc.embedding) }))
    .sort((a, b) => b.score - a.score)
    .slice(0, topK);
}
```

For example, if a user asks about "authentication", filtering to the `api` section before similarity search reduces noise from tutorials or blog posts that mention authentication in passing.

## Step 6: Query the RAG Pipeline

```
function cosineSim(a: number[], b: number[]) {
  const dot = a.reduce((s, v, i) => s + v * b[i], 0);
  const normA = Math.sqrt(a.reduce((s, v) => s + v * v, 0));
  const normB = Math.sqrt(b.reduce((s, v) => s + v * v, 0));
  return dot / (normA * normB);
}

async function query(question: string, topK = 5) {
  const [qEmbedding] = await embedChunks([question]);

  const ranked = store
    .map((doc) => ({ ...doc, score: cosineSim(qEmbedding, doc.embedding) }))
    .sort((a, b) => b.score - a.score)
    .slice(0, topK);

  const context = ranked
    .map((d) => `[Source: ${d.url}]
${d.chunk}`)
    .join("

---

");

  const completion = await openai.chat.completions.create({
    model: "gpt-4o-mini",
    messages: [
      {
        role: "system",
        content:
          "Answer based on the provided context. Cite source URLs when possible. Be concise.",
      },
      {
        role: "user",
        content: `Context:

${context}

Question: ${question}`,
      },
    ],
  });

  return {
    answer: completion.choices[0].message.content,
    sources: ranked.map((d) => ({ url: d.url, score: d.score })),
  };
}
```

## Why CRW Works Well for RAG

The key requirement for RAG is **clean text**. Navigation bars, cookie banners, sidebar widgets, and footers pollute your vector store with noise that hurts retrieval quality. CRW's lol-html parser is tuned for content extraction — it strips non-content elements aggressively. The result is that chunks from CRW are dense with actual content, which improves both embedding quality and retrieval precision.

The second requirement is **low latency**. If you're re-indexing a site frequently (to keep your knowledge base current), slow per-page fetches are a real bottleneck. CRW's local-first engine keeps each fetch quick, so a re-index job finishes before your next query instead of always running behind.

The third requirement is **operational simplicity**. The scraping service should be a minor component in your stack, not a major dependency. CRW's small single-binary footprint means it doesn't compete with your embedding model or vector store for memory. You can run CRW alongside everything else on the same $20/month server.

## Using fastCRW Cloud

If you don't want to self-host, [fastCRW](https://fastcrw.com) is the managed version with the same API. Just change `BASE_URL` to `https://api.fastcrw.com` and add your API key header. The code above works unchanged.

```
const BASE_URL = "https://api.fastcrw.com";
const HEADERS = {
  "Content-Type": "application/json",
  "Authorization": "Bearer YOUR_FASTCRW_API_KEY",
};
```

fastCRW's proxy network is useful when your target sites have rate limiting or bot protection — the managed infrastructure handles retry and rotation automatically.

## Frequently Asked Questions

### What is RAG?

RAG (Retrieval-Augmented Generation) is a pattern where you supplement an LLM's knowledge with retrieved documents. Instead of relying on the model's training data, you store a knowledge base as vector embeddings, retrieve the most relevant chunks at query time, and inject them into the model's context. This gives the model accurate, up-to-date information it couldn't have been trained on.

### How do I scrape a website for RAG?

Use CRW's `/v1/crawl` endpoint to fetch all pages from a documentation site as clean markdown. The markdown can be chunked and embedded directly — no HTML parsing or content cleaning needed. For a single page, use `/v1/scrape`. For URL discovery before full scraping, use `/v1/map`.

### Can CRW scrape JavaScript-heavy sites for RAG?

Yes — CRW supports JavaScript-rendered pages via LightPanda. Add `"actions": [{"type": "wait", "milliseconds": 2000}]` to your scrape request to allow time for JavaScript to execute before content extraction. For heavily dynamic sites (those requiring user interaction to reveal content), Playwright-based scrapers may be more reliable.

### What chunk size should I use?

For most documentation and article content, 400–600 words per chunk with 10–15% overlap is a good starting point. For highly technical content with lots of code, smaller chunks (200–300 words) with heading-based splitting often performs better. Measure retrieval quality on your specific content — the right chunk size varies by domain.

### How do I handle authentication for private docs?

Pass custom headers in your scrape request to authenticate with private documentation sites:

```
body: JSON.stringify({
  url: "https://internal-docs.company.com/api",
  formats: ["markdown"],
  headers: {
    "Authorization": "Bearer your-docs-token",
    "Cookie": "session=abc123",
  },
})
```

For OAuth-protected sites requiring browser-based login flows, CRW's current HTTP-based approach won't work — you'd need a Playwright-based solution for those cases.

### Is CRW good for RAG pipelines?

CRW is a good fit for RAG pipelines where the source content is standard web pages (documentation, blogs, news, Wikipedia). It produces clean markdown with minimal boilerplate, which improves embedding quality and retrieval precision compared to using raw HTML. For PDFs, scanned documents, or heavy SPAs, you may need to supplement with other extraction tools.
