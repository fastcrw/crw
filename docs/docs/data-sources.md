<div class="page-intro">
  <div class="page-kicker">Integrations</div>
  <h1>Data Sources</h1>
  <p class="page-subtitle">Structured data from named public APIs (papers, packages, registries, music metadata) through two MCP tools. Most sources run on fastCRW; some run on your own machine, with your own key, under the source's own terms.</p>
  <div class="page-capabilities">
    <div class="page-capability"><strong>Tools:</strong> <code>crw_find_data</code>, <code>crw_get_data</code></div>
    <div class="page-capability"><strong>Needs:</strong> cloud mode (<code>CRW_API_KEY</code>)</div>
    <div class="page-capability"><strong>Keys:</strong> <code>crw connectors set-key</code></div>
  </div>
</div>

## What they are

In cloud mode, `crw-mcp` and `crw mcp` add two tools to the usual nine:

- `crw_find_data` searches the catalogue and returns, for each match, the `provider`, the `capability` and the options it takes (`requiredOptions`, `optionalOptions`).
- `crw_get_data` calls one of them with an `options` object and returns the source's JSON in `data`, with `provenance` (source name, the URL called, licence, attribution, time).

```json
{
  "provider": "openalex",
  "capability": "works",
  "creditsUsed": 0,
  "provenance": {
    "source": "OpenAlex",
    "upstreamUrl": "https://api.openalex.org/works/W2741809807",
    "licence": "CC0 1.0",
    "licenceUrl": null,
    "attributionText": null,
    "retrievedAt": "2026-10-29T12:00:00.000Z"
  },
  "data": { "id": "https://openalex.org/W2741809807" }
}
```

The agent calls both tools the same way whatever the source. Where a call runs is decided by crw:

| Source runs | How | Cost |
|---|---|---|
| On fastCRW | crw forwards the call to the fastCRW API, which calls the source | the item's `creditsCost` per call |
| On your machine | crw calls the source directly from your machine, with your key if it needs one | free (`creditsUsed: 0`); the data never passes through fastCRW |

Sources that run on your machine are ones whose terms let you call them for your own use. You call them under those terms, so read them before relying on a source; `provenance.licence` and `attributionText` say what applies to the data.

:::note
The hosted MCP endpoint (`https://fastcrw.com/mcp`, used by claude.ai on the web and ChatGPT) cannot run a source on your machine. It lists those sources with `requiresLocalInstall: true` and answers a call with `[client_execution_required]`. Install the local server to use them: `npx -y crw-mcp install` (Claude Code, Cursor, Codex, Claude Desktop).
:::

## Store your own key

Some sources that run on your machine need a free key from the source, some take one optionally for a higher rate limit, and some need none.

```bash
crw connectors list
```

```text
PROVIDER  NAME      KEY                LICENCE
openalex  OpenAlex  optional, not set  CC0 1.0
```

The `KEY` column reads `not needed`, `stored`, `optional, not set`, `required, missing`, or `stored, approve again` (see `connector_origin_not_approved` below).

```bash
crw connectors set-key openalex
```

`set-key` prints where the key will go (the source's origins and whether it travels as a query parameter, a header or HTTP Basic), asks you to approve that, then reads the key from a hidden prompt. It never takes the key as an argument, so it stays out of your shell history. In a script, pipe the key in and approve with `--yes`:

```bash
printf '%s' "$OPENALEX_KEY" | crw connectors set-key openalex --yes
```

The key is saved to `~/.config/crw/config.toml` (mode 0600), together with what you approved:

```toml
[connectors.openalex]
api_key = "..."
origins = ["https://api.openalex.org"]
auth = "query:api_key"
```

A key is only ever sent to the origins and in the placement stored next to it. If a source's definition later asks for something else, the call fails with `connector_origin_not_approved` and the key is not sent; run `set-key` again to review and approve the new destination. Agents pick up a stored key on their next call, no restart needed.

### Docker, CI and `$CRW_CONFIG`

A `[connectors]` table in a file passed with `$CRW_CONFIG` or `--config` is not read. Use environment variables instead, with the provider id upper-cased and `-` written as `_`:

```bash
CRW_CONNECTORS__OPENALEX__API_KEY=...
CRW_CONNECTORS__OPENALEX__ORIGINS=https://api.openalex.org
CRW_CONNECTORS__OPENALEX__AUTH=query:api_key
```

`ORIGINS` is comma-separated. `AUTH` is required; a key without `ORIGINS` and `AUTH` is never sent. When `__API_KEY` is set, these variables replace the file entry for that source.

## How calls from your machine are made

- Source definitions are fetched from the fastCRW API and kept for at most an hour. If they cannot be refreshed after that, data tools answer `[data_sources_unavailable]` until a refresh succeeds; the other tools keep working.
- Only `https`, never a private, loopback or link-local address (checked on every DNS answer), at most 3 redirects and only within the same origin.
- 20 second timeout, 8 MiB response limit, one call at a time per source with the source's own spacing, and one retry when the source asks to wait 10 seconds or less.
- System HTTP proxies are not used for these calls.
- Your key is removed from the result, from `provenance.upstreamUrl` and from every error message.
- Results over 256 KiB are replaced with `[result_too_large]`; narrow the options and call again.

## Usage counts

After each call that runs on your machine, crw sends the fastCRW API one anonymous count: the provider, the capability, the outcome (`success`, `upstream_error`, `rate_limited`, `timeout`, `invalid_response` or `blocked`) and the time it took. It never sends the URL, your options, your key or the data. To turn it off:

```toml
[client]
telemetry = false
```

or set `CRW_CLIENT__TELEMETRY=false`.

## Errors

Errors come back as a tool error (`isError: true`) whose text starts with a code in brackets, followed by a sentence the agent can pass on.

| Code | Meaning | Fix |
|---|---|---|
| `data_sources_unavailable` | Source definitions could not be loaded (network, or an API without data sources) | Check the connection and `CRW_API_URL`; try again |
| `crw_too_old` | This crw does not understand the current source definitions | Upgrade: `npx -y crw-mcp@latest`, `brew upgrade crw` or `cargo install crw-cli`, then restart the MCP server |
| `invalid_options` | A missing or malformed option, or a bad `provider` / `capability` | Use the ids and options from `crw_find_data` |
| `unknown_data_source` | No such source or capability | Call `crw_find_data` |
| `connector_key_missing` | The source needs your own key | `crw connectors set-key <provider>` |
| `connector_origin_not_approved` | The stored key was approved for another origin or placement, or lacks one | `crw connectors set-key <provider>` to review and approve again |
| `connector_key_invalid` | The stored key has characters an HTTP header cannot carry | Store the key again |
| `connector_misconfigured` | The source definition is unusable right now | Try again later |
| `blocked` | A safety rule stopped the request before it left your machine (redirect to another origin, private address, too many redirects) | Nothing to fix locally; report it if it persists |
| `upstream_error` | The source answered with an error status or could not be reached | Check the options; try again |
| `upstream_timeout` | The source did not answer within 20 seconds | Try again |
| `upstream_rate_limited` | The source is limiting requests from your machine | Wait, or store a key if the source offers higher limits with one |
| `invalid_response` | The source answered with something other than JSON, or over 8 MiB | Try again; narrow the options |
| `result_too_large` | The result is over 256 KiB | Narrow the options |
| `client_execution_required` | Hosted MCP only: the source runs on your machine | `npx -y crw-mcp install` |
| `unauthorized` | Missing or invalid fastCRW API key | Check `CRW_API_KEY` |
| `insufficient_credits` | Out of credits for a source that runs on fastCRW | Top up or upgrade |
| `rate_limited` | Too many requests to the fastCRW API | Slow down |
| `server_unreachable`, `server_error` | The fastCRW API could not be reached or failed | Try again |
