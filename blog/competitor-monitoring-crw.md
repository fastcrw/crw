# How to Monitor Competitor Websites with CRW

> Set up automated competitor website monitoring with CRW — detect changes, compare snapshots, and generate AI summaries of what your competitors are up to.

**Published:** 2026-04-04  
**Updated:** 2026-05-23  
**Canonical:** https://fastcrw.com/blog/competitor-monitoring-crw

---

## What We're Building

A competitor monitoring system that: (1) periodically crawls competitor websites using CRW, (2) stores page snapshots, (3) compares current content against previous snapshots to detect changes, and (4) generates AI-powered summaries of what changed and why it matters.

Most website monitoring tools only detect that something changed. Ours goes further — CRW's clean markdown output makes it easy to diff content meaningfully, and an LLM summarizes the changes in business terms: "Competitor X added a new enterprise pricing tier" instead of "HTML changed on /pricing".

## Architecture Overview

The system has four components:

- **Crawler** — CRW's `/v1/crawl` fetches all pages from competitor sites as clean markdown
- **Snapshot Store** — SQLite stores page content with timestamps for historical comparison
- **Differ** — Compares current snapshots against previous ones using difflib
- **Summarizer** — OpenAI generates business-relevant summaries of detected changes

## Prerequisites

- CRW running locally: `docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`
- Python 3.10+
- An OpenAI API key (for change summarization)

```
pip install firecrawl-py openai apscheduler
```

## Step 1: Set Up CRW and the Snapshot Database

```
from firecrawl import FirecrawlApp

from datetime import datetime

# Connect to CRW
app = FirecrawlApp(api_key="crw_live_YOUR-KEY", api_url="http://localhost:3000")

# Or use fastCRW cloud
# app = FirecrawlApp(api_key="crw_live_YOUR-KEY", api_url="https://api.fastcrw.com")

DB_PATH = "competitor_monitor.db"

def init_db():
    """Create tables for competitor monitoring."""
    with sqlite3.connect(DB_PATH) as conn:
        conn.execute("""
            CREATE TABLE IF NOT EXISTS competitors (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                base_url TEXT UNIQUE NOT NULL,
                check_interval_hours INTEGER DEFAULT 24
            )
        """)
        conn.execute("""
            CREATE TABLE IF NOT EXISTS snapshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                competitor_id INTEGER NOT NULL,
                url TEXT NOT NULL,
                title TEXT,
                content_md TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                captured_at TEXT NOT NULL,
                FOREIGN KEY (competitor_id) REFERENCES competitors(id)
            )
        """)
        conn.execute("""
            CREATE TABLE IF NOT EXISTS changes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                competitor_id INTEGER NOT NULL,
                url TEXT NOT NULL,
                change_type TEXT NOT NULL,
                diff_text TEXT,
                summary TEXT,
                detected_at TEXT NOT NULL,
                FOREIGN KEY (competitor_id) REFERENCES competitors(id)
            )
        """)
        conn.execute("""
            CREATE INDEX IF NOT EXISTS idx_snapshots_url_date
            ON snapshots(competitor_id, url, captured_at DESC)
        """)
        conn.commit()

init_db()
```

## Step 2: Add Competitors to Track

```
def add_competitor(name: str, base_url: str, check_interval_hours: int = 24) -> int:
    """Add a competitor to monitor."""
    with sqlite3.connect(DB_PATH) as conn:
        cursor = conn.execute(
            "INSERT OR IGNORE INTO competitors (name, base_url, check_interval_hours) VALUES (?, ?, ?)",
            (name, base_url, check_interval_hours),
        )
        conn.commit()
        if cursor.lastrowid:
            return cursor.lastrowid
        row = conn.execute("SELECT id FROM competitors WHERE base_url = ?", (base_url,)).fetchone()
        return row[0]

# Add competitors
add_competitor("Acme Corp", "https://acme-corp.com", check_interval_hours=12)
add_competitor("Beta Inc", "https://beta-inc.com", check_interval_hours=24)
add_competitor("Gamma Labs", "https://gamma-labs.io", check_interval_hours=24)
```

## Step 3: Crawl and Snapshot Competitor Sites

Crawl each competitor's site and store the content as snapshots:

```
import hashlib

def crawl_competitor(competitor_id: int, base_url: str, page_limit: int = 50) -> list[dict]:
    """Crawl a competitor site and return page data."""
    try:
        # Start an async crawl
        crawl_result = app.crawl_url(
            base_url,
            params={
                "limit": page_limit,
                "scrapeOptions": {"formats": ["markdown"]},
            },
            poll_interval=5,
        )

        if not crawl_result or "data" not in crawl_result:
            print(f"  Crawl returned no data for {base_url}")
            return []

        return crawl_result["data"]

    except Exception as e:
        print(f"  Crawl error for {base_url}: {e}")
        return []

def save_snapshots(competitor_id: int, pages: list[dict]):
    """Save page snapshots to the database."""
    now = datetime.now().isoformat()

    with sqlite3.connect(DB_PATH) as conn:
        for page in pages:
            markdown = page.get("markdown", "")
            if not markdown or len(markdown) < 50:
                continue  # Skip empty/trivial pages

            url = page.get("metadata", {}).get("sourceURL", "")
            title = page.get("metadata", {}).get("title", "")
            content_hash = hashlib.sha256(markdown.encode()).hexdigest()

            conn.execute(
                """INSERT INTO snapshots
                   (competitor_id, url, title, content_md, content_hash, captured_at)
                   VALUES (?, ?, ?, ?, ?, ?)""",
                (competitor_id, url, title, markdown, content_hash, now),
            )
        conn.commit()

    print(f"  Saved {len(pages)} snapshots")
```

## Step 4: Detect Changes Between Snapshots

Compare current snapshots against the most recent previous ones:

```
import difflib

def detect_changes(competitor_id: int) -> list[dict]:
    """Compare latest snapshots against previous ones to detect changes."""
    changes = []

    with sqlite3.connect(DB_PATH) as conn:
        conn.row_factory = sqlite3.Row

        # Get all unique URLs for this competitor
        urls = conn.execute(
            "SELECT DISTINCT url FROM snapshots WHERE competitor_id = ? AND url != ''",
            (competitor_id,),
        ).fetchall()

        for url_row in urls:
            url = url_row["url"]

            # Get the two most recent snapshots for this URL
            recent = conn.execute(
                """SELECT content_md, content_hash, captured_at
                   FROM snapshots
                   WHERE competitor_id = ? AND url = ?
                   ORDER BY captured_at DESC
                   LIMIT 2""",
                (competitor_id, url),
            ).fetchall()

            if len(recent) < 2:
                # First snapshot — mark as new page
                changes.append({
                    "url": url,
                    "change_type": "new_page",
                    "diff": None,
                    "current_content": recent[0]["content_md"] if recent else "",
                })
                continue

            current, previous = recent[0], recent[1]

            # Skip if content hasn't changed
            if current["content_hash"] == previous["content_hash"]:
                continue

            # Generate diff
            diff = list(difflib.unified_diff(
                previous["content_md"].splitlines(),
                current["content_md"].splitlines(),
                fromfile=f"previous ({previous['captured_at'][:10]})",
                tofile=f"current ({current['captured_at'][:10]})",
                lineterm="",
            ))

            if diff:
                diff_text = "\n".join(diff)
                # Count added and removed lines
                added = sum(1 for l in diff if l.startswith("+") and not l.startswith("+++"))
                removed = sum(1 for l in diff if l.startswith("-") and not l.startswith("---"))

                changes.append({
                    "url": url,
                    "change_type": "modified",
                    "diff": diff_text,
                    "lines_added": added,
                    "lines_removed": removed,
                    "current_content": current["content_md"],
                })

    # Also detect removed pages
    with sqlite3.connect(DB_PATH) as conn:
        conn.row_factory = sqlite3.Row
        # Pages that existed in previous crawl but not in latest
        removed = conn.execute(
            """SELECT DISTINCT s1.url
               FROM snapshots s1
               WHERE s1.competitor_id = ?
               AND s1.url NOT IN (
                   SELECT url FROM snapshots
                   WHERE competitor_id = ?
                   AND captured_at = (SELECT MAX(captured_at) FROM snapshots WHERE competitor_id = ?)
               )
               AND s1.captured_at = (
                   SELECT MAX(captured_at) FROM snapshots
                   WHERE competitor_id = ? AND captured_at < (
                       SELECT MAX(captured_at) FROM snapshots WHERE competitor_id = ?
                   )
               )""",
            (competitor_id, competitor_id, competitor_id, competitor_id, competitor_id),
        ).fetchall()

        for row in removed:
            changes.append({
                "url": row["url"],
                "change_type": "removed",
                "diff": None,
            })

    return changes
```

## Step 5: Summarize Changes with AI

Use OpenAI to generate business-relevant summaries of detected changes:

```
from openai import OpenAI

client = OpenAI()

def summarize_changes(competitor_name: str, changes: list[dict]) -> str:
    """Generate an AI summary of all detected changes for a competitor."""
    if not changes:
        return f"No changes detected for {competitor_name}."

    # Build a concise change report for the LLM
    change_descriptions = []
    for change in changes:
        if change["change_type"] == "new_page":
            desc = f"NEW PAGE: {change['url']}"
            if change.get("current_content"):
                desc += f"\nContent preview: {change['current_content'][:500]}"
            change_descriptions.append(desc)

        elif change["change_type"] == "modified":
            desc = (
                f"MODIFIED: {change['url']} "
                f"(+{change.get('lines_added', 0)}/-{change.get('lines_removed', 0)} lines)"
            )
            if change.get("diff"):
                desc += f"\nDiff:\n{change['diff'][:1000]}"
            change_descriptions.append(desc)

        elif change["change_type"] == "removed":
            change_descriptions.append(f"REMOVED PAGE: {change['url']}")

    changes_text = "\n\n---\n\n".join(change_descriptions)

    response = client.chat.completions.create(
        model="gpt-4o-mini",
        messages=[
            {
                "role": "system",
                "content": (
                    "You are a competitive intelligence analyst. Summarize website changes "
                    "in business terms. Focus on: pricing changes, new features/products, "
                    "messaging shifts, hiring signals, and strategic moves. Be concise and "
                    "actionable. Use bullet points."
                ),
            },
            {
                "role": "user",
                "content": (
                    f"Competitor: {competitor_name}\n\n"
                    f"Detected changes:\n\n{changes_text}\n\n"
                    "Summarize these changes and their business implications."
                ),
            },
        ],
        max_tokens=500,
    )

    return response.choices[0].message.content or "Unable to generate summary."

def summarize_single_change(competitor_name: str, change: dict) -> str:
    """Summarize a single significant change."""
    if not change.get("diff"):
        return f"{change['change_type'].replace('_', ' ').title()}: {change['url']}"

    response = client.chat.completions.create(
        model="gpt-4o-mini",
        messages=[
            {
                "role": "system",
                "content": "Summarize this website change in one sentence, focusing on business impact.",
            },
            {
                "role": "user",
                "content": f"Competitor: {competitor_name}\nURL: {change['url']}\nDiff:\n{change['diff'][:1500]}",
            },
        ],
        max_tokens=100,
    )

    return response.choices[0].message.content or ""
```

## Step 6: Schedule Monitoring with APScheduler

```
from apscheduler.schedulers.blocking import BlockingScheduler

def monitor_competitor(competitor_id: int, name: str, base_url: str):
    """Run a full monitoring cycle for one competitor."""
    print(f"\n[{datetime.now().isoformat()}] Monitoring {name} ({base_url})...")

    # Crawl
    pages = crawl_competitor(competitor_id, base_url)
    if not pages:
        print(f"  No pages returned, skipping.")
        return

    # Save snapshots
    save_snapshots(competitor_id, pages)

    # Detect changes
    changes = detect_changes(competitor_id)

    if not changes:
        print(f"  No changes detected.")
        return

    print(f"  Detected {len(changes)} changes:")
    for c in changes:
        print(f"    [{c['change_type']}] {c['url']}")

    # Summarize with AI
    summary = summarize_changes(name, changes)
    print(f"\n  AI Summary:\n{summary}")

    # Save changes to database
    now = datetime.now().isoformat()
    with sqlite3.connect(DB_PATH) as conn:
        for change in changes:
            conn.execute(
                """INSERT INTO changes
                   (competitor_id, url, change_type, diff_text, summary, detected_at)
                   VALUES (?, ?, ?, ?, ?, ?)""",
                (
                    competitor_id,
                    change["url"],
                    change["change_type"],
                    change.get("diff", ""),
                    summarize_single_change(name, change) if change.get("diff") else "",
                    now,
                ),
            )
        conn.commit()

def run_all_monitors():
    """Run monitoring for all tracked competitors."""
    print(f"\n{'='*60}")
    print(f"Competitor Monitoring Run — {datetime.now().isoformat()}")
    print(f"{'='*60}")

    with sqlite3.connect(DB_PATH) as conn:
        conn.row_factory = sqlite3.Row
        competitors = conn.execute("SELECT id, name, base_url FROM competitors").fetchall()

    for comp in competitors:
        monitor_competitor(comp["id"], comp["name"], comp["base_url"])

    print(f"\nMonitoring complete for {len(competitors)} competitors.")

def main():
    """Start the competitor monitoring system."""
    init_db()

    # Run an immediate check
    run_all_monitors()

    # Schedule recurring checks
    scheduler = BlockingScheduler()
    scheduler.add_job(run_all_monitors, "interval", hours=12)

    print("\nMonitoring scheduler started. Running every 12 hours.")
    print("Press Ctrl+C to stop.")
    scheduler.start()

if __name__ == "__main__":
    main()
```

## Step 7: Generate Competitive Intelligence Reports

Create weekly reports summarizing all competitor activity:

```
def generate_weekly_report() -> str:
    """Generate a weekly competitive intelligence report."""
    from datetime import timedelta

    week_ago = (datetime.now() - timedelta(days=7)).isoformat()

    with sqlite3.connect(DB_PATH) as conn:
        conn.row_factory = sqlite3.Row

        competitors = conn.execute("SELECT id, name, base_url FROM competitors").fetchall()

        report = f"# Competitive Intelligence Report\n"
        report += f"Week ending {datetime.now().strftime('%Y-%m-%d')}\n\n"

        for comp in competitors:
            changes = conn.execute(
                """SELECT url, change_type, summary, detected_at
                   FROM changes
                   WHERE competitor_id = ? AND detected_at >= ?
                   ORDER BY detected_at DESC""",
                (comp["id"], week_ago),
            ).fetchall()

            report += f"## {comp['name']}\n"
            report += f"URL: {comp['base_url']}\n"
            report += f"Changes this week: {len(changes)}\n\n"

            if changes:
                for change in changes:
                    report += f"- **[{change['change_type']}]** {change['url']}\n"
                    if change["summary"]:
                        report += f"  {change['summary']}\n"
            else:
                report += "No changes detected this week.\n"

            report += "\n"

    return report

# Generate and print report
print(generate_weekly_report())
```

## Monitoring Specific Sections

Focus monitoring on high-value pages like pricing, changelog, and blog:

```
def monitor_key_pages(competitor_id: int, base_url: str):
    """Monitor only specific high-value pages instead of full crawl."""
    key_paths = [
        "/pricing",
        "/changelog",
        "/blog",
        "/features",
        "/enterprise",
        "/about",
        "/careers",
    ]

    for path in key_paths:
        url = f"{base_url.rstrip('/')}{path}"
        try:
            result = app.scrape_url(url, params={"formats": ["markdown"]})
            if result and "markdown" in result:
                # Save snapshot for this specific page
                content = result["markdown"]
                content_hash = hashlib.sha256(content.encode()).hexdigest()
                title = result.get("metadata", {}).get("title", "")

                with sqlite3.connect(DB_PATH) as conn:
                    conn.execute(
                        """INSERT INTO snapshots
                           (competitor_id, url, title, content_md, content_hash, captured_at)
                           VALUES (?, ?, ?, ?, ?, ?)""",
                        (competitor_id, url, title, content, content_hash, datetime.now().isoformat()),
                    )
                    conn.commit()

                print(f"  ✓ {path}")
        except Exception as e:
            print(f"  ✗ {path}: {e}")
```

## Why CRW for This?

Competitor monitoring requires clean, consistent content extraction across diverse websites. CRW is ideal for this:

- **Clean markdown output** — CRW strips navigation, ads, and boilerplate, giving you just the content. This makes diffs meaningful — you see actual content changes, not template shifts.
- **Full-site crawling** — The `/v1/crawl` endpoint discovers and scrapes all linked pages automatically. You don't need to manually list every URL to monitor.
- **Low-latency, local-first** — Running the engine next to your scheduler lets you monitor multiple competitors with dozens of pages each, multiple times per day, without remote API round trips.
- **Tiny footprint** — A single small static binary that runs alongside your monitoring stack on minimal infrastructure.

## Next Steps

- Read [How to Build a RAG Pipeline with CRW](/blog/rag-pipeline-with-crw) to add natural language queries over your competitor data
- Check out [Website to Markdown with CRW](/blog/website-to-markdown) for more on CRW's content extraction
- See [CRW vs Firecrawl](/blog/firecrawl-vs-crawl4ai-vs-crw) for a detailed comparison

Self-host CRW from [GitHub](https://github.com/fastcrw/crw) for free, or use [fastCRW](https://fastcrw.com) for managed cloud scraping with no infrastructure to maintain.

## FAQ

### Why use clean markdown instead of raw HTML for change detection?

Raw HTML diffs are noisy — a single template tweak, rotated ad slot, or session token changes the markup without changing anything a human cares about. CRW strips navigation, ads, and boilerplate so a diff surfaces real content changes like a new pricing tier or a reworded headline, which also makes the LLM summaries far more accurate.

### How often can I poll competitor sites without hitting limits?

Self-hosting the AGPL-3.0 engine has no per-request cost, so you can crawl every competitor as often as your scheduler allows — the tutorial runs every 12 hours. On the managed cloud each crawled page costs 1 credit (2 if chrome-rendered), so a 50-page crawl twice a day across three competitors is roughly 600 credits a day, well within the 100,000-credit Standard tier.

### Does CRW detect new and removed pages, not just edits?

Yes. The detect_changes function compares each crawl against the previous snapshot set: a URL with only one snapshot is flagged new_page, a URL missing from the latest crawl is flagged removed, and a URL whose content hash changed is flagged modified with a line-level diff. All three feed into the AI summary.

### Can I monitor only specific pages like /pricing instead of the whole site?

Yes. The monitor_key_pages helper scrapes a fixed list of high-value paths — /pricing, /changelog, /features, /careers and so on — with /v1/scrape instead of a full crawl. Each scrape costs 1 credit on the managed cloud, so targeted monitoring is cheaper and faster than re-crawling an entire site every cycle.

### How reliable is CRW at scraping diverse competitor sites?

On Firecrawl's public scrape-content-dataset-v1 (1,000 URLs, harness diagnose_3way.py, run 2026-05-08), fastCRW posted the highest truth-recall of the three tools tested at 63.74% of 819 labeled URLs, ~92% scrape success of reachable URLs, and 0 thrown errors across 3,000 requests. The 34 URLs only fastCRW recovers — 70% more than the other two combined — matter most for monitoring jobs where missing a competitor change has real cost.

### Do I need a heavy infrastructure stack to run this?

No. CRW ships as a single binary, roughly a 10 MB download, in one container, versus the five containers a Firecrawl self-host needs. You can run the engine, SQLite, and the APScheduler loop together on one small machine with no Redis or separate worker processes.
