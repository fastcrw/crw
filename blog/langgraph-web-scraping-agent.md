# How to Build a Web Scraping Agent with LangGraph and CRW

> Build a web scraping agent with LangGraph and CRW — graph-based orchestration, state management, and conditional routing.

**Published:** 2026-04-27  
**Updated:** 2026-04-27  
**Canonical:** https://fastcrw.com/blog/langgraph-web-scraping-agent

---

## What We're Building

A LangGraph agent that autonomously scrapes websites, extracts structured data, and makes decisions about what to scrape next. The agent uses CRW as its scraping backend — getting clean markdown from any URL in under a second — while LangGraph handles the orchestration: state management, tool routing, and conditional branching.

By the end of this tutorial, you'll have a working agent that can: (1) discover pages on a target site using CRW's `/v1/map` endpoint, (2) scrape and extract content from selected pages, (3) analyze the content and decide whether to scrape more pages, and (4) compile a structured report from all gathered data.

## Prerequisites

- CRW running locally (`docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`) or a [fastCRW](https://fastcrw.com) API key
- Python 3.11+
- An OpenAI API key (for the LLM powering the agent)
- `pip install langgraph langchain-openai "firecrawl-py>=1,<2"` — the snippets below use the v1 SDK shape (`FirecrawlApp`, `scrape_url(..., params=...)`); v2 renamed the client and changed method signatures

## Why LangGraph for Web Scraping Agents?

LangGraph models agent logic as a directed graph. Each node is a function, and edges define the flow between them. This is a natural fit for scraping workflows where you need to:

- **Branch conditionally** — scrape more pages or stop based on what you've found
- **Maintain state** — accumulate scraped data across multiple tool calls
- **Retry on failure** — route back to a scrape node if a page fails
- **Human-in-the-loop** — pause for approval before scraping sensitive sites

Compared to a simple ReAct loop, LangGraph gives you explicit control over the agent's execution path, making it easier to debug and reason about.

## Step 1: Define the Agent State

LangGraph agents operate on a shared state object. Define what the agent needs to track:

```
from typing import TypedDict, Annotated
from langgraph.graph.message import add_messages
from langchain_core.messages import BaseMessage

class AgentState(TypedDict):
    messages: Annotated[list[BaseMessage], add_messages]
    target_url: str
    discovered_urls: list[str]
    scraped_pages: list[dict]
    report: str
```

The `messages` field uses LangGraph's built-in message reducer so chat history accumulates automatically. The other fields track our scraping progress.

## Step 2: Create CRW Scraping Tools

Define tools that the agent can call. We'll use the Firecrawl Python SDK (v1.x) pointed at CRW — CRW exposes a Firecrawl-compatible API, so the same client works against your CRW base URL:

```
from firecrawl import FirecrawlApp
from langchain_core.tools import tool

# Point at your CRW instance
crw = FirecrawlApp(
    api_key="crw_live_YOUR-KEY",
    api_url="http://localhost:3000"  # or "https://api.fastcrw.com"
)

@tool
def discover_urls(url: str) -> list[str]:
    """Discover all URLs on a website using CRW's map endpoint."""
    result = crw.map_url(url)
    return result.get("links", [])[:50]  # limit to 50 URLs

@tool
def scrape_page(url: str) -> dict:
    """Scrape a single page and return clean markdown content."""
    result = crw.scrape_url(url, params={"formats": ["markdown"]})
    return {
        "url": url,
        "title": result.get("metadata", {}).get("title", ""),
        "markdown": result.get("markdown", ""),
    }

@tool
def extract_data(url: str, schema: dict) -> dict:
    """Extract structured data from a page using CRW's JSON extraction."""
    result = crw.scrape_url(url, params={
        "formats": ["json"],
        "jsonSchema": schema
    })
    return result.get("json", {})
```

## Step 3: Build the Agent Graph

Now wire the tools into a LangGraph graph with conditional routing:

```
from langgraph.graph import StateGraph, END
from langgraph.prebuilt import ToolNode
from langchain_openai import ChatOpenAI

# Initialize the LLM with tools
llm = ChatOpenAI(model="gpt-4o", temperature=0)
tools = [discover_urls, scrape_page, extract_data]
llm_with_tools = llm.bind_tools(tools)

# Define graph nodes
def agent_node(state: AgentState) -> dict:
    """The LLM decides what to do next."""
    system_prompt = f"""You are a web scraping agent. Your goal is to gather
    information from {state['target_url']}.

    Strategy:
    1. First discover URLs on the target site
    2. Select the most relevant pages to scrape
    3. Scrape each page for content
    4. When you have enough data, compile a report

    Pages scraped so far: {len(state['scraped_pages'])}
    """
    messages = [{"role": "system", "content": system_prompt}] + state["messages"]
    response = llm_with_tools.invoke(messages)
    return {"messages": [response]}

def compile_report(state: AgentState) -> dict:
    """Compile all scraped data into a final report."""
    pages = state["scraped_pages"]
    report_parts = []
    for page in pages:
        report_parts.append(f"## {page['title']}
Source: {page['url']}

{page['markdown'][:500]}")
    report = "

---

".join(report_parts)
    return {"report": report}

# Routing function
def should_continue(state: AgentState) -> str:
    last_message = state["messages"][-1]
    if hasattr(last_message, "tool_calls") and last_message.tool_calls:
        return "tools"
    return "compile"

# Build the graph
tool_node = ToolNode(tools)

graph = StateGraph(AgentState)
graph.add_node("agent", agent_node)
graph.add_node("tools", tool_node)
graph.add_node("compile", compile_report)

graph.set_entry_point("agent")
graph.add_conditional_edges("agent", should_continue, {
    "tools": "tools",
    "compile": "compile",
})
graph.add_edge("tools", "agent")
graph.add_edge("compile", END)

app = graph.compile()
```

## Step 4: Visualize the Graph

LangGraph can render the agent's execution graph, which is helpful for debugging:

```
# Print ASCII representation
app.get_graph().print_ascii()

# Output:
#   +---------+
#   | agent   |
#   +---------+
#     /     #    v       v
# +-------+ +---------+
# | tools | | compile |
# +-------+ +---------+
#    |            |
#    v            v
# +-------+   +-----+
# | agent |   | END |
# +-------+   +-----+
```

## Step 5: Run the Agent

```
from langchain_core.messages import HumanMessage

result = app.invoke({
    "messages": [HumanMessage(content="Research this website and gather key information")],
    "target_url": "https://docs.example.com",
    "discovered_urls": [],
    "scraped_pages": [],
    "report": "",
})

print(result["report"])
```

## Step 6: Add State Persistence

For long-running scraping jobs, persist the agent's state so it can resume after interruptions:

```
from langgraph.checkpoint.memory import MemorySaver

checkpointer = MemorySaver()
app = graph.compile(checkpointer=checkpointer)

# Run with a thread ID for persistence
config = {"configurable": {"thread_id": "scrape-job-1"}}

result = app.invoke({
    "messages": [HumanMessage(content="Scrape the documentation site")],
    "target_url": "https://docs.example.com",
    "discovered_urls": [],
    "scraped_pages": [],
    "report": "",
}, config=config)

# Resume later — state is preserved
result = app.invoke({
    "messages": [HumanMessage(content="Now scrape the API reference section too")],
}, config=config)
```

## Step 7: Stream Agent Progress

For real-time visibility into what the agent is doing, use LangGraph's streaming:

```
async for event in app.astream_events(
    {
        "messages": [HumanMessage(content="Research this website")],
        "target_url": "https://docs.example.com",
        "discovered_urls": [],
        "scraped_pages": [],
        "report": "",
    },
    version="v2",
):
    if event["event"] == "on_tool_start":
        print(f"🔧 Calling: {event['name']}({event['data']['input']})")
    elif event["event"] == "on_tool_end":
        print(f"✅ Result: {str(event['data']['output'])[:200]}")
```

## Advanced: Multi-Site Comparison Agent

Extend the agent to compare data across multiple sites — useful for competitive analysis or price monitoring:

```
class ComparisonState(TypedDict):
    messages: Annotated[list[BaseMessage], add_messages]
    sites: list[str]
    site_data: dict[str, list[dict]]
    comparison_report: str

@tool
def scrape_multiple(urls: list[str]) -> list[dict]:
    """Scrape multiple pages in sequence using CRW."""
    results = []
    for url in urls:
        try:
            result = crw.scrape_url(url, params={"formats": ["markdown"]})
            results.append({
                "url": url,
                "title": result.get("metadata", {}).get("title", ""),
                "markdown": result.get("markdown", ""),
            })
        except Exception as e:
            results.append({"url": url, "error": str(e)})
    return results
```

## Using fastCRW Instead of Self-Hosted

To use the managed [fastCRW](https://fastcrw.com) cloud service instead of self-hosting, just change the API URL:

```
crw = FirecrawlApp(
    api_key="crw_live_YOUR-FASTCRW-KEY",
    api_url="https://api.fastcrw.com"
)
```

Everything else stays exactly the same. fastCRW handles infrastructure and scaling — so your agent can focus on the logic.

## Why CRW for LangGraph Agents?

**Latency matters for agents.** When an LLM agent makes a tool call, the user is waiting. CRW's local-first engine keeps each call quick, so your agent stays responsive across multi-page research tasks instead of stalling on slow remote round trips.

**Clean markdown improves agent reasoning.** CRW strips navigation, ads, and boilerplate automatically. The LLM sees only the content that matters, which reduces token usage and improves the quality of the agent's decisions.

**Firecrawl SDK compatibility.** CRW works with the existing Firecrawl Python SDK — just change the `api_url`. If you have existing LangChain/LangGraph code using Firecrawl, switching to CRW is a one-line change.

## Next Steps

- [Build a RAG pipeline](/blog/rag-pipeline-with-crw) with the data your agent collects
- [Use CRW's MCP server](/blog/mcp-web-scraping) for direct AI agent integration
- [Compare CRW vs Firecrawl](/blog/firecrawl-vs-crawl4ai-vs-crw) in detail

## Get Started

Run CRW locally in one command:

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

Or sign up for [fastCRW](https://fastcrw.com) to skip the infrastructure and start building your LangGraph agent immediately.
