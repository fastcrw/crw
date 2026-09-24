# How to Build a Lead Enrichment Pipeline with CRW

> Build a lead enrichment pipeline that scrapes company websites, extracts structured data like industry, size, and tech stack, and enriches your CRM using CRW.

**Published:** 2026-04-18  
**Updated:** 2026-04-18  
**Canonical:** https://fastcrw.com/blog/lead-enrichment-crw

---

## What We're Building

A lead enrichment pipeline that: (1) takes a list of company URLs from your CRM or CSV, (2) scrapes each company's website using CRW, (3) extracts structured company data — name, industry, company size, tech stack, key contacts — using LLM schema extraction, and (4) writes enriched data back to your CRM or database.

Most lead enrichment APIs charge per lookup and give you stale data. With CRW, you scrape the company's actual website for real-time information. The LLM extraction turns unstructured "About Us" pages into structured records you can act on.

## Architecture Overview

The pipeline has four stages:

- **Ingest** — Read company URLs from CSV, CRM API, or database
- **Crawl** — Use CRW's `/v1/map` to discover key pages (about, team, careers, pricing), then `/v1/scrape` to fetch them
- **Extract** — Use CRW's `/v1/extract` with a JSON schema to pull structured company data
- **Enrich** — Write the structured data back to your CRM or export as enriched CSV

## Prerequisites

- CRW running locally: `docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`
- Python 3.10+
- An OpenAI API key (used by CRW for LLM extraction)

```
pip install firecrawl-py pandas requests
```

## Step 1: Set Up CRW and Define the Company Schema

Connect to CRW using the Firecrawl SDK and define a comprehensive schema for company data:

```
from firecrawl import FirecrawlApp

from datetime import datetime

# Connect to CRW
app = FirecrawlApp(api_key="crw_live_YOUR-KEY", api_url="http://localhost:3000")

# Or use fastCRW cloud
# app = FirecrawlApp(api_key="crw_live_YOUR-KEY", api_url="https://api.fastcrw.com")

COMPANY_SCHEMA = {
    "type": "object",
    "properties": {
        "company_name": {
            "type": "string",
            "description": "The official company name"
        },
        "industry": {
            "type": "string",
            "description": "The primary industry or sector (e.g., SaaS, Fintech, Healthcare)"
        },
        "description": {
            "type": "string",
            "description": "A one-sentence description of what the company does"
        },
        "company_size": {
            "type": "string",
            "description": "Employee count range (e.g., 1-10, 11-50, 51-200, 201-500, 500+)"
        },
        "founded_year": {
            "type": "integer",
            "description": "The year the company was founded"
        },
        "headquarters": {
            "type": "string",
            "description": "City and country of headquarters"
        },
        "tech_stack": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Technologies, frameworks, or platforms mentioned on the site"
        },
        "pricing_model": {
            "type": "string",
            "description": "How they charge (freemium, subscription, enterprise, usage-based, etc.)"
        },
        "key_products": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Main products or services offered"
        },
        "contact_email": {
            "type": "string",
            "description": "General contact or sales email if listed on the site"
        },
        "social_links": {
            "type": "object",
            "properties": {
                "linkedin": {"type": "string"},
                "twitter": {"type": "string"},
                "github": {"type": "string"}
            },
            "description": "Social media profile URLs found on the site"
        }
    },
    "required": ["company_name", "industry", "description"]
}
```

## Step 2: Discover Relevant Pages with Map

Company data is scattered across multiple pages — about, team, careers, pricing. Use CRW's `/v1/map` to discover these pages, then scrape only the ones that matter:

```
def discover_key_pages(company_url: str) -> list[str]:
    """Use /v1/map to find the most relevant pages for enrichment."""
    try:
        result = app.map_url(company_url)

        if not result or "links" not in result:
            return [company_url]  # Fallback to just the homepage

        all_urls = result["links"]

        # Filter to pages likely to contain company info
        relevant_keywords = [
            "about", "team", "company", "careers", "jobs",
            "pricing", "contact", "press", "mission", "story"
        ]

        key_pages = [company_url]  # Always include homepage
        for url in all_urls:
            url_lower = url.lower()
            if any(kw in url_lower for kw in relevant_keywords):
                key_pages.append(url)

        # Limit to 5 pages to keep it fast
        return key_pages[:5]

    except Exception as e:
        print(f"Map failed for {company_url}: {e}")
        return [company_url]

# Example
pages = discover_key_pages("https://example-saas.com")
print(f"Found {len(pages)} relevant pages")
```

## Step 3: Scrape and Extract Company Data

Now scrape the discovered pages and extract structured data:

```
def enrich_company(company_url: str) -> dict | None:
    """Scrape a company website and extract structured data."""
    try:
        # Step 1: Discover relevant pages
        key_pages = discover_key_pages(company_url)
        print(f"  Found {len(key_pages)} key pages for {company_url}")

        # Step 2: Extract structured data using the schema
        result = app.extract(
            urls=key_pages,
            params={
                "prompt": (
                    "Extract comprehensive company information from these pages. "
                    "Look for company name, industry, size, tech stack, products, "
                    "pricing model, and contact information."
                ),
                "schema": COMPANY_SCHEMA,
            }
        )

        if result and "data" in result:
            data = result["data"]
            data["source_url"] = company_url
            data["enriched_at"] = datetime.now().isoformat()
            data["pages_analyzed"] = len(key_pages)
            return data

    except Exception as e:
        print(f"Error enriching {company_url}: {e}")

    return None

# Test with a single company
company = enrich_company("https://example-saas.com")
if company:
    print(json.dumps(company, indent=2))
```

## Step 4: Process Leads in Bulk

Process a list of company URLs from a CSV file and enrich them all:

```
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

def enrich_from_csv(input_path: str, output_path: str, max_workers: int = 3):
    """Read company URLs from CSV, enrich each, and write results."""
    # Read input CSV — expects a column named 'website' or 'url'
    df = pd.read_csv(input_path)
    url_column = "website" if "website" in df.columns else "url"

    urls = df[url_column].dropna().tolist()
    print(f"Processing {len(urls)} companies...")

    results = []
    failed = []

    # Process with controlled concurrency
    with ThreadPoolExecutor(max_workers=max_workers) as executor:
        future_to_url = {
            executor.submit(enrich_company, url): url
            for url in urls
        }

        for future in as_completed(future_to_url):
            url = future_to_url[future]
            try:
                result = future.result()
                if result:
                    results.append(result)
                    print(f"  ✓ {result.get('company_name', url)}")
                else:
                    failed.append(url)
                    print(f"  ✗ Failed: {url}")
            except Exception as e:
                failed.append(url)
                print(f"  ✗ Error for {url}: {e}")

    # Convert to DataFrame and export
    enriched_df = pd.json_normalize(results)
    enriched_df.to_csv(output_path, index=False)

    print(f"\nDone! Enriched {len(results)}/{len(urls)} companies.")
    print(f"Results saved to {output_path}")
    if failed:
        print(f"Failed URLs ({len(failed)}): {failed}")

    return enriched_df

# Usage
enriched = enrich_from_csv("leads.csv", "enriched_leads.csv")
```

## Step 5: Score and Segment Leads

Use the enriched data to score leads based on your ideal customer profile (ICP):

```
def score_lead(company_data: dict, icp: dict) -> int:
    """Score a lead 0-100 based on how well it matches your ICP."""
    score = 0

    # Industry match (30 points)
    if company_data.get("industry", "").lower() in [i.lower() for i in icp.get("industries", [])]:
        score += 30

    # Company size match (25 points)
    size = company_data.get("company_size", "")
    if size in icp.get("company_sizes", []):
        score += 25

    # Tech stack overlap (20 points)
    company_tech = set(t.lower() for t in company_data.get("tech_stack", []))
    icp_tech = set(t.lower() for t in icp.get("tech_stack", []))
    if company_tech and icp_tech:
        overlap = len(company_tech & icp_tech) / len(icp_tech)
        score += int(overlap * 20)

    # Has contact email (10 points)
    if company_data.get("contact_email"):
        score += 10

    # Pricing model match (15 points)
    if company_data.get("pricing_model", "").lower() in [p.lower() for p in icp.get("pricing_models", [])]:
        score += 15

    return min(score, 100)

# Define your ICP
my_icp = {
    "industries": ["SaaS", "Developer Tools", "Data Infrastructure"],
    "company_sizes": ["11-50", "51-200", "201-500"],
    "tech_stack": ["Python", "React", "PostgreSQL", "AWS", "Kubernetes"],
    "pricing_models": ["subscription", "usage-based"],
}

# Score all enriched leads
def score_all_leads(enriched_data: list[dict], icp: dict) -> list[dict]:
    """Score and sort all leads by ICP fit."""
    for lead in enriched_data:
        lead["icp_score"] = score_lead(lead, icp)

    return sorted(enriched_data, key=lambda x: x["icp_score"], reverse=True)
```

## Step 6: Export to CRM

Push enriched data to your CRM. Here's an example for HubSpot:

```
import requests

HUBSPOT_API_KEY = "your-hubspot-api-key"
HUBSPOT_BASE = "https://api.hubapi.com"

def push_to_hubspot(company_data: dict) -> dict | None:
    """Create or update a company in HubSpot with enriched data."""
    properties = {
        "name": company_data.get("company_name", ""),
        "domain": company_data.get("source_url", "").replace("https://", "").replace("http://", "").rstrip("/"),
        "industry": company_data.get("industry", ""),
        "description": company_data.get("description", ""),
        "numberofemployees": _size_to_number(company_data.get("company_size", "")),
        "city": company_data.get("headquarters", "").split(",")[0].strip() if company_data.get("headquarters") else "",
    }

    # Add tech stack as a custom property (if configured in HubSpot)
    tech_stack = company_data.get("tech_stack", [])
    if tech_stack:
        properties["tech_stack"] = "; ".join(tech_stack)

    try:
        response = requests.post(
            f"{HUBSPOT_BASE}/crm/v3/objects/companies",
            headers={
                "Authorization": f"Bearer {HUBSPOT_API_KEY}",
                "Content-Type": "application/json",
            },
            json={"properties": properties},
        )
        response.raise_for_status()
        return response.json()
    except requests.exceptions.HTTPError as e:
        print(f"HubSpot error for {company_data.get('company_name')}: {e}")
        return None

def _size_to_number(size_range: str) -> int:
    """Convert size range string to a representative number."""
    mapping = {"1-10": 5, "11-50": 30, "51-200": 125, "201-500": 350, "500+": 750}
    return mapping.get(size_range, 0)
```

## Complete Pipeline Script

Here's the full pipeline tied together:

```
def run_enrichment_pipeline(
    input_csv: str,
    output_csv: str,
    push_to_crm: bool = False,
    max_workers: int = 3,
):
    """Run the full lead enrichment pipeline."""
    print("=" * 60)
    print("Lead Enrichment Pipeline")
    print("=" * 60)

    # Step 1: Enrich from CSV
    enriched_df = enrich_from_csv(input_csv, output_csv, max_workers)

    # Step 2: Score leads
    enriched_records = enriched_df.to_dict("records")
    scored = score_all_leads(enriched_records, my_icp)

    # Step 3: Print top leads
    print(f"\nTop 10 Leads by ICP Score:")
    print("-" * 40)
    for lead in scored[:10]:
        print(f"  {lead.get('icp_score', 0):3d}  {lead.get('company_name', 'Unknown')}")
        print(f"       {lead.get('industry', 'N/A')} | {lead.get('company_size', 'N/A')}")

    # Step 4: Optionally push to CRM
    if push_to_crm:
        print(f"\nPushing {len(scored)} leads to HubSpot...")
        for lead in scored:
            result = push_to_hubspot(lead)
            if result:
                print(f"  ✓ {lead.get('company_name')}")

    print("\nPipeline complete!")

if __name__ == "__main__":
    run_enrichment_pipeline(
        input_csv="leads.csv",
        output_csv="enriched_leads.csv",
        push_to_crm=False,
    )
```

## Why CRW for This?

Lead enrichment services like Clearbit or ZoomInfo charge $0.10–$1.00 per lookup and return cached data that can be months old. With CRW, you scrape the company's live website for real-time data at a fraction of the cost:

- **Real-time data** — You're scraping the company's actual website, not a stale database. If they just updated their pricing page or announced a funding round, you'll see it immediately.
- **Schema flexibility** — Define exactly what data you need with JSON schemas. Need to track tech stack? Add it. Need pricing model? Add it. No waiting for an enrichment vendor to add new fields.
- **Cost efficiency** — Self-hosted CRW is free. Even with fastCRW cloud, enriching 1,000 companies costs a fraction of traditional enrichment APIs.
- **Low-latency, local-first** — Running the engine next to your script keeps page fetches quick, so enriching a large company list stays fast enough to run daily.

## Next Steps

- Read [How to Build a RAG Pipeline with CRW](/blog/rag-pipeline-with-crw) to add natural language search over your enriched leads
- Check out [Website to Markdown with CRW](/blog/website-to-markdown) for more on CRW's content extraction
- See [CRW vs Firecrawl](/blog/firecrawl-vs-crawl4ai-vs-crw) for a detailed comparison

Self-host CRW from [GitHub](https://github.com/fastcrw/crw) for free, or use [fastCRW](https://fastcrw.com) for managed cloud scraping with no infrastructure to maintain.
