# How to Use CRW with OpenAI Agents SDK for Web-Aware AI

> Integrate CRW as a tool in OpenAI's Agents SDK. Build web-aware agents with function calling, handoffs, and real-time web scraping capabilities.

**Published:** 2026-04-17  
**Updated:** 2026-04-17  
**Canonical:** https://fastcrw.com/blog/openai-agents-sdk-crw

---

## What We're Building

A web-aware AI agent using OpenAI's Agents SDK with CRW as its scraping backend. The agent can browse websites, extract content, and answer questions based on live web data — not just its training knowledge. We'll cover tool definitions, the agent loop, guardrails, and handoffs between specialized agents.

OpenAI's Agents SDK provides the orchestration layer (tool calling, agent handoffs, tracing), while CRW handles the actual web scraping with sub-second response times.

## Prerequisites

- CRW running locally (`docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`) or a [fastCRW](https://fastcrw.com) API key
- Python 3.11+
- An OpenAI API key
- `pip install openai-agents firecrawl-py`

## How OpenAI Agents SDK Works

The Agents SDK provides a lightweight framework for building agentic applications:

- **Agents** — LLMs with instructions and tools
- **Tools** — Python functions decorated with `@function_tool`
- **Handoffs** — agents can delegate to other agents
- **Guardrails** — input/output validation for safety
- **Runner** — executes the agent loop until completion

## Step 1: Define CRW Tools

Create function tools that wrap CRW's scraping endpoints:

```
from agents import function_tool
from firecrawl import FirecrawlApp

# Initialize CRW
crw = FirecrawlApp(
    api_key="crw_live_YOUR-KEY",
    api_url="http://localhost:3000"  # or "https://api.fastcrw.com"
)

@function_tool
def scrape_page(url: str) -> str:
    """Scrape a web page and return its content as clean markdown.
    Use this to get the full text content of any URL.

    Args:
        url: The full URL to scrape (e.g., https://example.com/page)
    """
    result = crw.scrape_url(url, params={"formats": ["markdown"]})
    title = result.get("metadata", {}).get("title", "")
    markdown = result.get("markdown", "")
    return f"# {title}
Source: {url}

{markdown}"

@function_tool
def discover_urls(url: str) -> str:
    """Discover all pages on a website. Returns a list of URLs found
    on the site without downloading their content.

    Args:
        url: The base URL of the website to explore
    """
    result = crw.map_url(url)
    links = result.get("links", [])
    return f"Found {len(links)} pages:
" + "
".join(links[:30])

@function_tool
def extract_data(url: str, fields: str) -> str:
    """Extract specific structured data from a web page.

    Args:
        url: The URL to extract data from
        fields: Comma-separated list of fields to extract (e.g., "price,title,description")
    """

    schema_props = {}
    for field in fields.split(","):
        schema_props[field.strip()] = {"type": "string"}

    result = crw.scrape_url(url, params={
        "formats": ["json"],
        "jsonSchema": {"type": "object", "properties": schema_props}
    })
    return json.dumps(result.get("json", {}), indent=2)
```

## Step 2: Create the Agent

```
from agents import Agent

web_agent = Agent(
    name="Web Research Agent",
    instructions="""You are a web research agent with access to real-time web scraping tools.

    When answering questions:
    1. If the user asks about a specific website, use discover_urls first to understand its structure
    2. Scrape the most relevant pages using scrape_page
    3. For specific data points (prices, features, etc.), use extract_data
    4. Synthesize the scraped content into a clear, cited answer

    Always cite your sources with URLs. If scraping fails, explain what happened and suggest alternatives.
    Never make up information — only report what you found on the web.""",
    tools=[scrape_page, discover_urls, extract_data],
)
```

## Step 3: Run the Agent

```
from agents import Runner

result = await Runner.run(
    web_agent,
    "What are the main features and pricing of the product at https://example.com?",
)

print(result.final_output)
```

## Step 4: Add Agent Handoffs

For complex workflows, create specialized agents and use handoffs:

```
from agents import Agent

# Scraper agent — focused on data gathering
scraper_agent = Agent(
    name="Scraper",
    instructions="""You are a web scraping specialist. When asked to research a topic:
    1. Discover URLs on the target site
    2. Scrape the most relevant pages
    3. Return the raw scraped content with source URLs

    Do NOT analyze or summarize — just gather data and hand off to the analyst.""",
    tools=[scrape_page, discover_urls, extract_data],
    handoffs=["analyst"],  # can delegate to analyst
)

# Analyst agent — focused on synthesis
analyst_agent = Agent(
    name="Analyst",
    instructions="""You are a data analyst. You receive scraped web content and produce
    structured reports. Organize findings with headers, bullet points, and citations.
    Be concise and factual — only report what's in the provided data.""",
    handoffs=["scraper"],  # can request more data from scraper
)

# Triage agent — routes to the right specialist
triage_agent = Agent(
    name="Triage",
    instructions="""You are a triage agent. Route user requests:
    - Questions about websites or web content → hand off to Scraper
    - Questions about analyzing data → hand off to Analyst
    - Simple questions → answer directly""",
    handoffs=["scraper", "analyst"],
)

# Run with triage as entry point
result = await Runner.run(
    triage_agent,
    "Research https://docs.example.com and give me a summary of their API endpoints",
)
```

## Step 5: Add Guardrails

Prevent the agent from scraping sensitive or disallowed sites:

```
from agents import GuardrailFunctionOutput, input_guardrail, Agent

BLOCKED_DOMAINS = ["internal.company.com", "admin.example.com", "localhost"]

@input_guardrail
async def url_safety_check(ctx, agent, input_text: str) -> GuardrailFunctionOutput:
    """Check that the user isn't asking to scrape blocked domains."""
    for domain in BLOCKED_DOMAINS:
        if domain in input_text.lower():
            return GuardrailFunctionOutput(
                output_info={"blocked_domain": domain},
                tripwire_triggered=True,
            )
    return GuardrailFunctionOutput(
        output_info={"status": "safe"},
        tripwire_triggered=False,
    )

# Agent with guardrails
safe_web_agent = Agent(
    name="Safe Web Agent",
    instructions="You are a web research agent. Only scrape public websites.",
    tools=[scrape_page, discover_urls, extract_data],
    input_guardrails=[url_safety_check],
)
```

## Step 6: Tracing and Debugging

The Agents SDK includes built-in tracing to debug agent behavior:

```
from agents import Runner, trace

with trace("research-session"):
    result = await Runner.run(
        web_agent,
        "Scrape https://docs.example.com and summarize the getting started guide",
    )

# Traces show:
# - Each tool call (which URLs were scraped)
# - Token usage per step
# - Handoff decisions
# - Total execution time
```

## Real-World Example: Competitive Monitor

Build an agent that monitors competitor websites for changes:

```
from agents import Agent, function_tool, Runner

@function_tool
def scrape_and_compare(url: str, previous_content: str) -> str:
    """Scrape a page and compare it to previous content.

    Args:
        url: The URL to check for changes
        previous_content: The previously scraped content to compare against
    """
    result = crw.scrape_url(url, params={"formats": ["markdown"]})
    current = result.get("markdown", "")

    if not previous_content:
        return json.dumps({"status": "first_scrape", "content": current})

    if current == previous_content:
        return json.dumps({"status": "no_changes"})

    return json.dumps({
        "status": "changed",
        "current_content": current,
        "content_length_diff": len(current) - len(previous_content),
    })

monitor_agent = Agent(
    name="Competitor Monitor",
    instructions="""You monitor competitor websites for changes.
    When given URLs, scrape them and report any differences from previous versions.
    Highlight new features, pricing changes, or messaging updates.""",
    tools=[scrape_page, discover_urls, scrape_and_compare],
)

result = await Runner.run(
    monitor_agent,
    "Check https://competitor.com/pricing for any changes. Previous content was empty (first check).",
)
print(result.final_output)
```

## Using fastCRW Instead of Self-Hosted

Switch to [fastCRW](https://fastcrw.com) cloud by changing one line:

```
crw = FirecrawlApp(
    api_key="crw_live_YOUR-FASTCRW-KEY",
    api_url="https://api.fastcrw.com"
)
```

All tools and agents work identically. fastCRW manages infrastructure, proxies, and scaling — so you can focus on your agent logic.

## Why CRW for OpenAI Agents?

**Low-latency tool responses.** The Agents SDK runs a synchronous loop — each tool call blocks until it returns. CRW's local-first engine keeps each call quick, so a multi-tool research session stays responsive instead of stalling on slow remote round trips.

**Clean output reduces hallucination.** When tool output is noisy (raw HTML, navigation elements, ads), the model is more likely to hallucinate or miss key information. CRW returns clean markdown — only the content that matters — which improves response accuracy.

**Firecrawl-compatible.** If you're already using Firecrawl with OpenAI function calling, switching to CRW is a one-line change (`api_url`). Your tool definitions and agent configuration stay the same.

## Next Steps

- [Build a RAG pipeline](/blog/rag-pipeline-with-crw) to store and retrieve scraped content
- [Use CRW's MCP server](/blog/mcp-web-scraping) for standardized agent tool integration
- [Compare CRW vs Firecrawl](/blog/firecrawl-vs-crawl4ai-vs-crw) performance and features

## Get Started

Run CRW locally in one command:

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

Or sign up for [fastCRW](https://fastcrw.com) to skip infrastructure and start building web-aware OpenAI agents immediately.
