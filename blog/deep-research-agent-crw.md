# How to Build a Deep Research Agent with CRW

> Build a deep research agent that searches, scrapes, and synthesizes findings into structured reports using CRW's scraping API.

**Published:** 2026-04-02  
**Updated:** 2026-04-02  
**Canonical:** https://fastcrw.com/blog/deep-research-agent-crw

---

## What We're Building

A deep research agent that autonomously researches any topic by: (1) searching the web for relevant sources, (2) discovering pages on found sites using CRW's `/v1/map`, (3) scraping and extracting content with `/v1/scrape`, (4) extracting structured data with `/v1/scrape` using `formats: ["json"]` and a `jsonSchema`, and (5) synthesizing everything into a comprehensive research report with citations.

Unlike simple RAG pipelines that work with a fixed corpus, this agent actively explores the web — following leads, drilling into promising sources, and iterating until it has enough information to produce a thorough answer.

## Prerequisites

- CRW running locally (`docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`) or a [fastCRW](https://fastcrw.com) API key
- Python 3.11+
- An OpenAI API key
- `pip install openai firecrawl-py`

## Architecture Overview

The research agent follows a loop: **Plan → Search → Scrape → Analyze → Decide → Repeat or Report**. Each iteration deepens the agent's understanding until it decides it has enough information.

```
# The research loop:
#
#  ┌─────────┐
#  │  Plan   │ ← Break research question into sub-questions
#  └────┬────┘
#       ▼
#  ┌─────────┐
#  │ Search  │ ← Find relevant URLs (map endpoint)
#  └────┬────┘
#       ▼
#  ┌─────────┐
#  │ Scrape  │ ← Get clean content (scrape endpoint)
#  └────┬────┘
#       ▼
#  ┌─────────┐
#  │ Analyze │ ← Extract key findings, identify gaps
#  └────┬────┘
#       ▼
#  ┌─────────┐
#  │ Decide  │ ← Enough info? → Report. Gaps? → Loop back.
#  └─────────┘
```

## Step 1: Set Up the CRW Client

```
from firecrawl import FirecrawlApp

# CRW client — self-hosted or fastCRW
crw = FirecrawlApp(
    api_key="crw_live_YOUR-KEY",
    api_url="http://localhost:3000"  # or "https://api.fastcrw.com"
)

client = openai.OpenAI()
```

## Step 2: Build the Research Planner

The planner breaks a high-level research question into specific sub-questions:

```
def plan_research(question: str, existing_findings: str = "") -> list[str]:
    """Break a research question into sub-questions."""
    prompt = f"""You are a research planner. Break this research question into
    3-5 specific sub-questions that can be answered by scraping web pages.

    Research question: {question}

    {"Existing findings (avoid duplicating these):" + existing_findings if existing_findings else ""}

    Return a JSON array of sub-questions. Example:
    ["What is X's pricing model?", "How does X compare to Y?", "What are the technical requirements?"]"""

    response = client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": prompt}],
        response_format={"type": "json_object"},
    )
    result = json.loads(response.choices[0].message.content)
    return result.get("questions", result.get("sub_questions", []))
```

## Step 3: URL Discovery with Map

Use CRW's `/v1/map` endpoint to find relevant pages without downloading their full content:

```
def discover_sources(seed_urls: list[str]) -> list[str]:
    """Discover relevant URLs from seed sites using CRW's map endpoint."""
    all_urls = []
    for url in seed_urls:
        try:
            result = crw.map_url(url)
            links = result.get("links", [])
            all_urls.extend(links)
        except Exception as e:
            print(f"Map failed for {url}: {e}")
    # Deduplicate while preserving order
    seen = set()
    unique = []
    for url in all_urls:
        if url not in seen:
            seen.add(url)
            unique.append(url)
    return unique
```

## Step 4: Intelligent URL Selection

Not all discovered URLs are worth scraping. Use the LLM to select the most relevant ones:

```
def select_urls(urls: list[str], question: str, max_urls: int = 10) -> list[str]:
    """Use LLM to select the most relevant URLs for the research question."""
    prompt = f"""Given this research question: "{question}"

    Select the {max_urls} most relevant URLs from this list:
    {json.dumps(urls[:100])}

    Return a JSON object with a "urls" array containing only the selected URLs.
    Prioritize pages that likely contain substantive information (docs, blog posts,
    about pages) over generic pages (login, terms of service, etc.)."""

    response = client.chat.completions.create(
        model="gpt-4o-mini",
        messages=[{"role": "user", "content": prompt}],
        response_format={"type": "json_object"},
    )
    result = json.loads(response.choices[0].message.content)
    return result.get("urls", [])[:max_urls]
```

## Step 5: Scrape and Extract Content

```
def scrape_sources(urls: list[str]) -> list[dict]:
    """Scrape multiple URLs and return structured content."""
    results = []
    for url in urls:
        try:
            data = crw.scrape_url(url, params={"formats": ["markdown"]})
            results.append({
                "url": url,
                "title": data.get("metadata", {}).get("title", ""),
                "content": data.get("markdown", ""),
            })
        except Exception as e:
            print(f"Scrape failed for {url}: {e}")
    return results

def extract_structured(url: str, schema: dict) -> dict:
    """Extract structured data from a page using CRW's json format."""
    try:
        data = crw.scrape_url(url, params={
            "formats": ["json"],
            "jsonSchema": schema,
        })
        return data.get("json", {})
    except Exception as e:
        print(f"Extract failed for {url}: {e}")
        return {}
```

## Step 6: Analyze and Synthesize

```
def analyze_findings(question: str, sources: list[dict]) -> dict:
    """Analyze scraped content and identify key findings and gaps."""
    source_text = ""
    for s in sources:
        source_text += f"

--- Source: {s['url']} ---
{s['content'][:2000]}"

    prompt = f"""Analyze these sources to answer: "{question}"

    Sources:
    {source_text}

    Return a JSON object with:
    - "findings": array of key findings (each with "fact" and "source_url")
    - "gaps": array of information gaps that need more research
    - "confidence": 0-100 score of how well the question is answered
    - "summary": 2-3 sentence summary of findings so far"""

    response = client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": prompt}],
        response_format={"type": "json_object"},
    )
    return json.loads(response.choices[0].message.content)
```

## Step 7: The Research Loop

Put it all together in a loop that iterates until the agent has enough information:

```
def deep_research(question: str, seed_urls: list[str], max_iterations: int = 3) -> dict:
    """Run the full deep research pipeline."""
    all_findings = []
    all_sources = []
    iteration = 0

    while iteration < max_iterations:
        iteration += 1
        print(f"
{'='*60}")
        print(f"Research iteration {iteration}/{max_iterations}")
        print(f"{'='*60}")

        # Plan: what sub-questions do we need to answer?
        existing = json.dumps([f["fact"] for f in all_findings])
        sub_questions = plan_research(question, existing)
        print(f"Sub-questions: {sub_questions}")

        # Discover URLs
        discovered = discover_sources(seed_urls)
        print(f"Discovered {len(discovered)} URLs")

        # Select the most relevant URLs
        selected = select_urls(discovered, question)
        print(f"Selected {len(selected)} URLs to scrape")

        # Scrape
        new_sources = scrape_sources(selected)
        all_sources.extend(new_sources)
        print(f"Scraped {len(new_sources)} pages")

        # Analyze
        analysis = analyze_findings(question, all_sources)
        all_findings.extend(analysis.get("findings", []))
        confidence = analysis.get("confidence", 0)
        gaps = analysis.get("gaps", [])

        print(f"Confidence: {confidence}/100")
        print(f"Gaps remaining: {gaps}")

        # Decide: enough information?
        if confidence >= 80 or not gaps:
            print("Sufficient information gathered. Generating report.")
            break

        # If gaps remain, use them to guide next iteration
        seed_urls = []  # reset for next iteration search

    # Generate final report
    report = generate_report(question, all_findings, all_sources)
    return {
        "report": report,
        "sources": [{"url": s["url"], "title": s["title"]} for s in all_sources],
        "iterations": iteration,
        "total_findings": len(all_findings),
    }
```

## Step 8: Generate the Final Report

```
def generate_report(question: str, findings: list[dict], sources: list[dict]) -> str:
    """Generate a comprehensive research report with citations."""
    findings_text = json.dumps(findings, indent=2)
    source_list = "
".join([f"- [{s['title']}]({s['url']})" for s in sources])

    prompt = f"""Write a comprehensive research report answering: "{question}"

    Key findings:
    {findings_text}

    Requirements:
    - Start with an executive summary
    - Organize findings into logical sections with headers
    - Cite sources inline using [Source Title](URL) format
    - Include a "Sources" section at the end
    - Be factual — only include information from the findings
    - 500-1000 words

    Available sources:
    {source_list}"""

    response = client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": prompt}],
    )
    return response.choices[0].message.content
```

## Running the Agent

```
result = deep_research(
    question="What are the top open-source web scraping frameworks in 2026 and how do they compare?",
    seed_urls=[
        "https://github.com/topics/web-scraping",
        "https://docs.example.com/scraping-tools",
    ],
    max_iterations=3,
)

print(result["report"])
print(f"
Research completed in {result['iterations']} iterations")
print(f"Used {len(result['sources'])} sources")
print(f"Extracted {result['total_findings']} findings")
```

## Adding Structured Extraction

For specific data points, scrape with CRW's `json` format and a `jsonSchema`:

```
# Extract pricing information from a competitor page
pricing_schema = {
    "type": "object",
    "properties": {
        "plans": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "price": {"type": "string"},
                    "features": {"type": "array", "items": {"type": "string"}},
                },
            },
        },
        "has_free_tier": {"type": "boolean"},
        "enterprise_available": {"type": "boolean"},
    },
}

pricing = extract_structured("https://competitor.com/pricing", pricing_schema)
print(json.dumps(pricing, indent=2))
```

## Using fastCRW Instead of Self-Hosted

For production research agents that scrape many different sites, [fastCRW](https://fastcrw.com) handles proxy rotation and scaling:

```
crw = FirecrawlApp(
    api_key="crw_live_YOUR-FASTCRW-KEY",
    api_url="https://api.fastcrw.com"
)
```

The rest of the code stays the same. fastCRW is particularly valuable for deep research agents because they scrape diverse sites — the managed infrastructure handles scaling and reliability across different domains.

## Why CRW for Deep Research?

**Low latency enables deeper research.** Each research iteration involves multiple scrape calls. A local-first engine keeps each call quick, so multi-page iterations compound into a responsive, interactive tool rather than a slow batch job that waits on remote API round trips.

**Map endpoint enables intelligent exploration.** CRW's `/v1/map` returns all URLs on a site without downloading content. This lets the agent discover the site structure first, then selectively scrape only the relevant pages — saving time and tokens.

**JSON format provides structured data.** Instead of scraping raw content and parsing it with an LLM yourself, CRW's `json` format on `/v1/scrape` (request `formats: ["json"]` with a `jsonSchema`) returns structured JSON matching your schema at `data.json`. This is more reliable for specific data extraction tasks.

## Next Steps

- [Store research findings in a RAG pipeline](/blog/rag-pipeline-with-crw) for later retrieval
- [Use CRW's MCP server](/blog/mcp-web-scraping) to give AI assistants direct web access
- [Compare CRW vs Firecrawl](/blog/firecrawl-vs-crawl4ai-vs-crw) for performance benchmarks

## Get Started

Run CRW locally in one command:

```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```

Or sign up for [fastCRW](https://fastcrw.com) to start building your deep research agent without managing infrastructure.
