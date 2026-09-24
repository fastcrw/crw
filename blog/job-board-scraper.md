# How to Build a Job Board Scraper with CRW and OpenAI

> Build a job board scraper with CRW and OpenAI — extract listings, match against your resume, and automate your job search.

**Published:** 2026-04-29  
**Updated:** 2026-04-29  
**Canonical:** https://fastcrw.com/blog/job-board-scraper

---

## What We're Building

A job board scraper that: (1) scrapes job listings from multiple boards using CRW, (2) extracts structured data — title, company, salary, location, requirements — using LLM schema extraction, (3) stores listings in a database with deduplication, and (4) matches jobs against your resume using OpenAI to rank the best fits.

CRW handles the scraping and structured extraction. OpenAI handles the resume matching and scoring. The result is an automated job search pipeline that runs on a schedule, finds new listings, and tells you which ones are worth applying to.

## Architecture Overview

The pipeline has five stages:

- **Discover** — Use CRW's `/v1/map` to find job listing URLs on each board
- **Scrape** — Use CRW's `/v1/scrape` to fetch individual job postings
- **Extract** — Use CRW's `/v1/extract` with a JSON schema to pull structured job data
- **Store** — Save listings to SQLite with deduplication
- **Match** — Use OpenAI to score jobs against your resume and rank by fit

## Prerequisites

- CRW running locally: `docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`
- Python 3.10+
- An OpenAI API key

```
pip install firecrawl-py openai apscheduler
```

## Step 1: Set Up CRW and Define the Job Schema

```
from firecrawl import FirecrawlApp
from openai import OpenAI

from datetime import datetime

# Connect to CRW
app = FirecrawlApp(api_key="crw_live_YOUR-KEY", api_url="http://localhost:3000")

# Or use fastCRW cloud
# app = FirecrawlApp(api_key="crw_live_YOUR-KEY", api_url="https://api.fastcrw.com")

# OpenAI for resume matching
client = OpenAI()

JOB_SCHEMA = {
    "type": "object",
    "properties": {
        "title": {
            "type": "string",
            "description": "The job title"
        },
        "company": {
            "type": "string",
            "description": "The hiring company name"
        },
        "location": {
            "type": "string",
            "description": "Job location (city, state, remote, hybrid, etc.)"
        },
        "salary_min": {
            "type": "number",
            "description": "Minimum salary in USD (annual), null if not listed"
        },
        "salary_max": {
            "type": "number",
            "description": "Maximum salary in USD (annual), null if not listed"
        },
        "employment_type": {
            "type": "string",
            "description": "Full-time, part-time, contract, or internship"
        },
        "experience_level": {
            "type": "string",
            "description": "Junior, mid-level, senior, lead, or principal"
        },
        "required_skills": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Required technical skills and technologies"
        },
        "nice_to_have_skills": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Preferred but not required skills"
        },
        "responsibilities": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Key job responsibilities (top 5)"
        },
        "benefits": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Listed benefits and perks"
        },
        "apply_url": {
            "type": "string",
            "description": "Direct URL to apply, if different from the listing page"
        }
    },
    "required": ["title", "company", "location", "required_skills"]
}
```

## Step 2: Set Up the Jobs Database

```
DB_PATH = "job_tracker.db"

def init_db():
    """Create the job tracking tables."""
    with sqlite3.connect(DB_PATH) as conn:
        conn.execute("""
            CREATE TABLE IF NOT EXISTS jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                company TEXT NOT NULL,
                location TEXT,
                salary_min REAL,
                salary_max REAL,
                employment_type TEXT,
                experience_level TEXT,
                required_skills TEXT,
                nice_to_have_skills TEXT,
                responsibilities TEXT,
                benefits TEXT,
                source_url TEXT UNIQUE NOT NULL,
                apply_url TEXT,
                content_hash TEXT NOT NULL,
                match_score REAL,
                match_reasoning TEXT,
                first_seen TEXT NOT NULL,
                last_seen TEXT NOT NULL,
                status TEXT DEFAULT 'new'
            )
        """)
        conn.execute("""
            CREATE TABLE IF NOT EXISTS job_boards (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                base_url TEXT NOT NULL,
                search_path TEXT,
                listing_pattern TEXT
            )
        """)
        conn.commit()

init_db()
```

## Step 3: Discover Job Listings with Map

Use CRW's `/v1/map` to discover job listing URLs on each board:

```
def discover_job_urls(board_url: str, listing_pattern: str = "/jobs/") -> list[str]:
    """Use /v1/map to discover job listing URLs on a board."""
    try:
        result = app.map_url(board_url)

        if not result or "links" not in result:
            return []

        all_urls = result["links"]

        # Filter to URLs that look like individual job listings
        job_urls = [
            url for url in all_urls
            if listing_pattern in url.lower()
            and not url.endswith(listing_pattern)  # Exclude the listings index page
            and not url.endswith(listing_pattern.rstrip("/"))
        ]

        print(f"  Found {len(job_urls)} job URLs (from {len(all_urls)} total)")
        return job_urls

    except Exception as e:
        print(f"  Map error for {board_url}: {e}")
        return []

# Configure job boards to monitor
JOB_BOARDS = [
    {
        "name": "Example Tech Jobs",
        "base_url": "https://jobs.example-tech.com",
        "search_path": "/search?q=python+developer&l=remote",
        "listing_pattern": "/job/",
    },
    {
        "name": "Startup Board",
        "base_url": "https://startup-jobs.example.com",
        "search_path": "/remote/engineering",
        "listing_pattern": "/jobs/",
    },
    {
        "name": "Dev Careers",
        "base_url": "https://devcareer.example.io",
        "search_path": "/listings?category=backend",
        "listing_pattern": "/listing/",
    },
]
```

## Step 4: Scrape and Extract Job Data

Scrape individual job postings and extract structured data:

```
def extract_job(url: str) -> dict | None:
    """Scrape a job posting and extract structured data."""
    try:
        result = app.extract(
            urls=[url],
            params={
                "prompt": (
                    "Extract the job listing information from this page. "
                    "Include the job title, company, location, salary range, "
                    "required skills, responsibilities, and benefits."
                ),
                "schema": JOB_SCHEMA,
            }
        )

        if result and "data" in result:
            job_data = result["data"]
            job_data["source_url"] = url
            return job_data

    except Exception as e:
        print(f"    Error extracting {url}: {e}")

    return None

def scrape_job_board(board: dict) -> list[dict]:
    """Scrape all jobs from a single board."""
    print(f"\nScraping {board['name']}...")

    # Discover listing URLs
    search_url = f"{board['base_url']}{board['search_path']}"
    job_urls = discover_job_urls(search_url, board.get("listing_pattern", "/jobs/"))

    if not job_urls:
        # Fallback: try mapping the base URL
        job_urls = discover_job_urls(board["base_url"], board.get("listing_pattern", "/jobs/"))

    jobs = []
    for i, url in enumerate(job_urls[:30]):  # Limit to 30 per board
        print(f"  [{i+1}/{min(len(job_urls), 30)}] Extracting: {url[:80]}...")
        job = extract_job(url)
        if job and job.get("title"):
            jobs.append(job)
            print(f"    ✓ {job['title']} at {job.get('company', 'Unknown')}")

    print(f"  Extracted {len(jobs)} jobs from {board['name']}")
    return jobs
```

## Step 5: Store Jobs with Deduplication

```
def save_job(job: dict) -> bool:
    """Save a job to the database. Returns True if it's a new listing."""
    now = datetime.now().isoformat()

    # Create a content hash for deduplication
    hash_content = f"{job.get('title', '')}{job.get('company', '')}{job.get('location', '')}"
    content_hash = hashlib.sha256(hash_content.encode()).hexdigest()

    with sqlite3.connect(DB_PATH) as conn:
        # Check if this job already exists
        existing = conn.execute(
            "SELECT id FROM jobs WHERE source_url = ? OR content_hash = ?",
            (job.get("source_url", ""), content_hash),
        ).fetchone()

        if existing:
            # Update last_seen timestamp
            conn.execute(
                "UPDATE jobs SET last_seen = ? WHERE id = ?",
                (now, existing[0]),
            )
            conn.commit()
            return False

        # Insert new job
        conn.execute(
            """INSERT INTO jobs
               (title, company, location, salary_min, salary_max,
                employment_type, experience_level, required_skills,
                nice_to_have_skills, responsibilities, benefits,
                source_url, apply_url, content_hash, first_seen, last_seen)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
            (
                job.get("title", ""),
                job.get("company", ""),
                job.get("location", ""),
                job.get("salary_min"),
                job.get("salary_max"),
                job.get("employment_type", ""),
                job.get("experience_level", ""),
                json.dumps(job.get("required_skills", [])),
                json.dumps(job.get("nice_to_have_skills", [])),
                json.dumps(job.get("responsibilities", [])),
                json.dumps(job.get("benefits", [])),
                job.get("source_url", ""),
                job.get("apply_url", ""),
                content_hash,
                now,
                now,
            ),
        )
        conn.commit()
        return True

def save_all_jobs(jobs: list[dict]) -> tuple[int, int]:
    """Save all jobs. Returns (new_count, duplicate_count)."""
    new_count = 0
    dup_count = 0
    for job in jobs:
        if save_job(job):
            new_count += 1
        else:
            dup_count += 1
    return new_count, dup_count
```

## Step 6: Match Jobs Against Your Resume

Use OpenAI to score how well each job matches your background:

```
RESUME = """
Senior Backend Engineer with 6 years of experience.

Skills: Python, Go, PostgreSQL, Redis, Docker, Kubernetes, AWS, REST APIs,
GraphQL, FastAPI, Django, SQLAlchemy, Celery, RabbitMQ, Terraform

Experience:
- Led backend team of 4 at a Series B startup (2022-present)
- Built data pipeline processing 10M events/day
- Designed microservices architecture serving 50k RPM
- Previous: Backend engineer at mid-size SaaS company (2019-2022)

Education: BS Computer Science

Looking for: Senior/Staff backend roles, remote-friendly, $180k-$250k
"""

def match_job_to_resume(job: dict, resume: str = RESUME) -> dict:
    """Score how well a job matches the resume using OpenAI."""
    job_description = f"""
Title: {job.get('title', 'Unknown')}
Company: {job.get('company', 'Unknown')}
Location: {job.get('location', 'Unknown')}
Salary: ${job.get('salary_min', 'N/A')} - ${job.get('salary_max', 'N/A')}
Experience Level: {job.get('experience_level', 'Unknown')}
Required Skills: {', '.join(job.get('required_skills', []))}
Nice to Have: {', '.join(job.get('nice_to_have_skills', []))}
Responsibilities: {'; '.join(job.get('responsibilities', [])[:5])}
"""

    response = client.chat.completions.create(
        model="gpt-4o-mini",
        messages=[
            {
                "role": "system",
                "content": (
                    "You are a job matching assistant. Score how well a job listing matches "
                    "a candidate's resume on a scale of 0-100. Consider: skill overlap, "
                    "experience level fit, salary alignment, and role type match. "
                    "Respond with JSON: {"score": <number>, "reasoning": "<2-3 sentences>"}"
                ),
            },
            {
                "role": "user",
                "content": f"Resume:\n{resume}\n\nJob Listing:\n{job_description}",
            },
        ],
        max_tokens=200,
        response_format={"type": "json_object"},
    )

    try:
        result = json.loads(response.choices[0].message.content or "{}")
        return {
            "score": result.get("score", 0),
            "reasoning": result.get("reasoning", ""),
        }
    except json.JSONDecodeError:
        return {"score": 0, "reasoning": "Failed to parse match result"}

def score_new_jobs():
    """Score all unscored jobs in the database."""
    with sqlite3.connect(DB_PATH) as conn:
        conn.row_factory = sqlite3.Row
        unscored = conn.execute(
            "SELECT * FROM jobs WHERE match_score IS NULL AND status = 'new'"
        ).fetchall()

    print(f"\nScoring {len(unscored)} new jobs against resume...")

    for job_row in unscored:
        job = dict(job_row)
        # Parse JSON fields
        job["required_skills"] = json.loads(job.get("required_skills", "[]"))
        job["nice_to_have_skills"] = json.loads(job.get("nice_to_have_skills", "[]"))
        job["responsibilities"] = json.loads(job.get("responsibilities", "[]"))

        match = match_job_to_resume(job)

        with sqlite3.connect(DB_PATH) as conn:
            conn.execute(
                "UPDATE jobs SET match_score = ?, match_reasoning = ? WHERE id = ?",
                (match["score"], match["reasoning"], job["id"]),
            )
            conn.commit()

        print(f"  [{match['score']:3.0f}] {job['title']} at {job['company']}")
        if match["score"] >= 80:
            print(f"       ★ HIGH MATCH: {match['reasoning']}")
```

## Step 7: Generate Daily Job Reports

```
def generate_daily_report(min_score: int = 60) -> str:
    """Generate a daily report of top matching jobs."""
    with sqlite3.connect(DB_PATH) as conn:
        conn.row_factory = sqlite3.Row

        top_jobs = conn.execute(
            """SELECT title, company, location, salary_min, salary_max,
                      experience_level, match_score, match_reasoning,
                      source_url, first_seen
               FROM jobs
               WHERE match_score >= ? AND status = 'new'
               ORDER BY match_score DESC
               LIMIT 20""",
            (min_score,),
        ).fetchall()

        total_new = conn.execute(
            "SELECT COUNT(*) FROM jobs WHERE status = 'new'"
        ).fetchone()[0]

    report = f"""
{'='*60}
Daily Job Report — {datetime.now().strftime('%Y-%m-%d')}
{'='*60}
Total new listings: {total_new}
Matching jobs (score >= {min_score}): {len(top_jobs)}
{'='*60}
"""

    for i, job in enumerate(top_jobs, 1):
        salary = ""
        if job["salary_min"] and job["salary_max"]:
            salary = f"${job['salary_min']:,.0f}-${job['salary_max']:,.0f}"
        elif job["salary_min"]:
            salary = f"${job['salary_min']:,.0f}+"
        else:
            salary = "Not listed"

        report += f"""
{i}. {job['title']} at {job['company']}
   Score: {job['match_score']:.0f}/100 | Location: {job['location']} | Salary: {salary}
   Level: {job['experience_level'] or 'Not specified'}
   Why: {job['match_reasoning']}
   Apply: {job['source_url']}
"""

    return report
```

## Step 8: Run the Full Pipeline

```
from apscheduler.schedulers.blocking import BlockingScheduler

def run_job_search():
    """Execute the full job search pipeline."""
    print(f"\n{'='*60}")
    print(f"Job Search Run — {datetime.now().isoformat()}")
    print(f"{'='*60}")

    all_jobs = []

    # Scrape all configured boards
    for board in JOB_BOARDS:
        jobs = scrape_job_board(board)
        all_jobs.extend(jobs)

    if not all_jobs:
        print("No jobs found across any board.")
        return

    # Save with deduplication
    new_count, dup_count = save_all_jobs(all_jobs)
    print(f"\nResults: {new_count} new jobs, {dup_count} duplicates skipped")

    # Score new jobs against resume
    score_new_jobs()

    # Generate and print report
    report = generate_daily_report(min_score=60)
    print(report)

    # Optionally send the report via email/webhook
    # send_report(report)

def main():
    """Start the automated job search."""
    init_db()

    # Run an immediate search
    run_job_search()

    # Schedule daily runs
    scheduler = BlockingScheduler()
    scheduler.add_job(run_job_search, "cron", hour=8, minute=0)  # Every day at 8 AM

    print("\nJob search scheduler started. Running daily at 8:00 AM.")
    print("Press Ctrl+C to stop.")
    scheduler.start()

if __name__ == "__main__":
    main()
```

## Managing Job Application Status

Track your application progress alongside the scraper:

```
def update_job_status(job_id: int, status: str):
    """Update a job's status (new, applied, interviewing, rejected, offer)."""
    valid_statuses = {"new", "saved", "applied", "interviewing", "rejected", "offer", "declined"}
    if status not in valid_statuses:
        raise ValueError(f"Invalid status. Must be one of: {valid_statuses}")

    with sqlite3.connect(DB_PATH) as conn:
        conn.execute(
            "UPDATE jobs SET status = ? WHERE id = ?",
            (status, job_id),
        )
        conn.commit()

def get_application_stats() -> dict:
    """Get a summary of application statuses."""
    with sqlite3.connect(DB_PATH) as conn:
        rows = conn.execute(
            "SELECT status, COUNT(*) as count FROM jobs GROUP BY status"
        ).fetchall()
    return {row[0]: row[1] for row in rows}

# Usage
stats = get_application_stats()
print(f"Application stats: {json.dumps(stats, indent=2)}")
```

## Advanced: Scraping with Keyword Filters

Many job boards support search URLs. Combine CRW's map with targeted searches:

```
def search_jobs(board_url: str, keywords: list[str], location: str = "remote") -> list[str]:
    """Build search URLs and discover job listings."""
    all_job_urls = set()

    for keyword in keywords:
        # Most boards use query parameters for search
        search_urls = [
            f"{board_url}/search?q={keyword}&l={location}",
            f"{board_url}/jobs?query={keyword}&location={location}",
            f"{board_url}/{location}/{keyword}",
        ]

        for search_url in search_urls:
            try:
                urls = discover_job_urls(search_url)
                all_job_urls.update(urls)
                if urls:
                    break  # Found results, skip other URL patterns
            except Exception:
                continue

    return list(all_job_urls)

# Search for specific roles
job_urls = search_jobs(
    "https://jobs.example-tech.com",
    keywords=["python backend", "senior engineer", "staff engineer"],
    location="remote",
)
print(f"Found {len(job_urls)} listings across all searches")
```

## Why CRW for This?

Job board scraping has unique challenges: varied HTML structures, dynamic content, and the need for structured data extraction. CRW addresses all three:

- **LLM extraction handles any board** — No need to write custom parsers for Indeed, LinkedIn, Greenhouse, Lever, or Ashby. The JSON schema approach extracts structured job data from any board's HTML without site-specific selectors.
- **URL discovery with Map** — CRW's `/v1/map` endpoint finds all job listing URLs on a board without you needing to know the pagination structure. One call discovers every listing.
- **Low-latency, local-first** — Running the engine next to your script keeps page fetches quick, so scraping listings across several boards stays fast enough to run before your morning coffee.
- **Self-hosted for privacy** — Your resume and job search activity stay on your machine. No data sent to third-party scraping services.

## Next Steps

- Read [How to Build a RAG Pipeline with CRW](/blog/rag-pipeline-with-crw) to add natural language search over your job listings
- Check out [Website to Markdown with CRW](/blog/website-to-markdown) for more on CRW's content extraction
- See [CRW vs Firecrawl](/blog/firecrawl-vs-crawl4ai-vs-crw) for a detailed comparison

Self-host CRW from [GitHub](https://github.com/fastcrw/crw) for free, or use [fastCRW](https://fastcrw.com) for managed cloud scraping with no infrastructure to maintain.
