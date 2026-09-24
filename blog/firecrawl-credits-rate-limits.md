# Firecrawl Credits and Rate Limits, Demystified (2026)

> How Firecrawl's credit accounting and concurrency limits actually work — what burns credits, how rate limits map to tiers, why agent traffic blows through caps, and how to model and cap your real spend.

**Published:** 2026-05-23  
**Updated:** 2026-05-23  
**Canonical:** https://fastcrw.com/blog/firecrawl-credits-rate-limits

---

*By the fastCRW team · Credit/limit data verified 2026-05-18 · Confirm on firecrawl.dev before relying on these.*

**Disclosure:** Written by the fastCRW team. fastCRW is a Firecrawl-compatible alternative. The Firecrawl mechanics below are from public docs and pricing as of 2026-05-18.

## Two budgets, not one

Every hosted scraping API gives you two budgets that throttle you in different ways: a **credit budget** (how much total work per billing period) and a **concurrency budget** (how many requests in flight at once). Teams plan the first and forget the second, then get surprised when an agent fleet hits the concurrency wall long before the credit cap. Understand both.

## How Firecrawl credits accrue

The base mapping is straightforward and honest:

- **Scrape:** ~1 credit per page.
- **Crawl:** ~1 credit per page crawled — so a crawl's cost is its page count, not "one request."
- **Search:** ~2 credits per 10 results.
- **Interact / browser actions:** ~2 credits per browser-minute.

Two things to internalize. First, **crawl is the credit sink**: a single crawl call over a 5,000-page site is ~5,000 credits. Always map a site first to know what you're committing. Second, render-heavy and structured-extraction modes can bill more than the headline 1 credit/page depending on configuration, and Firecrawl's AI extraction is widely reported to draw on a **separate token-based subscription** rather than your credit pool. So the credit budget alone does not predict the bill if you use extraction.

## The free-tier credit cliff

The most-cited surprise: Firecrawl's free tier is **1,000 credits one-time** — a lifetime grant, not a monthly refill. A 600-page crawl plus a few hundred test scrapes can exhaust the entire free allowance in one evaluation session, with no monthly reset to wait for. When you compare free tiers, "1,000 one-time" and "1,000/month" are completely different economics even though the page shows the same number. fastCRW's free tier, by contrast, is a one-time lifetime 1000 credits *plus* a free unlimited local self-host mode — the local mode is the part that actually removes the cliff for development and CI.

## How rate limits map to tiers

Concurrency scales with the plan. Approximate published concurrency by tier:

| Tier | Price | Concurrency | Credits |
| --- | --- | --- | --- |
| Free | $0 | 2 | 1,000 one-time |
| Hobby | $16/mo | 5 | 5,000/mo |
| Standard | $83/mo | 50 | 100,000/mo |
| Growth | $333/mo | 100 | 500,000/mo |
| Scale | $599/mo | 150 | 1,000,000/mo |

The implication people miss: concurrency, not credits, is often the first ceiling. An agent product that fans out 80 simultaneous scrapes is over the Standard concurrency budget (50) even if it's nowhere near 100,000 credits — so you get throttled, queued, or pushed to upgrade for a reason that has nothing to do with total volume.

## Why agent traffic blows through caps

Human-driven scraping is steady. Agent-driven scraping is bursty by nature: a single user task can trigger a fan-out of dozens of page fetches in a few seconds, then nothing for minutes. Two failure modes follow:

1. **Concurrency spikes** hit the per-tier in-flight limit during bursts, even at low average throughput. You feel this as latency and 429s under load.
2. **Credit variance** is high: a few "expensive" tasks (deep crawls, extraction-heavy pages) dominate the bill, so the monthly average badly underestimates the worst month.

Forecast the worst month, not the average. Spiky workloads are where surprise overage charges and forced tier upgrades come from.

## A credit-modeling worksheet

1. **Classify your calls:** scrape vs crawl vs search vs extract. Estimate monthly volume for each.
2. **Cost crawls by page count:** for each crawl target, map it first; sum page counts × ~1 credit.
3. **Add the extract line separately:** if you extract structured JSON on Firecrawl, add the separate-subscription floor (reported ~$89/mo on top of plan). Don't bury it inside the credit estimate.
4. **Find the binding ceiling:** compute both monthly credits and peak concurrency. Whichever hits its tier limit first is your real constraint — size the plan to that, not the other one.
5. **Stress the worst month:** multiply the spiky portion by 2–3x. If that breaches a cap, plan the next tier or an escape hatch now.

## Capping the downside structurally

All of the above assumes the only lever you have is "pick a bigger tier." That's true for hosted-only products. It is not true if your engine is open-core.

fastCRW uses a 1-credit-per-page model with no separate extract subscription — JSON extraction is part of the scrape call under the same credit — so the credit estimate *is* the bill, not a floor. Its launch tiers undercut Firecrawl tier-for-tier ($13 vs $16 Hobby, $69 vs $83 Standard, $279 vs $333 Growth, $549 vs $599 Scale; launch pricing expires 2026-06-01). More importantly, the same engine is a single ~10MB (compressed download) AGPL-3.0 Rust binary you can self-host with **unlimited requests and no per-request credit at all**. That converts the "what if we blow the cap" risk from a budgeting problem into a deployment choice: when metering stops making sense, run the same Firecrawl-compatible API yourself.

## Practical guardrails regardless of vendor

- **Map before you crawl** so a crawl's credit cost is known before you commit it.
- **Cap crawl depth and page limits** in the request — an unbounded crawl is an unbounded bill.
- **Read credit metadata in responses** and aggregate it; don't trust headline rates for render/extract modes.
- **Alert on burn rate**, not just total — a sudden slope change predicts the overage before it lands.
- **Keep a self-host fallback configured** so a pricing or cap surprise is a config flip, not an outage.

## A worked burn-rate model you can copy

Spreadsheets beat intuition for credit forecasting. Here is the model, in plain terms, that has saved teams from surprise tier jumps:

- **Baseline:** average pages/day × 30 = monthly base credits. This is the number people quote and the number that is always wrong on its own.
- **Crawl spikes:** list every recurring crawl, multiply each by its mapped page count and frequency. A weekly 4,000-page refresh is 16,000+ credits/month by itself — add these explicitly, do not fold them into the average.
- **Burst factor:** for agent traffic, multiply the agent-driven portion by 2–3x to model the worst week. Agent fan-out is not Poisson; a handful of heavy tasks dominate.
- **Extract line:** if you extract on Firecrawl, add the separate-subscription floor as a fixed line (~$89/mo reported), independent of the credit total. Burying it inside credits is the single most common forecasting error.
- **Ceiling check:** if baseline + crawl spikes + burst exceeds ~70% of your tier's credit cap, you will cross it in a normal month. Plan the next tier or the self-host valve now, not after the overage email.

Re-run this monthly against actuals. The gap between forecast and actual is itself a signal: a widening gap usually means a crawl grew or an agent behavior changed, and catching that early is the difference between a planned upgrade and a surprised one.

## Concurrency tuning that does not waste a tier

Because concurrency is often the binding limit before credits, treat it as a knob, not a given:

1. **Measure your true peak in-flight count**, not your average. Instrument the client; agents lie about this in design docs.
2. **Add a client-side concurrency limiter** sized just under the tier ceiling. Hitting the server's limit produces 429s and retries that waste latency and sometimes credits; a local semaphore converts that into smooth queuing you control.
3. **Separate latency-critical from batch traffic.** Inline agent scrapes and background ingestion crawls should not share one concurrency budget — starve neither by classifying and prioritizing.
4. **Decide between buying concurrency and self-hosting it.** Upgrading a tier purely to raise a concurrency ceiling is paying for credits you may not use. A self-hosted engine you scale by adding a stateless replica behind a load balancer decouples concurrency from credit spend entirely.

## The structural fix, restated as a principle

Every guardrail above manages a meter you do not control. The principle worth internalizing: *a cost model with no floor is a risk, not a budget.* An open-core engine puts a floor under the model — when credits or concurrency stop making economic sense, the same Firecrawl-compatible API runs on a single ~10MB (compressed download) AGPL-3.0 binary with no per-page meter and concurrency limited only by hardware you provision. You keep the credit model for convenience and keep the self-host option as the cap. That combination — predictable per-page credits with no extract dual-billing, plus an unmetered escape valve — is what turns credit and rate-limit planning from a recurring fire drill into a one-time architectural decision.

## Sources

- Firecrawl pricing and credit docs: [firecrawl.dev/pricing](https://www.firecrawl.dev/pricing) (verified 2026-05-18)
- fastCRW credit model and self-host: [github.com/fastcrw/crw](https://github.com/fastcrw/crw)

Related: [Firecrawl pricing explained](/blog/firecrawl-pricing-explained) · [Migrate from Firecrawl](/blog/migrate-from-firecrawl)

## FAQ

### How many credits does a Firecrawl crawl use?

Roughly 1 credit per page crawled. A crawl over a 5,000-page site costs about 5,000 credits, not 1. Always map a site first so the credit cost is known before you commit the crawl.

### Do Firecrawl credits reset monthly on the free tier?

No. The free tier is a one-time lifetime grant of 1,000 credits, not a monthly allowance. fastCRW's free tier is a one-time lifetime 1000 credits plus a free unlimited local self-host mode that removes the cliff for dev and CI.

### What hits first, the credit cap or the concurrency limit?

For bursty agent traffic, usually concurrency. An agent fanning out 80 simultaneous scrapes exceeds Standard's ~50 concurrency budget long before reaching 100,000 credits. Size your plan to whichever ceiling binds first.
