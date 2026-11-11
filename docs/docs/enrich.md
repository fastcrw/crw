<div class="page-intro">
  <div class="page-kicker">More API</div>
  <h1>Enrich</h1>
  <p class="page-subtitle">Fill the empty cells of a table from the web. Send rows and the columns you want filled; every cell comes back with a value, a status and the page it was read from.</p>
  <div class="page-capabilities">
    <div class="page-capability"><strong>Best for:</strong> lead lists, company tables, catalog gaps</div>
    <div class="page-capability"><strong>Route:</strong> <code>/v1/enrich</code> (job on the hosted API)</div>
    <div class="page-capability"><strong>Start with:</strong> a website column</div>
  </div>
</div>

## How it works

For each row, fastCRW:

1. Reads the row's own website first, when the row has a URL or a domain in any value.
2. Searches once for whatever is still missing, using the row's other values and your `context`.
3. Extracts the missing columns from the top results and merges them in rank order.

A cell is only filled from a page that states it. Each filled cell names that page and, when the value appears in it word for word, quotes the sentence it came from.

## Hosted API

On `api.fastcrw.com`, enrich runs as a job, so a table can be as large as your plan's job limit:

```http
POST https://api.fastcrw.com/v1/enrich
Authorization: Bearer YOUR_API_KEY
```

The body is the request described below, plus an optional `budget` (`maxCredits`, `maxDurationMs`). The answer is `202`:

```json
{ "success": true, "id": "cm3...", "status": "accepted", "expiresAt": "2026-11-08T10:00:00.000Z",
  "budget": { "maxCredits": 222, "maxDurationMs": 600000, "maxSubrequests": 120 } }
```

Poll `GET /v1/enrich/{id}`. It returns every row in request order: rows waiting their turn are `"status": "pending"` and rows being filled right now are `"running"`, both with empty `cells`; finished rows are the row objects described below plus `creditsUsed`.

```json
{
  "success": true,
  "id": "cm3...",
  "status": "running",
  "budget": { "maxCredits": 222, "creditsUsed": 18, "maxDurationMs": 600000, "maxSubrequests": 120 },
  "data": { "rows": [ { "index": 0, "rowId": "acme-1", "status": "completed", "cells": { }, "creditsUsed": 18 },
                      { "index": 1, "rowId": "acme-2", "status": "pending", "cells": {} } ] }
}
```

| Job `status` | Meaning |
|---|---|
| `accepted`, `running` | Rows are being filled. |
| `completed` | Every row has a result. |
| `partial` | The job stopped early (`stopReason`: `budget_exhausted`, `deadline_exceeded` or `subrequest_limit`). Rows still `pending` were not run: send them as a new job, matched back by `rowId`. |
| `cancelled` | Stopped by `DELETE /v1/enrich/{id}`. Rows that finished keep their results. |

Billing follows the usual rules: each page read costs what a scrape of it costs, each search what a search costs, plus the model usage for that row. A row that fails is not billed. The default budget covers the most the rows could cost; pass a smaller `maxCredits` to cap a job, and expect `partial` when it runs out. Enrich is available on paid plans; the hosted API runs it on its managed model, so `llmApiKey` and related fields are not accepted there.

The hosted MCP server exposes it as `crw_enrich` for up to 10 rows: it returns the table when it is done, or the rows so far plus the job `id` to call again with.

## Request

```http
POST /v1/enrich
```

```json
{
  "rows": [
    { "id": "acme-1", "company": "Stripe", "website": "stripe.com", "ceo": null },
    { "id": "acme-2", "company": "Linear", "website": "linear.app", "ceo": "" }
  ],
  "columns": [
    { "name": "ceo", "description": "Current CEO, full name", "type": "text" },
    { "name": "employees", "description": "Approximate employee count", "type": "number" },
    { "name": "stage", "description": "Latest funding stage", "type": "options",
      "options": ["Seed", "Series A", "Series B", "Later", "Public"] }
  ],
  "context": "B2B software companies"
}
```

| Field | Notes |
|---|---|
| `rows` | Flat objects: at most 3 per call on a self-hosted server, up to your plan's job limit on the hosted API. Values that are not target columns describe the row's entity. An `id` (string or number) is echoed back as `rowId`. |
| `columns` | At most 10. `name` is the row key to fill, `description` says what belongs in it. |
| `columns[].type` | `text` (default), `number`, `url` or `options`. For `options`, a value that matches none of the labels comes back `notFound`. Dates are `text`, as written on the page. |
| `context` | Optional. One line about the table, used to tell apart entities with the same name. |
| `overwrite` | Optional, default `false`. A cell is filled only when its value is missing, `null` or `""`; `true` refills every listed column. |

## Response

```json
{
  "success": true,
  "status": "completed",
  "data": {
    "rows": [
      {
        "index": 0,
        "rowId": "acme-1",
        "status": "completed",
        "cells": {
          "ceo": {
            "value": "Patrick Collison",
            "status": "supported",
            "confidence": "high",
            "citations": [
              { "url": "https://stripe.com/about", "excerpt": "Patrick Collison, CEO" }
            ]
          },
          "employees": { "value": null, "status": "notFound", "citations": [] }
        },
        "usage": { "pages": ["https://stripe.com/about"], "searches": 1 }
      }
    ]
  }
}
```

Rows come back in request order. Keep only what you trust:

```js
const filled = row => Object.fromEntries(
  Object.entries(row.cells)
    .filter(([, cell]) => cell.status === "supported")
    .map(([name, cell]) => [name, cell.value])
);
```

### Cell status

| `status` | Meaning |
|---|---|
| `supported` | The value appears word for word in the quoted `excerpt` on the cited page. |
| `unverified` | The cited page was read and gave this value, but no exact sentence could be matched. Check it before you rely on it. |
| `notFound` | No page that was read stated this value. `value` is `null`. |

A target column that is missing from `cells` was never reached, usually because the row ran out of time. That is different from `notFound`. A row with no blank target cells comes back `completed` with empty `cells`.

Cells may carry extra evidence fields (`basisVersion`, `field`, `sourceHash`, `sourceTextKind`). `usage.llmUsage` reports the extraction token usage. Both are informational and may change.

### Row status

| `status` | Meaning |
|---|---|
| `completed` | Every target column was looked for. |
| `incomplete` | The row ran out of time (`stopReason: "timeout"`). Cells it did not reach are absent. Send the row again. |
| `failed` | No page could be read for the row (`error` says why; `stopReason: "timeout"` when time ran out first). Send it again, or add a website value. |

## Larger tables on a self-hosted server

On the hosted API, send the whole table as one job. A self-hosted server answers synchronously and takes 3 rows per call, so chunk bigger tables and join the results back by `rowId`:

```js
const results = [];
for (let i = 0; i < rows.length; i += 3) {
  const res = await fetch(`${BASE}/v1/enrich`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: `Bearer ${KEY}` },
    body: JSON.stringify({ rows: rows.slice(i, i + 3), columns, context }),
  }).then(r => r.json());
  results.push(...res.data.rows);
}
```

## Tips

- Include a website column. The row's own site is read first, and it is the strongest way to keep a common name from matching the wrong company.
- Describe each column the way the fact is written on a page ("Current CEO, full name"), not as a database field.
- Ask for facts pages state. Inferred values ("is this company a good fit?") come back `notFound`.

## Self-hosting

`/v1/enrich` needs an LLM (`[extraction.llm]`, or `llmApiKey` per request) and works best with a search backend configured. Without search, only rows that carry their own URL get cells. `GET /v1/capabilities` reports both under `enrich`.

The MCP server exposes the same call as the `crw_enrich` tool.
