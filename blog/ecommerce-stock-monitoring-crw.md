# E-Commerce Stock & Restock Monitoring in Python with CRW (2026)

> Build a restock monitor: poll product pages with CRW, extract stock status via JSON schema, detect in-stock transitions, and fire instant alerts. Full runnable Python — self-host free under AGPL-3.0.

**Published:** 2026-05-21  
**Updated:** 2026-05-21  
**Canonical:** https://fastcrw.com/blog/ecommerce-stock-monitoring-crw

---

## What We're Building

A restock monitor that watches product pages and notifies you the moment a sold-out item comes back in stock — or the moment a watched item sells out. Stock badges live in wildly different HTML across stores, so we use CRW's LLM extraction with a JSON schema to read availability semantically. The monitor only alerts on *transitions*, so you do not get spammed every poll.

## Architecture

- **Extract** — CRW `/v1/extract` returns `in_stock` + variant data per product
- **State** — SQLite stores the last known status per product
- **Transition detect** — Alert only when status flips
- **Notify** — Pluggable webhook / email sink

## Prerequisites

- CRW running: `docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest`
- Python 3.10+ and an OpenAI API key (used by CRW for extraction)

```
pip install firecrawl-py requests
```

## Step 1: SDK Setup

```
from firecrawl import FirecrawlApp

app = FirecrawlApp(api_key="crw_live_YOUR-KEY", api_url="http://localhost:3000")
# fastCRW cloud: api_url="https://api.fastcrw.com"
```

## Step 2: Availability Schema

```
STOCK_SCHEMA = {
    "type": "object",
    "properties": {
        "product_name": {"type": "string"},
        "in_stock": {"type": "boolean",
                     "description": "True if the product can be purchased now"},
        "availability_text": {"type": "string",
                              "description": "The raw availability label shown, e.g. 'In stock', 'Backorder'"},
        "price": {"type": "number", "description": "Current price, null if unknown"},
        "variants": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Variant label, e.g. size or color"},
                    "in_stock": {"type": "boolean"},
                },
                "required": ["name", "in_stock"],
            },
        },
    },
    "required": ["product_name", "in_stock"],
}
```

## Step 3: Check One Product

```
def check_stock(url: str) -> dict | None:
    result = app.extract(
        urls=[url],
        params={
            "prompt": "Determine whether this product is currently purchasable and list per-variant availability.",
            "schema": STOCK_SCHEMA,
        },
    )
    if result and "data" in result:
        return result["data"]
    return None
```

## Step 4: State Store

```
import sqlite3, hashlib
from datetime import datetime

DB = "stock.db"

def init_db():
    with sqlite3.connect(DB) as c:
        c.execute("""CREATE TABLE IF NOT EXISTS watch (
            id TEXT PRIMARY KEY, url TEXT, name TEXT,
            last_in_stock INTEGER, last_checked TEXT)""")

def pid(url: str) -> str:
    return hashlib.sha256(url.encode()).hexdigest()[:16]

def get_last(url: str):
    with sqlite3.connect(DB) as c:
        row = c.execute("SELECT last_in_stock FROM watch WHERE id=?",
                         (pid(url),)).fetchone()
        return None if row is None else bool(row[0])

def set_last(url: str, name: str, in_stock: bool):
    with sqlite3.connect(DB) as c:
        c.execute("""INSERT INTO watch VALUES (?,?,?,?,?)
                     ON CONFLICT(id) DO UPDATE SET
                       last_in_stock=excluded.last_in_stock,
                       last_checked=excluded.last_checked,
                       name=excluded.name""",
                  (pid(url), url, name, int(in_stock),
                   datetime.now().isoformat()))
```

## Step 5: Transition Detection + Alerts

```
import requests

WEBHOOK_URL = ""  # Slack/Discord incoming webhook, optional

def notify(message: str):
    print(f"[ALERT] {message}")
    if WEBHOOK_URL:
        try:
            requests.post(WEBHOOK_URL, json={"text": message}, timeout=10)
        except requests.RequestException as e:
            print(f"  webhook failed: {e}")

def process(url: str):
    data = check_stock(url)
    if not data:
        print(f"  could not read {url}")
        return

    now_in = bool(data["in_stock"])
    name = data.get("product_name", url)
    prev = get_last(url)

    if prev is None:
        print(f"  baseline: {name} -> {'IN' if now_in else 'OUT'} of stock")
    elif prev is False and now_in is True:
        notify(f"RESTOCK: {name} is back in stock — {url}")
    elif prev is True and now_in is False:
        notify(f"SOLD OUT: {name} just went out of stock — {url}")

    # Variant-level restock
    for v in data.get("variants", []):
        if v.get("in_stock"):
            print(f"    variant available: {v['name']}")

    set_last(url, name, now_in)
```

## Step 6: Polling Loop

Poll watched products at an interval. Jitter the sleep so requests do not hammer a store on a fixed cadence:

```
import time, random

WATCH = [
    "https://store.example.com/p/limited-sneaker",
    "https://store.example.com/p/gpu-rtx",
]

def run(interval_sec: int = 300):
    init_db()
    while True:
        for url in WATCH:
            print(f"Checking {url}")
            process(url)
            time.sleep(random.uniform(2, 6))  # be polite between products
        time.sleep(interval_sec + random.uniform(0, 60))

if __name__ == "__main__":
    run()
```

## Why "In Stock" Is Harder Than It Looks

Availability is one of the most adversarially-designed signals on the web. The same product can show "In stock" while the add-to-cart button is disabled, "Only 2 left" as a permanent urgency tactic, "Available for pre-order" (purchasable but not shippable), or a region-gated price that hides stock entirely until you pick a store. A selector that keys off a single badge class is wrong on at least one of these for almost every retailer, and the failure is silent — you simply stop getting restock alerts and never know why. This is precisely the case where semantic extraction earns its cost: the schema asks "can a user buy this right now" and the model reasons over the whole page (button state, badge text, shipping copy) rather than trusting one element. The `availability_text` field is deliberately included so you can audit what the model saw and tighten the prompt if a specific store fools it.

Treat `in_stock` as a decision, not a scrape. The right architecture, which this tutorial follows, is: extract a normalized boolean, persist it, and alert only on the transition. That decouples "how do we read this messy page" (CRW's job) from "what does a state change mean" (your job), and it means a one-off misread self-corrects on the next poll instead of firing a false restock alert.

## Resilient Polling: Backoff, Jitter, and Circuit Breaking

A monitor that hammers a store the moment it gets blocked makes the block worse and burns your own resources. Wrap the check in retry-with-backoff, and add a simple circuit breaker so a product that fails repeatedly is temporarily skipped instead of retried every cycle:

```
import time, random

_failures: dict[str, int] = {}

def check_resilient(url: str, max_attempts: int = 3) -> dict | None:
    # circuit breaker: back off products that keep failing
    if _failures.get(url, 0) >= 5:
        if random.random() > 0.2:        # only 1-in-5 cycles retries
            print(f"  circuit open, skipping {url}")
            return None

    delay = 3.0
    for attempt in range(1, max_attempts + 1):
        data = check_stock(url)
        if data:
            _failures[url] = 0
            return data
        time.sleep(delay + random.uniform(0, delay))
        delay *= 2

    _failures[url] = _failures.get(url, 0) + 1
    return None
```

The jitter (`random.uniform`) is not cosmetic — fixed-interval requests are the easiest pattern for a site to fingerprint and rate-limit. Randomizing both the inter-product delay and the backoff makes the monitor look like organic traffic and keeps it working longer.

## Alert Fatigue Is the Real Failure Mode

The fastest way to make a stock monitor useless is to make it noisy. If it pings on every poll, people mute the channel and miss the one alert that mattered. The transition-only design prevents repeat alerts, but two more rules sharpen it. First, debounce flapping: if a product oscillates in-stock/out-of-stock within a short window (common during a chaotic restock), collapse it into a single "restocked, going fast" alert rather than ten. Second, rank alerts — a restock on a target product the user is actively waiting for deserves a push notification; a routine price wiggle belongs in a daily summary. Encoding this priority in `notify()` (push vs digest) is what separates a tool people trust from one they ignore.

## Production Notes

- **Backoff on failure** — wrap `check_stock` with retry + exponential backoff so a transient block does not flap your alerts.
- **Idempotent alerts** — transition detection already prevents repeat notifications; never alert from raw status.
- **Run CRW alongside it** — with a low idle footprint, CRW fits on the same small box as the monitor.

## Why CRW

- **Schema extraction** reads "Add to cart" vs "Notify me" semantically — no per-store selector maintenance.
- **Open-core Rust**, small single binary, lower-latency, local-first, AGPL-3.0 + Managed Cloud.
- **No credit traps** when self-hosting; unlimited polling within your own infra.

## A Notification Sink You Can Trust

The default `notify()` prints and optionally posts a webhook. In practice you want delivery you can verify and a record of what fired, so a missed restock is debuggable after the fact. Persist every alert and make delivery best-effort but logged:

```
def notify(message: str, priority: str = "normal"):
    ts = datetime.now().isoformat()
    with sqlite3.connect(DB) as c:
        c.execute("""CREATE TABLE IF NOT EXISTS alerts
                     (ts TEXT, priority TEXT, message TEXT, delivered INTEGER)""")
        delivered = 0
        if WEBHOOK_URL:
            try:
                r = requests.post(WEBHOOK_URL,
                                  json={"text": message}, timeout=10)
                delivered = 1 if r.ok else 0
            except requests.RequestException as e:
                print(f"  delivery failed: {e}")
        c.execute("INSERT INTO alerts VALUES (?,?,?,?)",
                  (ts, priority, message, delivered))
    print(f"[{priority.upper()}] {message}")
```

Now an alert that failed to deliver is a row with `delivered=0` you can re-send or inspect, instead of a notification that vanished into a flaky webhook. For a monitor whose entire value is "tell me at the right moment," an auditable alert log is not optional polish — it is the part users actually depend on.

## Next Steps

- See [Build an AI Price Tracker](/blog/ai-price-tracker) to add price-history to the same monitor
- Read [Competitor Monitoring with CRW](/blog/competitor-monitoring-crw)

Self-host CRW from [GitHub](https://github.com/fastcrw/crw) for free, or use [fastCRW](https://fastcrw.com) for managed cloud scraping.

## FAQ

### How do I avoid getting alerted on every poll?

Alert only on transitions. Store the last known in_stock value per product and fire a notification only when it flips from out-of-stock to in-stock (or vice versa). The process() function in this tutorial implements exactly this, with a silent baseline on the first observation.

### Can it track per-variant availability like size or color?

Yes. The JSON schema includes a variants array, so CRW returns in-stock status per variant. You can fire an alert when any specific watched variant becomes available rather than only on the overall product flag.
