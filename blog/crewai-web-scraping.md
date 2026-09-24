# How to Use CRW with CrewAI for Multi-Agent Web Scraping

> Build a CrewAI crew with specialized agents for web scraping and data analysis. Use crewai-crw — the CRW tool package — for fast, clean content extraction.

**Published:** 2026-04-12  
**Updated:** 2026-04-12  
**Canonical:** https://fastcrw.com/blog/crewai-web-scraping

---

## What We're Building

A CrewAI crew with two specialized agents: a **Researcher** that scrapes websites using CRW, and an **Analyst** that processes and summarizes the scraped data. CrewAI handles the multi-agent orchestration — assigning tasks, passing context between agents, and managing the workflow — while CRW provides the fast scraping backend.

This pattern is useful for competitive intelligence, market research, content aggregation, and any workflow where scraping and analysis are distinct steps that benefit from specialization.

## Prerequisites

- CRW running locally (`docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`) or a [fastCRW](https://fastcrw.com) API key
- Python 3.11+
- An LLM API key (OpenAI, Anthropic, or use Ollama for free local inference)

```
pip install crewai crewai-crw
```

That's it — two packages. `crewai-crw` is the [CRW tool package](https://pypi.org/project/crewai-crw/) for CrewAI, published on PyPI. No SDK wrappers, no `firecrawl-py` — just direct HTTP to your CRW instance.

## How CrewAI Works

CrewAI organizes work into three concepts:

- **Agents** — autonomous units with a role, goal, and backstory that guide their behavior
- **Tasks** — specific assignments given to agents, with expected outputs
- **Crew** — the team that coordinates agents and tasks in a defined process (sequential or hierarchical)

Each agent can use tools — and that's where CRW comes in. The `crewai-crw` package provides three ready-to-use tools.

## Step 1: Import CRW Tools

The `crewai-crw` package gives you three tools out of the box:

```
from crewai_crw import CrwScrapeWebsiteTool, CrwCrawlWebsiteTool, CrwMapWebsiteTool

# Managed cloud by default (api.fastcrw.com), reads CRW_API_KEY
scrape_tool = CrwScrapeWebsiteTool()
crawl_tool = CrwCrawlWebsiteTool()
map_tool = CrwMapWebsiteTool()

# Or use fastCRW cloud
scrape_tool = CrwScrapeWebsiteTool(
    api_url="https://api.fastcrw.com",
    api_key="your-api-key",
)

# Or set env vars and skip constructor args entirely
# export CRW_API_URL=https://api.fastcrw.com
# export CRW_API_KEY=your-api-key
scrape_tool = CrwScrapeWebsiteTool()  # picks up from env
```

No custom tool classes to write. No `BaseTool` subclassing. No `firecrawl-py` dependency. The tools handle API calls, error handling, and response parsing internally.

## Step 2: Define the Agents

Create two specialized agents with distinct roles:

```
from crewai import Agent

# Agent 1: The Researcher
researcher = Agent(
    role="Web Research Specialist",
    goal="Discover and scrape relevant web pages to gather comprehensive information on the given topic",
    backstory="""You are an expert web researcher. You systematically discover
    pages on target websites, identify the most relevant content, and extract
    clean text for analysis. You prioritize thoroughness and accuracy.""",
    tools=[scrape_tool, map_tool],
    verbose=True,
    max_iter=10,
)

# Agent 2: The Analyst
analyst = Agent(
    role="Data Analyst",
    goal="Analyze scraped web content and produce structured, actionable summaries",
    backstory="""You are a senior data analyst who excels at finding patterns,
    extracting key insights, and presenting information in clear, structured
    formats. You work with raw scraped content and turn it into valuable reports.""",
    verbose=True,
)
```

## Step 3: Define the Tasks

Tasks specify what each agent should do and what output to produce:

```
from crewai import Task

# Task 1: Research
research_task = Task(
    description="""Research the website {target_url}.

    Steps:
    1. Use the CRW website map tool to find all pages on the site
    2. Identify the 5-10 most relevant pages based on their URLs
    3. Scrape each relevant page using the CRW web scrape tool
    4. Compile all scraped content with source URLs

    Focus on pages that contain product information, pricing,
    documentation, or key features.""",
    expected_output="A comprehensive collection of scraped content from all relevant pages, with source URLs for each piece of content.",
    agent=researcher,
)

# Task 2: Analysis
analysis_task = Task(
    description="""Analyze the research data provided by the researcher.

    Produce a structured report with:
    1. Executive Summary (2-3 sentences)
    2. Key Findings (bullet points)
    3. Product/Service Overview
    4. Pricing Information (if found)
    5. Competitive Advantages
    6. Potential Gaps or Concerns

    Base your analysis ONLY on the scraped content — do not invent information.""",
    expected_output="A structured analysis report with sections for summary, key findings, pricing, and competitive analysis.",
    agent=analyst,
    context=[research_task],
)
```

## Step 4: Assemble and Run the Crew

```
from crewai import Crew, Process

crew = Crew(
    agents=[researcher, analyst],
    tasks=[research_task, analysis_task],
    process=Process.sequential,
    verbose=True,
)

result = crew.kickoff(inputs={"target_url": "https://docs.example.com"})
print(result)
```

The crew runs sequentially: the researcher discovers and scrapes pages, then the analyst receives all scraped content and produces the final report.

## Step 5: Hierarchical Process for Complex Workflows

For more complex scraping workflows, use a hierarchical process where a manager agent delegates work:

```
from crewai import Agent, Crew, Process

manager = Agent(
    role="Research Manager",
    goal="Coordinate the research and analysis team to produce comprehensive reports",
    backstory="You manage a team of researchers and analysts, delegating tasks and ensuring quality.",
    allow_delegation=True,
)

hierarchical_crew = Crew(
    agents=[researcher, analyst],
    tasks=[research_task, analysis_task],
    process=Process.hierarchical,
    manager_agent=manager,
    verbose=True,
)

result = hierarchical_crew.kickoff(inputs={"target_url": "https://docs.example.com"})
```

## Real-World Example: Competitive Analysis Crew

Here's a complete, copy-paste example that compares multiple competitor websites:

```
from crewai import Agent, Task, Crew, Process
from crewai_crw import CrwScrapeWebsiteTool, CrwMapWebsiteTool

# Tools — one line each, no custom classes needed
scrape_tool = CrwScrapeWebsiteTool()
map_tool = CrwMapWebsiteTool()

# Specialized agents
scraper_agent = Agent(
    role="Competitive Intelligence Researcher",
    goal="Scrape competitor websites to gather product, pricing, and feature data",
    backstory="Expert at navigating competitor sites and extracting key business information.",
    tools=[scrape_tool, map_tool],
    max_iter=15,
)

comparison_agent = Agent(
    role="Competitive Analyst",
    goal="Compare competitors across key dimensions and identify advantages/gaps",
    backstory="Seasoned analyst who turns raw competitor data into strategic insights.",
)

# Tasks
scrape_competitors = Task(
    description="""Scrape the following competitor websites:
    {competitors}

    For each competitor, gather:
    - Product features and capabilities
    - Pricing information
    - Key messaging and positioning
    - Any technical specifications""",
    expected_output="Organized scraped content from each competitor website with labeled sections.",
    agent=scraper_agent,
)

compare_competitors = Task(
    description="""Create a competitive comparison matrix from the scraped data.

    Format as a table comparing:
    | Feature | Competitor A | Competitor B | Competitor C |

    Include sections for: Features, Pricing, Strengths, Weaknesses""",
    expected_output="A structured competitive comparison matrix with clear winner/loser annotations per feature.",
    agent=comparison_agent,
    context=[scrape_competitors],
)

crew = Crew(
    agents=[scraper_agent, comparison_agent],
    tasks=[scrape_competitors, compare_competitors],
    process=Process.sequential,
)

result = crew.kickoff(inputs={
    "competitors": "https://competitor1.com, https://competitor2.com, https://competitor3.com"
})
print(result)
```

## Configuration Options

Each tool accepts configuration to fine-tune behavior:

```
# Scrape with custom settings
scrape_tool = CrwScrapeWebsiteTool(
    config={
        "formats": ["markdown", "links"],
        "onlyMainContent": True,
        "renderJs": True,           # force JS rendering
        "waitFor": 2000,            # wait 2s after JS load
    }
)

# Crawl with a page limit
crawl_tool = CrwCrawlWebsiteTool(
    config={
        "limit": 50,
        "scrapeOptions": {
            "formats": ["markdown"],
            "onlyMainContent": True,
        },
    }
)

# Map with sitemap discovery
map_tool = CrwMapWebsiteTool(
    config={
        "limit": 100,
        "useSitemap": True,
    }
)
```

## Using fastCRW Cloud

Switch to the managed [fastCRW](https://fastcrw.com) cloud service — no self-hosting required:

```
# Option 1: Constructor args
scrape_tool = CrwScrapeWebsiteTool(
    api_url="https://api.fastcrw.com",
    api_key="your-api-key",
)

# Option 2: Environment variables (recommended)
# export CRW_API_URL=https://api.fastcrw.com
# export CRW_API_KEY=your-api-key
scrape_tool = CrwScrapeWebsiteTool()  # auto-picks from env
```

All tools and agents work identically. fastCRW is particularly useful for CrewAI crews that scrape many different sites — the managed infrastructure handles scaling and reliability for you.

## Why CRW for CrewAI?

**Zero boilerplate.** Install `crewai-crw`, import the tools, and use them. No custom `BaseTool` subclasses, no SDK wrappers, no `firecrawl-py` dependency.

**Low-latency tool responses keep agents on track.** CrewAI agents have iteration limits. Slow scrape calls burn through iterations waiting for responses. CRW's local-first engine keeps tool calls quick, so you get more useful iterations within the same budget.

**Clean output reduces agent confusion.** When an agent receives raw HTML or noisy content, it wastes tokens parsing irrelevant content and often makes mistakes. CRW returns clean markdown that agents can reason about directly.

**Self-hosted or cloud — your choice.** Run CRW locally for free during development, switch to fastCRW for production. Same tools, same code, different URL.

## Next Steps

- [crewai-crw on PyPI](https://pypi.org/project/crewai-crw/) — full documentation and source code
- [GitHub: us/crewai-crw](https://github.com/us/crewai-crw) — report issues, contribute
- [Build a RAG pipeline](/blog/rag-pipeline-with-crw) to make your scraped data searchable
- [Use CRW's MCP server](/blog/mcp-web-scraping) for direct agent tool integration
- [Compare CRW vs Firecrawl](/blog/firecrawl-vs-crawl4ai-vs-crw) performance and features

## Get Started

```
pip install crewai crewai-crw
```

Run CRW locally in one command:

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

Or sign up for [fastCRW](https://fastcrw.com) to skip infrastructure setup and start building your CrewAI crew today.
