---
name: crw-watch
description: |
  Detect what changed between two page snapshots with fastCRW — stateless diff
  as a REST primitive. Use when you need to track content changes, monitor a
  page for updates, or build a cron-based alert system: "has this page changed?",
  "alert me when pricing changes", "diff this week's scrape against last week's".
  Step 7 of the crw workflow ladder.
license: AGPL-3.0
metadata:
  author: us
  version: "0.3.0"
  homepage: https://fastcrw.com
  repository: https://github.com/fastcrw/crw
allowed-tools: Bash(crw:*) Bash(curl:*) Read
---

# crw-watch — change tracking and diffing

## When to use

- You want to know **what changed** between two snapshots of a page.
- Step 7 in the [crw ladder](../crw/SKILL.md). Assumes you can already scrape
  the page — see [crw-scrape](../crw-scrape/SKILL.md) (step 2).
- You want a self-hosted, stateless diff primitive you control. Firecrawl offers
  change tracking only as a managed cloud feature; crw exposes the same primitive
  as a REST endpoint that runs on **your own infra** — you own the snapshots,
  the cadence, and the data.

## Architecture: crw is stateless

crw stores nothing between calls. The caller owns the snapshots:

```
1. Scrape now        → store snapshot (markdown / json)
2. Scrape later      → call /v1/change-tracking/diff with current + previous
3. On status=changed → alert / act
4. Repeat on a cron
```

## Diff modes

Two modes, composable:

| Mode | Wire string | What it produces |
|------|-------------|-----------------|
| Git-style text diff | `"gitDiff"` (alias: `"git-diff"`) | Unified-diff text + parse-diff AST in `diff.text` / `diff.json` |
| Per-field JSON diff | `"json"` | Path-keyed map `{"$.field": {"previous":…,"current":…}}` in `diff.json`; requires `schema` |

Default (omit `modes`): `["gitDiff"]`. Combine both: `"modes": ["gitDiff", "json"]`.

## Quick start

**Single page diff** (REST):

```bash
curl -X POST "$CRW_API_URL/v1/change-tracking/diff" \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "current": {
      "markdown": "# Pricing\nPro plan: $49/mo"
    },
    "previous": {
      "markdown": "# Pricing\nPro plan: $39/mo",
      "contentHash": "<hash from prior result>"
    },
    "modes": ["gitDiff"]
  }'
```

Response shape:

```json
{
  "success": true,
  "data": {
    "status": "changed",
    "firstObservation": false,
    "contentHash": "<new hash>",
    "snapshot": { "markdown": "...", "contentHash": "..." },
    "diff": {
      "text": "@@ -1,2 +1,2 @@\n # Pricing\n-Pro plan: $39/mo\n+Pro plan: $49/mo",
      "json": { "files": [...] }
    }
  }
}
```

**Batch diff** (discriminated by presence of `batch` key):

```bash
curl -X POST "$CRW_API_URL/v1/change-tracking/diff" \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "batch": [
      { "url": "https://example.com/pricing", "current": {"markdown": "..."}, "previous": {"markdown": "..."} },
      { "url": "https://example.com/about",   "current": {"markdown": "..."} }
    ],
    "modes": ["gitDiff"]
  }'
```

Shared `modes`/`schema`/`prompt`/`contentType` at the top level are defaults;
each batch item can override them individually.

**Inline during a scrape** — pass `changeTracking` as a format on `/v1/scrape`:

```bash
curl -X POST "$CRW_API_URL/v1/scrape" \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://example.com/pricing",
    "formats": ["markdown", "changeTracking"],
    "changeTracking": {
      "modes": ["gitDiff"],
      "previous": { "markdown": "...", "contentHash": "..." }
    }
  }'
```

## Request fields

**Single mode:** `{ current, previous?, modes, schema?, prompt?, contentType?, tag?, goal?, judgeEnabled? }`

**Batch mode:** `{ batch: [...items], modes, schema?, ... }` where each item is
`{ url?, current, previous?, modes?, schema?, ... }`.

| Field | Type | Notes |
|-------|------|-------|
| `current.markdown` | string | Current page content (gitDiff / mixed) |
| `current.json` | object | Current extracted JSON (json / mixed) |
| `previous.markdown` | string | Prior snapshot for gitDiff |
| `previous.contentHash` | string | Persist from prior result's `snapshot.contentHash` |
| `modes` | string[] | `["gitDiff"]` (default), `["json"]`, or both |
| `schema` | JSON Schema | Required for `json` mode; defines tracked fields |
| `prompt` | string | Natural-language extraction prompt (alternative to `schema`) |
| `contentType` | string | If binary/non-text, triggers byte-hash comparison only |
| `tag` | string | Opaque caller ID echoed back on the result |
| `goal` | string | Natural-language filter for meaningful changes (AI judge, M2) |
| `judgeEnabled` | bool | Enable AI judgment (M2 feature; accepted but not yet applied) |

## The `goal` field (AI judge)

`goal` is a natural-language filter for what counts as a meaningful change, fed
to an LLM judge. It is accepted by the server now but applied in a future
milestone (M2). Guidance for when it lands:

- Be specific: `"Alert when the listed price changes; ignore copy rewrites and
  nav updates"` beats `"detect important changes"`.
- Narrow the scope: `"Only flag changes to the Features table, not the hero
  section"`.
- The judge returns `{meaningful, confidence, reason, meaningfulChanges[]}` in
  the result's `judgment` field.

## Cron pattern (self-hosted)

```bash
#!/usr/bin/env bash
# cron-check.sh — run every hour via cron or a scheduler
SNAPSHOT_FILE=".crw/snapshot.json"
CURRENT=$(crw scrape "https://example.com/pricing" --format markdown)

if [ -f "$SNAPSHOT_FILE" ]; then
  PREV_MARKDOWN=$(jq -r '.markdown' "$SNAPSHOT_FILE")
  PREV_HASH=$(jq -r '.contentHash' "$SNAPSHOT_FILE")
  RESULT=$(curl -s -X POST "$CRW_API_URL/v1/change-tracking/diff" \
    -H "Authorization: Bearer $CRW_API_KEY" \
    -H "Content-Type: application/json" \
    -d "{\"current\":{\"markdown\":$(jq -Rsc . <<<"$CURRENT")},\"previous\":{\"markdown\":$(jq -Rsc . <<<"$PREV_MARKDOWN"),\"contentHash\":\"$PREV_HASH\"},\"modes\":[\"gitDiff\"]}")
  STATUS=$(echo "$RESULT" | jq -r '.data.status')
  if [ "$STATUS" = "changed" ]; then
    echo "CHANGED: $(echo "$RESULT" | jq -r '.data.diff.text')"
    # → send alert, write to DB, trigger webhook, etc.
  fi
  echo "$RESULT" | jq '.data.snapshot' > "$SNAPSHOT_FILE"
else
  # First observation — store the snapshot
  curl -s -X POST "$CRW_API_URL/v1/change-tracking/diff" \
    -H "Authorization: Bearer $CRW_API_KEY" \
    -H "Content-Type: application/json" \
    -d "{\"current\":{\"markdown\":$(jq -Rsc . <<<"$CURRENT")},\"modes\":[\"gitDiff\"]}" \
    | jq '.data.snapshot' > "$SNAPSHOT_FILE"
fi
```

## Tips

- **Persist `snapshot` from each result** as the next call's `previous`. The
  `snapshot` field in the response contains the normalized content and
  `contentHash` — store it, don't recompute it.
- **`firstObservation: true`** means no `previous` was supplied. The server sets
  `status: "changed"` and returns `snapshot` but produces no diff. Store it as
  your baseline.
- **`json` mode needs `current.json` (+ optionally a schema).** Without structured
  input it produces no diff — use `gitDiff` mode for plain markdown.
- **Batch is more efficient at scale.** One HTTP round-trip for N pages instead
  of N calls. Top-level `modes`/`schema` as defaults keeps the body compact.
- **Data sovereignty.** You supply `previous`; crw computes and returns. Nothing
  is stored server-side. Your snapshots, your infra, your retention policy.

## See also

- [crw-scrape](../crw-scrape/SKILL.md) — get the current page content to feed
  into the diff
- [crw](../crw/SKILL.md) — ladder overview and routing rules
