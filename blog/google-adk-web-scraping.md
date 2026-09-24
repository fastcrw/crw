# Building AI Agents with Google ADK and CRW

> Use Google ADK with CRW for web scraping — learn function declarations, tool registration, and Gemini-powered scraping agents.

**Published:** 2026-04-06  
**Updated:** 2026-04-06  
**Canonical:** https://fastcrw.com/blog/google-adk-web-scraping

---

## What We're Building

A Google ADK agent that uses CRW to scrape websites, extract structured data, and produce research summaries. Google's Agent Development Kit (ADK) provides the agent framework — with built-in tool support, session management, and Gemini model integration — while CRW provides the fast, reliable scraping backend.

By the end, you'll have a Gemini-powered agent that can discover pages, scrape content, and answer questions about any website.

## Prerequisites

- CRW running locally (`docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`) or a [fastCRW](https://fastcrw.com) API key
- Python 3.11+
- A Google Cloud project with Gemini API access
- `pip install google-adk firecrawl-py`

## What is Google ADK?

Google's Agent Development Kit (ADK) is an open-source framework for building AI agents powered by Gemini models. Key concepts:

- **Agents** — autonomous entities with instructions and tools
- **Tools** — Python functions the agent can call (declared via `FunctionTool`)
- **Sessions** — persistent conversation state
- **Runners** — execute agents with streaming or batch responses

ADK handles the Gemini function-calling protocol automatically — you just write Python functions and register them as tools.

## Step 1: Define CRW Tools

Create Python functions that wrap CRW's API endpoints. ADK will expose these as tools to the Gemini model:

```
from firecrawl import FirecrawlApp

# Initialize CRW client
crw = FirecrawlApp(
    api_key="crw_live_YOUR-KEY",
    api_url="http://localhost:3000"  # or "https://api.fastcrw.com"
)

def scrape_webpage(url: str) -> dict:
    """Scrape a web page and return its content as clean markdown.

    Args:
        url: The full URL of the page to scrape.

    Returns:
        A dictionary with 'title', 'url', and 'content' keys.
    """
    result = crw.scrape_url(url, params={"formats": ["markdown"]})
    return {
        "title": result.get("metadata", {}).get("title", ""),
        "url": url,
        "content": result.get("markdown", ""),
    }

def discover_site_urls(url: str) -> dict:
    """Discover all pages on a website without downloading their content.

    Args:
        url: The base URL of the website to map.

    Returns:
        A dictionary with 'url' and 'links' keys, where links is a list of discovered URLs.
    """
    result = crw.map_url(url)
    links = result.get("links", [])
    return {
        "url": url,
        "total_found": len(links),
        "links": links[:30],  # return top 30 to stay within token limits
    }

def extract_structured_data(url: str, fields: str) -> dict:
    """Extract specific structured data from a web page.

    Args:
        url: The URL to extract data from.
        fields: Comma-separated list of fields to extract (e.g., "title,price,description").

    Returns:
        A dictionary with the extracted fields.
    """
    schema_properties = {}
    for field in fields.split(","):
        field = field.strip()
        schema_properties[field] = {"type": "string"}

    schema = {"type": "object", "properties": schema_properties}
    result = crw.scrape_url(url, params={
        "formats": ["json"],
        "jsonSchema": schema
    })
    return result.get("json", {})
```

## Step 2: Create the ADK Agent

Register the CRW tools with an ADK agent:

```
from google.adk.agents import Agent

# Create the agent with CRW tools
scraping_agent = Agent(
    name="web_research_agent",
    model="gemini-2.0-flash",
    description="An agent that can scrape and analyze web content using CRW.",
    instruction="""You are a web research agent with access to web scraping tools.

    When asked to research a topic or website:
    1. First use discover_site_urls to find all pages on the site
    2. Select the most relevant URLs based on the user's question
    3. Use scrape_webpage to get content from each relevant page
    4. Synthesize the information into a clear, structured answer

    When asked to extract specific data, use extract_structured_data.

    Always cite your sources with URLs. Be thorough but concise.""",
    tools=[scrape_webpage, discover_site_urls, extract_structured_data],
)
```

## Step 3: Run the Agent

Use ADK's runner to execute the agent:

```
from google.adk.runners import Runner
from google.adk.sessions import InMemorySessionService

# Set up session management
session_service = InMemorySessionService()

# Create a runner
runner = Runner(
    agent=scraping_agent,
    app_name="crw-research",
    session_service=session_service,
)

# Create a session
session = await session_service.create_session(
    app_name="crw-research",
    user_id="researcher-1",
)

# Run a research query
from google.genai.types import Content, Part

user_message = Content(
    role="user",
    parts=[Part(text="Research the CRW documentation site at https://docs.example.com and summarize the key features")],
)

response = await runner.run_async(
    user_id="researcher-1",
    session_id=session.id,
    new_message=user_message,
)

# Print agent's response
for event in response:
    if event.content and event.content.parts:
        for part in event.content.parts:
            if part.text:
                print(part.text)
```

## Step 4: Stream Responses

For better UX, stream the agent's responses as they arrive:

```
async for event in runner.run_async(
    user_id="researcher-1",
    session_id=session.id,
    new_message=user_message,
):
    if event.content and event.content.parts:
        for part in event.content.parts:
            if part.text:
                print(part.text, end="", flush=True)
            elif part.function_call:
                print(f"
[Calling: {part.function_call.name}({part.function_call.args})]")
            elif part.function_response:
                print(f"
[Got response from: {part.function_response.name}]")
```

## Step 5: Multi-Agent Setup

ADK supports multi-agent architectures. Create a scraper agent and a writer agent that work together:

```
from google.adk.agents import Agent

# Scraper agent — has CRW tools
scraper = Agent(
    name="scraper",
    model="gemini-2.0-flash",
    description="Scrapes websites and extracts content using CRW.",
    instruction="You scrape websites when asked. Return raw content with source URLs.",
    tools=[scrape_webpage, discover_site_urls, extract_structured_data],
)

# Writer agent — synthesizes and writes
writer = Agent(
    name="writer",
    model="gemini-2.0-flash",
    description="Writes structured reports from research data.",
    instruction="""You write clear, structured reports from provided research data.
    Use headers, bullet points, and citations. Be concise and factual.""",
)

# Orchestrator agent — delegates to scraper and writer
orchestrator = Agent(
    name="research_orchestrator",
    model="gemini-2.0-flash",
    description="Coordinates web research by delegating to scraper and writer agents.",
    instruction="""You coordinate research tasks:
    1. Delegate scraping tasks to the 'scraper' agent
    2. Once data is gathered, delegate report writing to the 'writer' agent
    3. Review the final output for completeness""",
    sub_agents=[scraper, writer],
)
```

## Real-World Example: Product Comparison Agent

```
# Complete product comparison agent
comparison_agent = Agent(
    name="product_comparison_agent",
    model="gemini-2.0-flash",
    description="Compares products by scraping their websites.",
    instruction="""You are a product comparison agent. When given product URLs:

    1. Use discover_site_urls on each product's site
    2. Find and scrape pricing pages, feature pages, and docs
    3. Use extract_structured_data for pricing info with fields: "plan_name,price,features"
    4. Produce a comparison table in markdown format

    Be objective. Only report facts from the scraped content.""",
    tools=[scrape_webpage, discover_site_urls, extract_structured_data],
)

# Use it
user_message = Content(
    role="user",
    parts=[Part(text="""Compare these web scraping APIs:
    - https://api-provider-a.com
    - https://api-provider-b.com
    Focus on pricing, features, and performance.""")],
)

response = await runner.run_async(
    user_id="analyst-1",
    session_id=session.id,
    new_message=user_message,
)
```

## Using fastCRW Instead of Self-Hosted

Switch to [fastCRW](https://fastcrw.com) by changing the API URL:

```
crw = FirecrawlApp(
    api_key="crw_live_YOUR-FASTCRW-KEY",
    api_url="https://api.fastcrw.com"
)
```

All tool functions work identically. fastCRW is ideal for production ADK agents that need reliable scraping without infrastructure management.

## Why CRW for Google ADK Agents?

**Low-latency function returns.** Gemini's function-calling flow is synchronous — the model waits for your tool to return before continuing. CRW's local-first engine keeps tool calls quick, so the agent loop stays responsive instead of stalling on slow remote round trips.

**Clean markdown for Gemini.** Gemini models work best with clean, structured text. CRW strips boilerplate HTML automatically, so the content injected into Gemini's context is dense with useful information — reducing token waste and improving response quality.

**Three endpoints cover all scraping needs.** Map for discovery, scrape for content, extract for structured data — these three tools give your ADK agent complete web access without needing multiple scraping libraries.

## Next Steps

- [Build a RAG pipeline](/blog/rag-pipeline-with-crw) with the data your agent collects
- [Use CRW's MCP server](/blog/mcp-web-scraping) for standardized tool integration
- [Compare CRW vs Firecrawl](/blog/firecrawl-vs-crawl4ai-vs-crw) for detailed performance benchmarks

## Get Started

Run CRW locally in one command:

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

Or sign up for [fastCRW](https://fastcrw.com) to skip infrastructure and start building your Google ADK agent today.
