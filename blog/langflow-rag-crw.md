# How to Build a RAG Chatbot with Langflow and CRW

> Build a visual RAG chatbot pipeline in Langflow using CRW as the web scraping data source — no coding required.

**Published:** 2026-04-20  
**Updated:** 2026-04-20  
**Canonical:** https://fastcrw.com/blog/langflow-rag-crw

---

## What We're Building

A fully functional RAG (Retrieval-Augmented Generation) chatbot built entirely in **Langflow**'s visual interface. The pipeline: (1) scrapes website content using CRW's API, (2) chunks and embeds the text, (3) stores embeddings in a vector database, and (4) answers user questions with source-grounded responses — all by dragging and connecting nodes.

Langflow is an open-source visual framework for building AI workflows. Combined with CRW's fast web scraping API, you can build a knowledge-base chatbot from any website in under 30 minutes.

## Prerequisites

- Langflow installed: `pip install langflow` then `langflow run` (opens at `http://localhost:7860`)
- CRW running locally (`docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`) or a [fastCRW](https://fastcrw.com) cloud API key
- An OpenAI API key (for embeddings and chat completion)

## Architecture Overview

Our Langflow flow has two phases:

- **Ingestion phase:** CRW API → Text Splitter → Embedding Model → Vector Store
- **Query phase:** User Question → Embedding → Vector Search → Prompt Template → LLM → Response

Langflow lets you build both phases as a single visual flow. Let's assemble it step by step.

## Step 1: Create the CRW Data Source

Langflow doesn't have a built-in CRW component, but its **API Request** node handles any REST API. Here's how to set it up:

### Add an API Request Node

Drag an **"API Request"** node onto the canvas. Configure it:

- **Method:** POST
- **URL:** `http://localhost:3000/v1/scrape` (or `https://api.fastcrw.com/v1/scrape` for cloud)
- **Headers:** `{"Content-Type": "application/json", "Authorization": "Bearer crw_live_YOUR_API_KEY"}`
- **Body:**

```
{
  "url": "https://docs.example.com",
  "formats": ["markdown"]
}
```

This node calls CRW's scrape endpoint and returns clean markdown content. The response includes `data.markdown` — the extracted page content stripped of navigation, ads, and boilerplate.

### For Multiple Pages: Use the Crawl Endpoint

To ingest an entire documentation site, use a two-step approach with CRW's crawl API:

```
// Step 1: Start crawl
POST http://localhost:3000/v1/crawl
{
  "url": "https://docs.example.com",
  "limit": 50,
  "scrapeOptions": { "formats": ["markdown"] }
}

// Step 2: Poll for results
GET http://localhost:3000/v1/crawl/{job_id}
```

For simplicity in Langflow, you can also create a small Python script that calls CRW's crawl endpoint, waits for completion, and outputs the combined markdown. Then use Langflow's **Python Function** node to run it:

```
import requests

def crawl_site(url: str, base_url: str = "http://localhost:3000") -> str:
    # Start crawl
    start = requests.post(
        f"{base_url}/v1/crawl",
        json={"url": url, "limit": 50, "scrapeOptions": {"formats": ["markdown"]}},
        headers={"Authorization": "Bearer crw_live_YOUR_API_KEY"},
    )
    job_id = start.json()["id"]

    # Poll until complete
    while True:
        time.sleep(2)
        status = requests.get(f"{base_url}/v1/crawl/{job_id}").json()
        if status["status"] == "completed":
            return "

---

".join(
                page["markdown"] for page in status["data"] if page.get("markdown")
            )
        if status["status"] == "failed":
            raise Exception("Crawl failed")
```

## Step 2: Add the Text Splitter

Large documents need to be split into smaller chunks for effective embedding and retrieval. Drag a **"Recursive Character Text Splitter"** node onto the canvas:

- **Chunk Size:** 1000 characters
- **Chunk Overlap:** 100 characters
- **Separators:** `[" ", " ", ". ", " "]`

Connect the output of your CRW API Request node (or Python Function node) to the text splitter's input. The splitter produces a list of text chunks, each small enough for the embedding model's context window.

## Step 3: Set Up Embeddings

Drag an **"OpenAI Embeddings"** node onto the canvas. Enter your OpenAI API key and select the model:

- **Model:** `text-embedding-3-small` (good balance of quality and cost)
- **API Key:** Your OpenAI key

This node converts each text chunk into a vector — a numerical representation that captures the chunk's meaning.

## Step 4: Configure the Vector Store

Langflow supports multiple vector stores. For a quick local setup, use **Chroma**:

- Drag a **"Chroma"** node onto the canvas
- **Collection Name:** `website-docs`
- **Persist Directory:** `./chroma_data` (so data survives restarts)

Connect: Text Splitter → Chroma (documents input), OpenAI Embeddings → Chroma (embedding input).

Other vector store options in Langflow include:

- **Pinecone:** Managed cloud vector DB — great for production
- **Qdrant:** Open-source, high-performance vector search
- **pgvector:** PostgreSQL extension — use your existing database

## Step 5: Build the Query Chain

Now we build the retrieval and generation pipeline that answers user questions.

### Chat Input

Drag a **"Chat Input"** node — this is where users type their questions.

### Retriever

Add a **"Vector Store Retriever"** node. Connect it to your Chroma node. Set:

- **Search Type:** Similarity
- **Number of Results (k):** 5

This retrieves the 5 most relevant chunks from your vector store based on the user's question.

### Prompt Template

Drag a **"Prompt"** node and configure the template:

```
You are a helpful assistant that answers questions based on the provided context.
Use only the information from the context to answer. If the answer is not in the
context, say "I don't have enough information to answer that."

Context:
{context}

Question: {question}

Answer:
```

Connect the retriever output to the `{context}` variable and the chat input to the `{question}` variable.

### LLM

Add an **"OpenAI"** chat model node:

- **Model:** `gpt-4o-mini` (fast and cost-effective)
- **Temperature:** 0.1 (low for factual answers)
- **API Key:** Your OpenAI key

Connect the prompt template output to the LLM input.

### Chat Output

Finally, add a **"Chat Output"** node and connect the LLM's response to it. This displays the answer in Langflow's built-in chat interface.

## Step 6: Run and Test

Your complete flow should now look like this:

**Ingestion:** CRW API → Text Splitter → [OpenAI Embeddings] → Chroma

**Query:** Chat Input → [Chroma Retriever] → Prompt Template → OpenAI LLM → Chat Output

Click the **"Run"** button (play icon) in Langflow's toolbar. The ingestion phase runs first — CRW scrapes the target website, the text splitter chunks the content, and Chroma stores the embeddings. Then the chat interface becomes active.

Try asking questions about the scraped content:

- "What are the main features?"
- "How do I get started?"
- "What are the API endpoints?"

## Adding Source Citations

To show users where answers come from, modify the prompt template to include source URLs:

```
You are a helpful assistant. Answer the question using only the provided context.
After your answer, list the source URLs you used.

Context (with sources):
{context}

Question: {question}

Answer (with sources):
```

CRW includes `metadata.sourceURL` in its responses. When you store chunks in the vector store, include the URL as metadata. The retriever passes this metadata through to the prompt, enabling the LLM to cite its sources.

## Keeping Content Fresh

Static knowledge bases go stale. Use CRW's `/v1/map` endpoint to detect new pages and re-ingest only what's changed:

```
def check_for_updates(site_url: str, known_urls: set) -> list:
    res = requests.post(
        "http://localhost:3000/v1/map",
        json={"url": site_url},
        headers={"Authorization": "Bearer crw_live_YOUR_API_KEY"},
    )
    current_urls = set(res.json().get("links", []))
    new_urls = current_urls - known_urls
    return list(new_urls)
```

Schedule this check with Langflow's webhook trigger or an external cron job. When new pages are found, re-run the ingestion flow for just those URLs.

## Production Tips

- **Chunk size tuning:** Start with 1000 characters and adjust. Smaller chunks (500) give more precise retrieval but may lack context. Larger chunks (1500) provide more context but may include irrelevant information.
- **Embedding model:** `text-embedding-3-small` is a good default. For higher quality, try `text-embedding-3-large` — it costs more but improves retrieval accuracy.
- **Re-ranking:** Add a Cohere Rerank node between the retriever and prompt for better result ordering. This is especially helpful when you increase `k` to 10+ results.
- **Memory:** Add a **"Conversation Memory"** node to enable multi-turn conversations. Connect it to the prompt template so the LLM can reference previous exchanges.
- **Export:** Langflow flows can be exported as JSON and version-controlled. Export your flow via File → Export and commit it to your repository.

## Self-Hosted vs. Cloud

Both CRW and Langflow can run either self-hosted or in the cloud:

| Component | Self-Hosted | Cloud |
| --- | --- | --- |
| CRW | `docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest` | [fastCRW](https://fastcrw.com) — managed API |
| Langflow | `pip install langflow && langflow run` | [DataStax Langflow](https://astra.datastax.com/langflow) — managed hosting |
| Vector Store | Chroma (local) or pgvector | Pinecone or Qdrant Cloud |

For a fully local setup with no external dependencies, use self-hosted CRW + local Langflow + Chroma + Ollama (for embeddings and LLM). This gives you a completely private RAG pipeline.

## Conclusion

Langflow + CRW makes building RAG chatbots accessible to anyone — no AI engineering experience required. Langflow's visual interface handles the pipeline complexity, while CRW delivers clean, structured content from any website.

For a code-first approach to RAG with CRW, check out our [RAG pipeline guide](/blog/rag-pipeline-with-crw). To learn more about CRW's AI agent integration, see our [MCP server guide](/blog/mcp-web-scraping).

Ready to start? [Self-host CRW](https://github.com/fastcrw/crw) for free or get a [fastCRW](https://fastcrw.com) cloud API key in seconds.
