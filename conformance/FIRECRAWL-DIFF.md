# fastCRW `/v2` ↔ Firecrawl v2 — behaviour diff

**Scope:** the `/v2/*` Firecrawl-compat surface. Our native `/v1` is unchanged.
**Closes the action item in `COMPATIBILITY-firecrawl.md` §3** ("add concrete
field-by-field diff for `metadata` and error envelope before any page claims
drop-in").

Everything below marked **captured** was observed by driving
`api.firecrawl.dev/v2/scrape` at `mock.fastcrw.com` — the same fixture URLs the
parity suite drives crw at. Run `./run.sh capture mock` to refresh; the
fixtures live in `fixtures/firecrawl_v2/mock_*.json`.

---

## 1. The rule

Firecrawl decides success in `scrapeURL/index.ts`:

```js
const isGoodStatusCode = (statusCode >= 200 && statusCode < 300) || statusCode === 304;
const hasRequiredOutput = isParsedImage || isLongEnough || !isGoodStatusCode;
if (hasRequiredOutput) return engineResult;
throw new EngineUnsuccessfulError(engine);
```

`!isGoodStatusCode` is an **OR term**, so a bad status is *sufficient* to accept
the result — even with an empty body. `controllers/v2/scrape.ts` then never reads
`metadata.statusCode` at all; it falls through to
`res.status(200).json({success: true, data: doc})`.

> **In Firecrawl, `success:false` means _its own infrastructure_ failed — DNS,
> timeout, every engine dead, a policy block. It never means "the target said
> 404."** The status lives in `data.metadata.statusCode` and the reason phrase in
> `data.metadata.error`.

Captured, for every one of 401/403/404/429/500/503:

```text
HTTP 200  {"success":true,"data":{"metadata":{
            "statusCode":404,"error":"Not Found", ...}}}
```

Two consequences that drove the fix:

1. Our old gate (`ScrapeData::http_error`) was **size-dependent** — it only fires
   under `ERROR_PAGE_MAX_TEXT` (200 bytes) — so the same site returned
   `success:false` for a terse 404 and `success:true` for a chatty one.
2. An empty body is only a failure when the status was *good*, because
   `isLongEnough` and `!isGoodStatusCode` are alternatives. **404 + empty body =
   success. 200 + empty body = failure.**

---

## 2. What changed

| # | Divergence | Now |
|---|---|---|
| 1 | 4xx/5xx target → `success:false` | `success:true`, `metadata.statusCode`, `metadata.error` = reason phrase |
| 2 | `metadata.url` aliased to `sourceURL` — the post-redirect URL was invisible | `metadata.url` = final URL (`PageMetadata.final_url`, stamped from the `FetchResult.final_url` that already existed) |
| 3 | no `metadata.error` | present on any status ≥ 400, Firecrawl's exact bare reason phrase |
| 4 | error envelope used `errorCode` only; both SDKs read `code` | emits **both** — `code` in Firecrawl's taxonomy, `errorCode` kept so the SaaS `RequestLog` still resolves |
| 5 | timeout 504, binary 422 | 408 / 500, per `controllers/v2/scrape.ts` |
| 6 | 200-with-empty-body → 200 `{success:false}` | 500 `SCRAPE_ALL_ENGINES_FAILED` |

Implementation is `routes/v2/error.rs` (new) plus `v2_verdict` in
`routes/v2/scrape.rs`. `/v1` is untouched: `http_error()` is still what the
native surface, crawl and batch consult — only this compat surface stopped
asking.

### Parity result

```text
before:  7/23 match Firecrawl v2
after:  21/21 match Firecrawl v2      (4 rows recorded, not asserted — §4)
```

---

## 3. The deliberate deviations

**A vendor wall served with HTTP 200 still fails for us; Firecrawl returns it.**

A Cloudflare "Just a moment..." shell has text, so `isLongEnough` is true and
Firecrawl hands the interstitial back as the page's content. We have already paid
for that behaviour — see `is_cdn_origin_error` in `crw-crawl`:

> `sacg.me` behind a dead origin was returned as `success: true` with "The
> initial connection between Cloudflare's network and the origin web server timed
> out" as its markdown, billed, and counted as a completed crawl page — for one
> customer, on the same source, since June.

Copying it back would re-open a closed, customer-visible bug. On a 4xx/5xx the
wall does *not* fail — the status already explains the page, which is the case
this whole change is about. Pinned by
`v2_verdict::vendor_wall_on_a_200_still_fails` and its `_on_a_403_` sibling.

**A wall no renderer tier could clear fails at any status; Firecrawl bills it.**

When the HTTP body and every browser tier the ladder tried all come back as an
anti-bot wall, Firecrawl's `!isGoodStatusCode` term accepts the 403 shell as the
page and bills it as a `success: true` document. We answer HTTP 200
`{success: false, data, error}` with the body cleared, whatever status the origin
sent. That class used to reach the caller as a 500 `SCRAPE_ALL_ENGINES_FAILED`,
which was never billed, and following Firecrawl here would start charging for a
page we could not get past. Only a wall the ladder itself condemned qualifies:
`classify_block`'s own verdict on a 4xx page that never went through the ladder
is still a document, as above. Pinned by
`v2_verdict::unclearable_wall_on_a_403_is_blocked_not_a_document` and
`classify_generic_block_on_a_403_without_the_flag_is_still_a_document`.

---

## 4. Rows recorded but not matched

`mock_parity.py` prints these as `[note]` rather than asserting them.
Matching Firecrawl's *contract* does not mean copying its extraction failures.

| Fixture | Firecrawl (captured) | crw |
|---|---|---|
| `/csv` — a 3-line valid CSV | **500** `SCRAPE_RETRY_LIMIT (document_antibot)` | parsed, 200 |
| `/html/malformed` — real prose behind unclosed tags | **500** `SCRAPE_ALL_ENGINES_FAILED` | parsed, 200 |
| `/redirect/loop` | never answered; held the connection past a 120s read timeout | 500 `SCRAPE_ALL_ENGINES_FAILED`, promptly |
| `/wall/captcha` | 408 `CONCURRENCY_QUEUE_TIMEOUT`, a capacity artifact of their side, not a verdict | 200, `success:true`, statusCode 403 on an engine with no browser tier; 200 `success:false` once a browser ladder ran and could not clear it (section 3) |

The `/wall/captcha` row needs a clean re-capture. Its 401 and 429 siblings both
captured cleanly and both confirm the status-code rule, so nothing here is
load-bearing.

---

## 4b. Unreachable targets — we cannot say what Firecrawl says

Firecrawl splits a case we merge. Both captured live:

| Target | Firecrawl |
|---|---|
| a hostname that does not resolve | **HTTP 200** `SCRAPE_DNS_RESOLUTION_ERROR` |
| a port that refuses the connection | **HTTP 500** `SCRAPE_SITE_ERROR` (`ERR_TUNNEL_CONNECTION_FAILED`) |

`CrwError::TargetUnreachable` is raised from `reqwest::Error::is_connect()`
(`http_only.rs:909`), which is true for **both**, and the message is
`"error sending request"` either way. There is nothing to branch on.

So this surface answers neither: it keeps its existing **422** and reports
`code: SCRAPE_SITE_ERROR`, the "URL failed to load" family, which is true in
both cases. Claiming the DNS code would be wrong whenever the real cause was a
refused connection, and answering HTTP 200 for a dead port tells the caller
"fine" about a request that failed — the one outcome that actively misleads.

Closing it means distinguishing resolver failures in the renderer's error chain
(`reqwest` exposes no `is_dns()`; it needs a walk of `Error::source()`). That is
a fetch-path change touching `/v1` too, so it is deliberately not in this PR.
Pinned meanwhile by `unreachable_target_does_not_claim_firecrawls_dns_answer`,
which asserts we never answer 200 here.

## 5b. `/v2/scrape` and `/v2/crawl` disagree about the same URL

This change did not touch `state.rs`, the shared crawl/batch path, which still
converts `http_error()` into a `block` and counts it in `blocked`. Measured
against a local stack, same URL, same surface:

```text
/v2/scrape  /status/404  ->  success:true,  1 credit,  HTTP tier only
/v2/crawl   /status/404  ->  blocked:1,     0 credits, escalated chrome + lightpanda
```

The crawl document's own warning: `chrome returned HTTP 404 (treated as
blocked); lightpanda returned an anti-bot block (structural_failure: ...)`.

That is the split the engine's own comment warns about — *"a URL is not billed on
one surface and refunded on the other"* — one level down. The SaaS reads it via
`page-billing.ts` (`billableCompleted(completed, blocked)`), so it is real money.

`state.rs` is shared with `/v1/crawl`, which is why it was left out of a
v2-scoped change. It should be the next thing fixed.

## 5. Still open

- **`/v2/crawl` + `/v2/batch/scrape` status envelope.** We omit `createdAt`,
  `completedAt`, `duration` and the robots/thin-crawl `warning` that
  `crawl-status.ts` emits. Our capture predates them, so recapture before
  chasing. We also carry an extra `blocked` counter — intentional, the SaaS
  bills off it.
- **Invalid crawl job id.** Firecrawl answers `400 {"success":false,"error":
  "Invalid job ID"}`; we hit axum's `Path<Uuid>` rejection and return plain text,
  which no SDK can parse.
- **`/v2/map`, `/v2/search`, `/v2/extract` error paths** — undiffed on both
  sides. Happy paths match.
- **Thin pages: a live parity gap.** Captured against a local stack WITH
  browsers: `/js/csr`, `/js/hydrate` and `/js/fetch` all come back from Firecrawl
  as `success:true` with the fixture sentinel in the markdown (43, 23 and 17
  characters). We answer **500 `SCRAPE_ALL_ENGINES_FAILED`** — chrome renders
  them correctly and our `structural_failure` classifier then discards the render
  as `minimal_text on small page`.

  Firecrawl's bar is `checkMarkdown.trim().length > 0`; ours is a heuristic built
  to catch JS shells and challenge pages. Loosening it is exactly how a
  Cloudflare interstitial gets billed as content, so this is a trade rather than
  an oversight — but a legitimately short page fails here and succeeds there, and
  the whole `/js/*` group is affected. Recorded as `[note]` rows.

- **The escalation ladder burns the deadline on empty pages.** With browsers
  configured, `/html/empty` answers `408 SCRAPE_TIMEOUT` rather than
  `500 SCRAPE_ALL_ENGINES_FAILED`: a 200 with no body escalates through chrome
  (12s nav budget) and lightpanda before anything concludes. Same end state as
  Firecrawl, reached far more expensively. The same shape shows on
  `/status/404` inside a crawl — see §5b.
- **The `/lie/*` group** (3 fixtures) has no expectation on either side.
