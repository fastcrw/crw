# Build an AI Price Tracker in Python (2026) — 50 Lines, Zero API Cost [Self-Hosted]

> Build an AI price tracker in 50 lines of Python: scrape with fastCRW, extract structured prices via LLM, store in SQLite, alert on drops. AGPL-3.0 self-host, zero per-request cost — full code included.

**Published:** 2026-04-03  
**Updated:** 2026-05-23  
**Canonical:** https://fastcrw.com/blog/ai-price-tracker

---

## What We're Building

An AI-powered price tracker that: (1) scrapes product pages from e-commerce sites on a schedule, (2) extracts structured price data using CRW's LLM extraction with JSON schemas, (3) stores price history in a SQLite database, and (4) sends alerts when prices drop below a threshold or change significantly.

CRW handles the scraping and structured extraction. We'll use Python with the Firecrawl SDK (which works with CRW by changing the API URL), APScheduler for scheduling, and SQLite for storage. By the end, you'll have a fully automated price monitoring system.

## Architecture Overview

The pipeline has four stages:

- **Scrape** — CRW fetches the product page and returns clean markdown
- **Extract** — CRW's `/v1/extract` endpoint uses an LLM to pull structured price data from the page using a JSON schema you define
- **Store** — Price snapshots are saved to SQLite with timestamps for historical tracking
- **Alert** — A comparison function checks for price drops and sends notifications

## Prerequisites

- CRW running locally: `docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`
- Python 3.10+
- An OpenAI API key (used by CRW for LLM extraction)

```
pip install firecrawl-py apscheduler requests
```

## Step 1: Set Up the Firecrawl SDK with CRW

The Firecrawl Python SDK works with CRW out of the box — just point it to your CRW instance:

```
from firecrawl import FirecrawlApp

# Self-hosted CRW
app = FirecrawlApp(api_key="crw_live_YOUR-KEY", api_url="http://localhost:3000")

# Or use fastCRW cloud
# app = FirecrawlApp(api_key="crw_live_YOUR-KEY", api_url="https://api.fastcrw.com")
```

This single change lets you use the full Firecrawl SDK ecosystem with a local-first, low-latency engine instead of a remote multi-second round trip.

## Step 2: Define the Price Extraction Schema

CRW's `/v1/extract` endpoint accepts a JSON schema that tells the LLM exactly what data to pull from the page. Define a schema for product pricing:

```
PRICE_SCHEMA = {
    "type": "object",
    "properties": {
        "product_name": {
            "type": "string",
            "description": "The full product name"
        },
        "current_price": {
            "type": "number",
            "description": "The current selling price in USD"
        },
        "original_price": {
            "type": "number",
            "description": "The original/list price before discounts, null if no discount"
        },
        "currency": {
            "type": "string",
            "description": "The currency code (USD, EUR, GBP, etc.)"
        },
        "in_stock": {
            "type": "boolean",
            "description": "Whether the product is currently in stock"
        },
        "seller": {
            "type": "string",
            "description": "The seller or store name"
        },
        "discount_percentage": {
            "type": "number",
            "description": "The discount percentage if on sale, null otherwise"
        }
    },
    "required": ["product_name", "current_price", "currency", "in_stock"]
}
```

The schema approach is powerful because it works across any e-commerce site — Amazon, Best Buy, Walmart, niche stores — without writing site-specific selectors. The LLM understands the page context and extracts the right data regardless of HTML structure.

## Step 3: Scrape and Extract Product Prices

Now combine scraping with extraction to get structured price data from any product URL:

```
import json
from datetime import datetime

def extract_price(url: str) -> dict | None:
    """Scrape a product page and extract structured price data."""
    try:
        # Use the extract endpoint with our schema
        result = app.extract(
            urls=[url],
            params={
                "prompt": "Extract the product pricing information from this page.",
                "schema": PRICE_SCHEMA,
            }
        )

        if result and "data" in result:
            price_data = result["data"]
            price_data["url"] = url
            price_data["scraped_at"] = datetime.now().isoformat()
            return price_data

    except Exception as e:
        print(f"Error extracting price from {url}: {e}")

    return None

# Test with a single product
product_url = "https://www.example-store.com/product/wireless-headphones"
price = extract_price(product_url)
if price:
    print(json.dumps(price, indent=2))
```

### Alternative: Scrape + Parse Approach

If you prefer more control, you can scrape the page as markdown first, then parse it yourself or send it to your own LLM:

```
def scrape_and_parse(url: str) -> dict | None:
    """Scrape page as markdown and extract price with custom logic."""
    result = app.scrape_url(url, params={"formats": ["markdown"]})

    if not result or "markdown" not in result:
        return None

    markdown = result["markdown"]

    # Option 1: Simple regex for known formats

    price_match = re.search(r"$(d+.?d*)", markdown)
    if price_match:
        return {
            "url": url,
            "current_price": float(price_match.group(1)),
            "raw_markdown": markdown[:500],  # Store context
            "scraped_at": datetime.now().isoformat(),
        }

    return None
```

## Step 4: Set Up the Price Database

Store price snapshots in SQLite so you can track price history over time:

```
import sqlite3
from contextlib import contextmanager

DB_PATH = "price_tracker.db"

def init_db():
    """Create the price tracking tables."""
    with sqlite3.connect(DB_PATH) as conn:
        conn.execute("""
            CREATE TABLE IF NOT EXISTS products (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                url TEXT UNIQUE NOT NULL,
                name TEXT,
                target_price REAL
            )
        """)
        conn.execute("""
            CREATE TABLE IF NOT EXISTS price_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                product_id INTEGER NOT NULL,
                price REAL NOT NULL,
                original_price REAL,
                currency TEXT DEFAULT 'USD',
                in_stock BOOLEAN DEFAULT 1,
                discount_pct REAL,
                scraped_at TEXT NOT NULL,
                FOREIGN KEY (product_id) REFERENCES products(id)
            )
        """)
        conn.commit()

def add_product(url: str, name: str = "", target_price: float = 0.0) -> int:
    """Add a product to track. Returns the product ID."""
    with sqlite3.connect(DB_PATH) as conn:
        cursor = conn.execute(
            "INSERT OR IGNORE INTO products (url, name, target_price) VALUES (?, ?, ?)",
            (url, name, target_price),
        )
        conn.commit()
        if cursor.lastrowid:
            return cursor.lastrowid
        # If already exists, fetch the ID
        row = conn.execute("SELECT id FROM products WHERE url = ?", (url,)).fetchone()
        return row[0]

def save_price(product_id: int, price_data: dict):
    """Save a price snapshot."""
    with sqlite3.connect(DB_PATH) as conn:
        conn.execute(
            """INSERT INTO price_history
               (product_id, price, original_price, currency, in_stock, discount_pct, scraped_at)
               VALUES (?, ?, ?, ?, ?, ?, ?)""",
            (
                product_id,
                price_data.get("current_price"),
                price_data.get("original_price"),
                price_data.get("currency", "USD"),
                price_data.get("in_stock", True),
                price_data.get("discount_percentage"),
                price_data.get("scraped_at", datetime.now().isoformat()),
            ),
        )
        conn.commit()

def get_price_history(product_id: int, limit: int = 30) -> list[dict]:
    """Get recent price history for a product."""
    with sqlite3.connect(DB_PATH) as conn:
        conn.row_factory = sqlite3.Row
        rows = conn.execute(
            """SELECT price, original_price, currency, in_stock, discount_pct, scraped_at
               FROM price_history
               WHERE product_id = ?
               ORDER BY scraped_at DESC
               LIMIT ?""",
            (product_id, limit),
        ).fetchall()
        return [dict(row) for row in rows]
```

## Step 5: Build the Alert System

Detect price changes and send notifications. Here's a simple alerting system that checks for drops:

```
def check_price_alerts(product_id: int, current_price: float) -> list[str]:
    """Check if the current price triggers any alerts."""
    alerts = []

    with sqlite3.connect(DB_PATH) as conn:
        conn.row_factory = sqlite3.Row

        # Get target price
        product = conn.execute(
            "SELECT url, name, target_price FROM products WHERE id = ?",
            (product_id,),
        ).fetchone()

        if not product:
            return alerts

        # Alert 1: Price dropped below target
        if product["target_price"] > 0 and current_price <= product["target_price"]:
            alerts.append(
                f"🎯 TARGET REACHED: {product['name']} is now ${current_price:.2f} "
                f"(target: ${product['target_price']:.2f})"
            )

        # Alert 2: Price dropped significantly from last check
        last_price = conn.execute(
            """SELECT price FROM price_history
               WHERE product_id = ?
               ORDER BY scraped_at DESC LIMIT 1 OFFSET 1""",
            (product_id,),
        ).fetchone()

        if last_price and last_price["price"] > 0:
            change_pct = ((current_price - last_price["price"]) / last_price["price"]) * 100
            if change_pct <= -5:  # 5% or more price drop
                alerts.append(
                    f"📉 PRICE DROP: {product['name']} dropped {abs(change_pct):.1f}% "
                    f"from ${last_price['price']:.2f} to ${current_price:.2f}"
                )
            elif change_pct >= 10:  # 10% or more price increase
                alerts.append(
                    f"📈 PRICE INCREASE: {product['name']} increased {change_pct:.1f}% "
                    f"from ${last_price['price']:.2f} to ${current_price:.2f}"
                )

    return alerts

def send_alert(message: str):
    """Send an alert notification. Customize this for your preferred channel."""
    # Option 1: Print to console
    print(f"\n{'='*60}")
    print(f"ALERT: {message}")
    print(f"{'='*60}\n")

    # Option 2: Send via webhook (Slack, Discord, etc.)
    # import requests
    # requests.post(WEBHOOK_URL, json={"text": message})

    # Option 3: Send email
    # import smtplib
    # ... email sending logic
```

## Step 6: Schedule Automated Price Checks

Use APScheduler to run price checks at regular intervals:

```
from apscheduler.schedulers.blocking import BlockingScheduler

def check_all_prices():
    """Run a price check for all tracked products."""
    print(f"\n[{datetime.now().isoformat()}] Running scheduled price check...")

    with sqlite3.connect(DB_PATH) as conn:
        conn.row_factory = sqlite3.Row
        products = conn.execute("SELECT id, url, name FROM products").fetchall()

    for product in products:
        print(f"  Checking: {product['name'] or product['url']}")
        price_data = extract_price(product["url"])

        if price_data and "current_price" in price_data:
            save_price(product["id"], price_data)

            # Check for alerts
            alerts = check_price_alerts(product["id"], price_data["current_price"])
            for alert in alerts:
                send_alert(alert)

            print(f"    Price: ${price_data['current_price']:.2f}")
        else:
            print(f"    Failed to extract price")

    print(f"Price check complete. Checked {len(products)} products.")

def main():
    """Initialize and start the price tracker."""
    init_db()

    # Add products to track
    products = [
        {
            "url": "https://www.example-store.com/product/wireless-headphones",
            "name": "Sony WH-1000XM5",
            "target_price": 278.00,
        },
        {
            "url": "https://www.example-store.com/product/mechanical-keyboard",
            "name": "Keychron Q1 Pro",
            "target_price": 149.00,
        },
        {
            "url": "https://www.example-store.com/product/4k-monitor",
            "name": "Dell U2723QE",
            "target_price": 450.00,
        },
    ]

    for p in products:
        add_product(p["url"], p["name"], p["target_price"])

    # Run an immediate check
    check_all_prices()

    # Schedule recurring checks every 6 hours
    scheduler = BlockingScheduler()
    scheduler.add_job(check_all_prices, "interval", hours=6)

    print("\nPrice tracker started. Checking every 6 hours.")
    print("Press Ctrl+C to stop.")
    scheduler.start()

if __name__ == "__main__":
    main()
```

## Step 7: Generate Price Reports

Add a reporting function to visualize price trends:

```
def generate_report(product_id: int) -> str:
    """Generate a text-based price report for a product."""
    with sqlite3.connect(DB_PATH) as conn:
        conn.row_factory = sqlite3.Row
        product = conn.execute(
            "SELECT url, name, target_price FROM products WHERE id = ?",
            (product_id,),
        ).fetchone()

    history = get_price_history(product_id, limit=30)

    if not history:
        return f"No price history for product {product_id}"

    prices = [h["price"] for h in history]
    current = prices[0]
    lowest = min(prices)
    highest = max(prices)
    avg = sum(prices) / len(prices)

    report = f"""
Price Report: {product['name']}
URL: {product['url']}
{'='*50}
Current Price:  ${current:.2f}
Lowest Price:   ${lowest:.2f}
Highest Price:  ${highest:.2f}
Average Price:  ${avg:.2f}
Target Price:   ${product['target_price']:.2f}
Data Points:    {len(history)}
{'='*50}
Recent History:
"""
    for h in history[:10]:
        stock = "✓" if h["in_stock"] else "✗"
        discount = f" (-{h['discount_pct']:.0f}%)" if h["discount_pct"] else ""
        report += f"  {h['scraped_at'][:16]}  ${h['price']:.2f}{discount}  [{stock}]\n"

    return report
```

## Monitoring Multiple Competitors

Track the same product across multiple stores to find the best deal:

```
def track_across_stores(product_name: str, urls: list[str], target_price: float):
    """Track the same product across multiple stores."""
    for url in urls:
        add_product(url, f"{product_name} - {url.split('/')[2]}", target_price)

def find_best_price(product_name: str) -> dict | None:
    """Find the current best price across all tracked stores for a product."""
    with sqlite3.connect(DB_PATH) as conn:
        conn.row_factory = sqlite3.Row
        result = conn.execute(
            """SELECT p.url, p.name, ph.price, ph.in_stock, ph.scraped_at
               FROM products p
               JOIN price_history ph ON p.id = ph.product_id
               WHERE p.name LIKE ?
               AND ph.in_stock = 1
               AND ph.scraped_at = (
                   SELECT MAX(scraped_at) FROM price_history WHERE product_id = p.id
               )
               ORDER BY ph.price ASC
               LIMIT 1""",
            (f"%{product_name}%",),
        ).fetchone()

    return dict(result) if result else None
```

## Why CRW for This?

Price tracking requires frequent, reliable scraping across diverse e-commerce sites. CRW brings three key advantages:

- **LLM extraction** — The `/v1/extract` endpoint with JSON schemas means you don't need to write fragile CSS selectors for each store. Define your schema once, and it works across Amazon, Best Buy, or any niche store.
- **Low-latency, local-first** — Running the engine next to your scheduler avoids remote API round trips, so checking a batch of products stays quick enough to run every few hours.
- **Lightweight** — A single small static binary with a modest idle footprint. Run it alongside your database and scheduler on the same machine with no resource contention.

## Next Steps

- Read [How to Build a RAG Pipeline with CRW](/blog/rag-pipeline-with-crw) to add natural language queries to your price data
- Check out [Website to Markdown with CRW](/blog/website-to-markdown) for more on CRW's content extraction
- See [CRW vs Firecrawl](/blog/firecrawl-vs-crawl4ai-vs-crw) for a detailed comparison

Self-host CRW from [GitHub](https://github.com/fastcrw/crw) for free, or use [fastCRW](https://fastcrw.com) for managed cloud scraping with no infrastructure to maintain.

## FAQ

### How does CRW extract prices without site-specific selectors?

You pass a JSON schema to the extract step describing the fields you want — product_name, current_price, currency, in_stock — and the LLM reads the page context to fill them in. Because it understands meaning rather than markup, the same schema works across Amazon, Best Buy, Walmart, and niche stores without writing or maintaining fragile CSS selectors per site.

### What does it cost to run an AI price tracker with CRW?

Self-hosting the engine is free under AGPL-3.0 — you only pay for your own server and the OpenAI or Anthropic key used for extraction. On the managed cloud, any request using formats: ["json"] (the /v1/extract endpoint) costs 5 credits, so the Hobby tier's 3,000 monthly credits covers roughly 600 price checks a month.

### Should I use the /v1/extract endpoint or scrape-and-parse myself?

Use /v1/extract when you want structured fields straight out of the box with no parsing code — it is the simplest path. Use the scrape-and-parse approach when you want more control or lower cost: a plain markdown scrape costs 1 credit versus 5 for json extraction, and you can run your own regex or LLM over the markdown.

### Is /v1/extract available when self-hosting CRW?

The /v1/extract convenience endpoint is a managed-cloud feature. When self-hosting, call /v1/scrape directly with formats: ["json"] and a jsonSchema — it produces the same structured output. Either way, LLM extraction supports OpenAI and Anthropic providers only.

### How fast and reliable is CRW for frequent price checks?

On Firecrawl's public scrape-content-dataset-v1 (1,000 URLs, harness diagnose_3way.py, run 2026-05-08), fastCRW recorded a 1914 ms p50 latency — in the same band as Crawl4AI's 1916 ms and Firecrawl's 2305 ms — with 91.8% scrape-success of reachable URLs and 0 thrown errors across 3,000 requests. Running the engine local-first next to your scheduler avoids remote round trips, so a batch check every few hours stays quick.

### Can I track the same product across multiple stores?

Yes. The track_across_stores helper registers one product under several store URLs, and find_best_price queries the latest in-stock snapshot across all of them ordered by price. This turns the tracker into a price-comparison tool that surfaces the cheapest current source for any product you follow.
