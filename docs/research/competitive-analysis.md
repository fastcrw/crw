# Competitive analysis: crawling and extraction engines

Implementation-level notes from reading the source of seven open-source
crawling, scraping and browser-automation projects, recorded so the
conclusions behind a change can be checked later without re-doing the
reading.

This document exists to support code changes, not to replace them. Where an
idea has already turned into a patch, the section says so. Where an idea was
rejected, the reason is recorded so it does not get re-proposed.

**Method.** Each repository was cloned at a pinned commit and read as source.
README and marketing claims were not treated as evidence. For every idea the
question asked was not "does this project do something clever" but "does the
*constraint* that made them build it also exist in crw" — with a citation into
crw's own tree either way. Ideas that solve a problem crw does not have are
recorded as rejected, not as future work.

**Licensing.** Nothing in this document is copied code. crw is AGPL-3.0.
Mechanisms are described in prose precisely enough to be reimplemented from
the description; the reimplementation is original work. Where a project's
license would attach obligations (Apache-2.0 NOTICE, BSD-3 attribution and
no-endorsement, MPL-2.0 file-level copyleft) that is noted, and none of those
obligations were incurred because no source was taken.

## Repositories read

| Project | Commit | License | Read for |
|---|---|---|---|
| `apify/crawlee` | `3a248f4c0b18230310d46c7dd9dfefadb9c4b87e` | Apache-2.0 | request queue, session pool, autoscaling, adaptive rendering, throttling |
| `D4Vinci/Scrapling` | `48da61d1ee85cea7bbbdff013d98c90602e1d93f` | BSD-3-Clause | fetcher tiers, HTTP/browser identity consistency, adaptive throttling |
| `unclecode/crawl4ai` | `862f6bccb9c063f49b9d42701baa0eea17a4993f` | Apache-2.0 | content pruning, markdown generation, schema-driven extraction, caching |
| `firecrawl/firecrawl` | `5e57a065ac4d958399b70d11bd5fdf167bc723a3` | AGPL-3.0 (root); `apps/api/package.json` declares ISC — the metadata contradicts the root LICENSE | scrape pipeline, engine selection, index cache, error taxonomy |
| `ultrafunkamsterdam/nodriver` | `a71cda374651d13815a42c5eeb61af04a711eaa7` | AGPL-3.0 | browser lifecycle, profile management, CDP transport limits |
| `Kaliiiiiiiiii-Vinyzu/patchright` | `26ab9ae74516a68077f218b5de763d85be9f6d5a` | Apache-2.0 | CDP side effects, dialog handling, isolated worlds, launch flags |
| `daijro/camoufox` | `571e416ac1ee52f055afc1f4e2e8ccd2b8d2ec17` | MPL-2.0 | internal consistency of browser identity configuration |

Firecrawl's production main-content extractor (`transformHtml` from
`@mendable/firecrawl-rs`) and its boilerplate-signature SQL are **not** in the
public repository. Conclusions about Firecrawl's extraction below are drawn
from the in-tree cheerio fallback and from call sites, and are marked as such.

## Scope note: what was deliberately not researched

The browser-automation reading was scoped to robustness and internal
consistency — keeping a correctly-configured crawler from being
false-positive-blocked because its automation setup is visibly broken, and
keeping browser processes from leaking. Techniques whose purpose is defeating
an explicit access-control decision (CAPTCHA solving, authentication or
paywall bypass) were out of scope and are not recorded here.

## Where crw is already ahead

Recording this matters as much as the gaps: several areas came back with
nothing to adopt, and that is a result.

- **Browser context pooling and teardown.** `crates/crw-renderer/src/browser_pool.rs`
  has an explicit slot state machine, a single-shot `terminator` flag
  arbitrating release-vs-drop-vs-shutdown, per-phase recycle timing and a
  three-phase drain. Crawlee's browser pool (count- and age-based retirement)
  is materially less rigorous. Nothing to take.
- **Signal-robust process-group teardown.** `crates/crw-renderer/src/browser.rs`
  registers a process *group* before readiness polling, so a Ctrl-C during
  browser startup still reaps. nodriver's equivalent (`stop()`, a three-attempt
  try/except cascade) is weaker.
- **Navigation lifecycle.** crw's readiness logic — load event, network-idle
  pump, SPA selector poll, content-stability tick — is more sophisticated than
  all three browser projects. nodriver's `Tab.wait()` is a sleep. This axis
  came back empty, which is itself worth knowing.
- **Resource interception.** `blocklist.rs` plus the intercept pump's
  outstanding-request tracking (so `Fetch.disable` cannot fail open) exceeds
  what the research targets do.
- **Table handling.** `table_normalize.rs` implements `rowspan`/`colspan`
  normalization; no target has an equivalent. crw's data-table-vs-layout-table
  scoring matches crawl4ai's and uses the same threshold.
- **Markdown fidelity.** crw's nested-list and GFM-table-safe indent handling
  is far ahead of crawl4ai's equivalent (a single string replacement).
- **Per-field evidence/attribution** (`basis.rs`, `evidence.rs`). No target has
  anything comparable. This is crw's clearest differentiator in agent-facing
  extraction.
- **Block and challenge classification** (`detector.rs`, `blocklist.rs`).
  Crawlee classifies on three status codes. crw is far ahead. What crw lacks is
  not the *detector* but a consequence for the verdict — see below.

## Ideas adopted

### Separate the dedup key from the URL that gets fetched

**Source.** Crawlee, `packages/core/src/request.ts` — a `Request` carries both
`url` (byte-exact, what goes on the wire) and `uniqueKey` (normalized, used
only for dedup). Normalization never feeds back into what is requested.

**Constraint.** Dedup wants aggressive canonicalization; fetching wants byte
fidelity. Conflating them silently rewrites requests.

**Did crw have the constraint?** Yes. `crw-crawl`'s `normalize_url` lowercased
the entire URL including path and query, and the result was what got enqueued
and fetched, and what `/map` returned. On a case-sensitive origin a discovered
`/docs/Guide` went out as `/docs/guide`.

**Status: adopted.** The key now folds only scheme and host (RFC 3986
§6.2.2.1); the crawl enqueues the link as the page wrote it.

### Honor `Retry-After`, and let a 429 slow the host limiter down

**Source.** Crawlee, `packages/basic-crawler/src/internals/throttling_request_manager.ts`
— two independent per-domain clocks (reactive backoff, proactive crawl delay),
dispatch gated on the later of the two. Backoff is `Retry-After` when present,
else exponential from a base, capped. The load-bearing detail is in-flight
burst suppression: requests already on the wire when the limit was hit all come
back 429 and describe *one* rate-limit event, so only the first advances the
exponent — without that, concurrency alone drives the exponent to its cap
immediately. Scrapling (`scrapling/spiders/throttle.py`) contributes
`parse_retry_after` handling both the delta-seconds and HTTP-date forms.

**Constraint.** A static requests-per-second is simultaneously too fast for
fragile origins and too slow for robust ones, and ignores the origin telling
you exactly how long to wait.

**Did crw have the constraint?** Yes. `Retry-After` was parsed nowhere on the
fetch path. A 429 fed the egress latch, which responds by moving to *paid proxy
egress* rather than by slowing down — converting a soft rate limit into ongoing
cost and a harder ban.

**Status: adopted** for the parse-and-wait half. The adaptive
latency-targeting half is recorded under future work: it is only load-bearing
once the crawl dispatch loop is actually concurrent.

### Answer `Page.javascriptDialogOpening`

**Source.** patchright, `driver_patches/crPagePatch.ts` — subscribe to the
dialog event *before* issuing `Page.enable`, because `Page.enable` can
immediately emit a dialog for a newly opened popup.

**Constraint.** Once a CDP client enables the `Page` domain, Chrome stops
auto-dismissing `alert`/`confirm`/`beforeunload` and blocks the renderer's main
thread until the client answers. An unanswered dialog means the load event
never arrives.

**Did crw have the constraint?** Yes, squarely: crw enables `Page` per session
and has no dialog handler, so a page that calls `alert()` during load burns the
full page timeout and every subsequent tier does the same.

**Status: adopted.**

### Bound the response body read, not just the advertised length

**Source.** General practice across the targets; crw's own gap found by audit.

**Constraint.** `Content-Length` is absent on any chunked/streamed response and
reports the *compressed* size when transport decompression is on, so a
pre-read length check does not bound memory.

**Status: adopted.**

## Ideas adapted, not adopted wholesale

### Per-node composite scoring and recursive DOM pruning

**Source.** crawl4ai, `crawl4ai/content_filter_strategy.py` —
`PruningContentFilter` scores every node on a weighted mean of text density,
link density, tag weight, class/id penalty and log text length, prunes below a
threshold, and recurses only into nodes that survived. A dynamic mode adapts
the threshold per node by tag importance and link ratio.

**Did crw have the constraint?** Yes. crw's `readability.rs` is a two-tier
hardcoded selector whitelist (`article`/`main`/`[role=main]`, then ~24
site-specific selectors) with a whole-body fallback. Its own source documents
the consequence: on page-builder sites the whole-page candidate wins on word
count and the mega menu reaches the markdown even for `onlyMainContent`
requests.

**Verdict: adapt, do not adopt wholesale.** crw's `clean.rs` token lists are
measured against a frozen corpus and are more precise than crawl4ai's regex;
replacing them would regress. The right shape is to add per-node pruning as one
more rung in crw's existing quality-scored candidate ladder, so it only wins
when it beats the incumbent and the recall gate arbitrates. Not attempted here:
it moves scrape success, which is a hard pre-merge gate, and needs benchmark
evidence this work did not produce.

### Deterministic CSS/XPath *schema* extraction

**Source.** crawl4ai, `crawl4ai/extraction_strategy.py` —
`JsonCssExtractionStrategy`. A schema is `{baseSelector, fields[]}`; each
`baseSelector` match produces one record; field types are `text`/`attribute`/
`html`/`regex`/`nested`/`list`/`nested_list`, selectors resolve relative to the
current element, and a `type` may be a pipeline (read an attribute, then regex
it). `computed` fields with an `expression` are deliberately disabled as an
eval-on-untrusted-input hazard — a precedent worth keeping.

**Did crw have the constraint?** Yes. crw has single-selector narrowing and
LLM-only structured JSON; every structured extraction costs a model call.
`crw-extract` already links both a CSS engine and an XPath engine, so the
machinery exists.

**Verdict: adapt.** This is the single highest-value capability gap found and
it changes crw's cost curve, but it is a new public API surface, not a bug fix,
and it needs a design decision from maintainers rather than a drive-by PR.
Recorded as future work.

### Proxy/session health as a decaying score

**Source.** Crawlee's `SessionPool` — a session carries an error score
(+1 on failure, −0.5 on success, retire at 3), and a blocked response is
treated as a fact about the *session*, not the request: the session is retired
and the request re-enqueued so the next attempt picks a different identity.

**Did crw have the constraint?** Partly. crw's default proxy selection is
`fnv1a(host) % len` — deliberately stateless and idempotent, which also means
that once an exit IP is burnt for a host it stays burnt for the rotator's
lifetime with no path back. crw's existing health state (renderer preference,
egress latch, tier breaker) is all host-scoped; none of it can say "this exit
is burnt for this host, pick another."

**Verdict: adapt, narrowly.** Do not port `SessionPool`; crw's single-shot
deadline-driven request model does not want a session pool. The two pieces
worth taking are a `(host, proxy) → decaying score` map that lets `pick_index`
skip a retired pair, and a crawl-level retry budget that distinguishes
"identity is burnt" from "transport failed". Recorded as future work — it is a
behavior change on the proxy path and wants its own benchmark.

## Ideas rejected

- **Crawlee's `AutoscaledPool` load signals** (CPU / event-loop lag / memory
  driving concurrency). crw has no event loop to measure and its concurrency is
  already bounded by explicit semaphores, reserved lanes and per-host caps. The
  one fragment worth revisiting someday is an RSS-based admission gate on the
  browser pool specifically, since browsers are the only real memory risk.
- **Firecrawl's generated-JavaScript extractors.** Firecrawl generates
  arbitrary JS per URL shape and runs it in a WebSocket code sandbox with an
  `askLlm` host callback the generated code can invoke mid-run. For a
  single-static-binary Rust product with crw's fail-closed posture, standing up
  a JS sandbox is a security and ops liability that buys nothing over a
  declarative schema. The *caching discipline* around it is worth keeping (cache
  key includes the codegen model and schema hash); the sandbox is not.
- **Firecrawl's `supportScore`/`priority` engine-selection arithmetic.** Opaque
  by its own source's admission, tuned for ~15 engines where crw has a handful,
  and it would fight crw's hard-pin invariants. The one extractable piece is the
  idea of a declarative `{engine × capability} → bool` table, which would let
  "screenshot requests only use capture-capable renderers" become a testable
  table lookup instead of imperative checks scattered across the tree.
- **Scrapling's adaptive element relocation** (persisting element fingerprints
  across runs to survive selector rot). It solves a problem a stateful per-site
  scraper has. crw's primary output is readability-derived main content, and a
  user selector that matches nothing already returns empty plus an explicit
  warning, which is the correct behavior.
- **Crawlee's request-queue persistence and crawl resumption.** crw's crawl
  jobs are in-memory, so a restart loses them — but that is a product decision
  about job durability, not a crawler defect.
- **patchright's `Console.enable` removal.** It works by disabling the Console
  API wholesale; `crw-browse` ships a console tool as a product feature. The
  cure costs more than the disease.
- **camoufox's fingerprint-randomization core.** crw wants one coherent
  identity, not a randomized one. The valuable part of camoufox for crw is the
  *consistency discipline* — derive every identity axis from one source and
  refuse to ship a partially-overridden set — not the sampling.

## Future work

Ranked by value, with the evidence that motivates each.

1. **Deterministic CSS/XPath schema extraction** (crawl4ai). Removes the LLM
   key from the structured-extraction critical path. Both engines are already
   in-tree.
2. **Per-node content pruning as a ladder rung** (crawl4ai). Targets the
   mega-menu-in-`onlyMainContent` failure crw's own source documents. Must be
   benchmark-gated.
3. **Concurrent crawl dispatch.** `run_crawl` builds a semaphore, acquires a
   permit, then awaits the fetch inline in the same iteration — nothing is
   spawned, so real concurrency is 1 and `max_concurrency` is inert. The
   identical bug was already found and fixed in the `/map` path, with the
   reasoning recorded in-file; the crawl loop never got the same treatment.
   This is the prerequisite for adaptive throttling being worth anything.
4. **An overall crawl deadline.** `run_crawl` has a per-page deadline and no
   wall-clock budget, so a 1000-page crawl is bounded only by 1000 × per-page.
   `discover_urls` already models the budgeted shape to copy.
5. **Proxy health scoring** (Crawlee). Above.
6. **Adaptive per-host pacing** (Crawlee + Scrapling). Latency-targeted
   convergence on top of the `Retry-After` handling now in place.
7. **Citation-style links as an output format** (crawl4ai). On link-dense
   pages inline hrefs are a large fraction of tokens and an agent rarely needs
   the href inline. Self-contained and no risk to recall.
8. **A coded, actionable error taxonomy** (Firecrawl). crw emits generic codes
   (`renderer_error`, `http_error`) on both its native and Firecrawl-compatible
   surfaces, so a client cannot branch on failure. crw already has the
   structured knowledge internally; it is flattened to a string at the
   boundary.
9. **Honor or explicitly reject `maxAge`/`storeInCache`** on the
   Firecrawl-compatible surface. They are currently accepted and silently
   ignored.

## Benchmark methodology

No benchmark numbers are recorded in this document, because none were produced
by the work it accompanies. The changes made alongside it are correctness
fixes, and none of them carries a performance claim.

This is deliberate: crw's own rules make scrape success a hard pre-merge gate,
and a performance or recall claim without a measured before/after against the
frozen corpus is not admissible. The future-work items that would move those
numbers (items 1–3 above) are recorded as needing that evidence, not as ready
to land.
