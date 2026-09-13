# Errors and Warnings

## Response semantics

- `success: true` means the pipeline produced usable page content.
- `success: false` means the request failed -- either an engine error or the target returned an error status (4xx/5xx) with minimal content.
- `error` is present when `success: false`. It describes what went wrong.
- `warning` flags degraded target outcomes (problematic status codes, a partial render) when `success: true` -- meaning content was produced but may be incomplete. An anti-bot wall is not one of these: it is always `success: false` with `error_code: "anti_bot"`.
- `metadata.statusCode` is the target site's HTTP status.
- `data` may still be present when `success: false` if partial content was extracted.

The important distinction is that there are two layers of status:

- the HTTP status returned by CRW,
- and the target site's own status exposed through `metadata.statusCode`.

You need both to debug real scraping failures.

## HTTP status codes returned by the API

| Status | Meaning |
| --- | --- |
| 200 | Success |
| 400 | Invalid request parameters (bad URL, invalid JSON body, invalid selector) |
| 401 | Invalid or missing API key |
| 404 | Endpoint not found |
| 422 | Validation failed (unknown format, invalid schema, extraction error), or the origin returned a body that is not a page (`unsupported_content_type`) |
| 429 | Rate limit or credit quota exceeded |
| 502 | Engine internal error |
| 503 | Server at capacity |
| 504 | Request timed out |

## How To Read Common Cases

| Situation | What it usually means | What to do next |
| --- | --- | --- |
| HTTP `200` with `warning` | The request succeeded, but the target result is degraded | Inspect `warning` and `metadata.statusCode` |
| HTTP `400` | Your request body is invalid | Fix fields, selectors, or schema |
| HTTP `422` | The request shape is valid JSON but semantically invalid | Check format names, schema, or extraction config |
| `unsupported_content_type` | The origin served an office document, image or archive rather than a page | Fetch the file yourself, or point the request at a PDF or an HTML page |
| HTTP `429` | Rate limit or credit ceiling hit | Back off and honor `Retry-After` |
| HTTP `502` / `504` | Upstream or timeout issue | Retry with backoff |

## Common engine errors

| Error | When it happens |
| --- | --- |
| Invalid URL | URL is malformed or targets a blocked address |
| Invalid selector | CSS selector or XPath expression cannot be parsed |
| Renderer timeout | JS rendering exceeded the page timeout |
| Navigation failed | CDP browser could not load the page |
| Response too large | Page exceeded the maximum allowed size |
| Invalid JSON schema | Schema provided for extraction is malformed |
| Extraction failure | LLM extraction failed (no LLM configured, or LLM returned an error) |
| `summary` format requires LLM config | `formats: ["summary"]` sent without a server-side `[extraction.llm]` block and without per-request `llmApiKey` |
| Key header required | Server is in key-required mode (`require_byok_header`) and the request lacks both the header and `llmApiKey` |
| No JS renderer available | `renderJs: true` but no CDP browser is configured |

## Common warnings

| Warning | When it appears |
| --- | --- |
| `Target returned 403 Forbidden` | Target site blocked the request |
| `Target returned 429 Too Many Requests` | Target site rate limited the request |
| `Blocked by anti-bot protection` | Page contains Cloudflare/captcha markers |
| `JS rendering was requested but no renderer is available` | Fallback to HTTP-only fetch |
| `content truncated to N bytes before summarization` | Scraped content exceeded `maxContentChars` (or `[extraction.llm].max_html_bytes`); the head of the page was summarized |
| `summary failed: <reason>` | A per-result summary in `/v1/search` failed (e.g. provider 429). Other results are still returned |
| `answer synthesis failed: <reason>` | The answer call in `/v1/search` failed. `answer` is `null` but `summaries` and `results` are still returned |
| `N of M per-result summaries failed` | Some but not all per-result summaries failed in `/v1/search` |

## Retry Guidance

Retrying helps only for some classes of failure:

- retry `429`, `502`, and `504` with backoff,
- do not blindly retry `400` or `422`,
- and treat repeated warnings from the same domain as a target compatibility problem, not a random transient issue.

If a page is repeatedly blocked by anti-bot protection, longer retry loops usually make the situation worse rather than better.
