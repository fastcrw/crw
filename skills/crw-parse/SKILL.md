---
name: crw-parse
description: |
  Parse a local or remote FILE (PDF) into markdown or structured JSON with
  fastCRW. Use when the source is a file on disk — "parse this PDF", "extract
  text from this document", "read this report", "convert PDF to markdown".
  Routing rule: URL → use crw-scrape; file on disk → use crw-parse. Step 5
  of the crw workflow ladder.
license: AGPL-3.0
metadata:
  author: us
  version: "0.3.0"
  homepage: https://fastcrw.com
  repository: https://github.com/fastcrw/crw
allowed-tools: Bash(crw:*) Bash(curl:*) Read
---

# crw-parse — local file extraction

## When to use

- The source is a **file on disk** (PDF), not a web page.
- Step 5 in the [crw ladder](../crw/SKILL.md). If you have a URL, use
  [crw-scrape](../crw-scrape/SKILL.md) (step 2) instead — scrape handles remote
  PDFs via URL. If you want a typed JSON object from a page, see
  [crw-extract](../crw-extract/SKILL.md) (step 6).
- PDF only. DOCX, XLSX, and other office formats are **not yet supported**
  (unlike Firecrawl's document endpoint). If you have a non-PDF document,
  convert it to PDF first or use an external tool.

## Quick start

**CLI** — `crw scrape` auto-detects a local file path and routes to the PDF
parser; there is no separate `crw parse` subcommand:

```bash
crw scrape report.pdf                         # → markdown to stdout
crw scrape report.pdf --format json --extract '{"type":"object","properties":{"title":{"type":"string"}}}' -o out.json
```

**MCP** (inside an agent harness):

```
crw_parse_file(
  contentBase64="<base64-encoded PDF bytes>",
  filename="report.pdf",
  formats=["markdown"],
  maxLength=0
  # For structured JSON output:
  # formats=["json"],
  # jsonSchema={"type":"object","properties":{"title":{"type":"string"}}}
)
```

**REST** — multipart upload, 50 MB limit, PDF only:

```bash
curl -X POST "$CRW_API_URL/v2/parse" \
  -H "Authorization: Bearer $CRW_API_KEY" \
  -F "file=@report.pdf" \
  -F 'options={"formats":["markdown"]}'
```

## Options

| Need | CLI (`crw scrape <path>`) | MCP field | REST `options` field |
|------|--------------------------|-----------|----------------------|
| Output format | `--format markdown\|json\|text\|links` | `formats` | `formats` |
| Structured JSON | `--extract '<schema>'` | `jsonSchema` + `formats:["json"]` | `jsonSchema` + `formats:["json"]` |
| AI summary | `--summary` | `formats:["summary"]` | `formats:["summary"]` |
| Summary prompt | `--prompt "TEXT"` | — | `summaryPrompt` |
| Limit output chars | — | `maxLength` (0 = unbounded) | `maxContentChars` |
| Force parser | — | `parsers:["pdf"]` | `parsers:["pdf"]` |

Formats `json` and `summary` require a server-side LLM configured in
`[extraction.llm]` of the server config (or via `crw setup` for the CLI).

## Honest gaps

- **PDF only.** The server rejects anything without a `%PDF-` magic header.
- **No OCR.** Scanned/image-only PDFs have no extractable text layer; they
  return empty markdown with a warning. There is no `attempt_scanned` option —
  scanned PDFs are a known gap.
- **50 MB cap** on REST uploads (per-route hard limit). The CLI passes bytes
  in-process, so it shares the same underlying limit.
- **LLM required for `json`/`summary`.** Without a configured LLM the request
  returns a 400.

## Tips

- **Read the result, don't stream it.** For large PDFs, write to `.crw/` and
  `grep`/`head` the output: `crw scrape big.pdf -o .crw/big.md`.
- **MCP requires base64.** Read the file in your agent, base64-encode the bytes,
  pass as `contentBase64`. The `filename` field is optional but helps with
  error messages.
- **Scanned PDFs return empty markdown — no warning field.** If the PDF has no
  extractable text layer, the REST response returns empty markdown with no
  `warning` field in the envelope. A warning (e.g. `warning: pdf_partial_text`)
  only appears on the CLI's stderr, never in the REST/MCP response. If you get
  empty markdown, assume a scanned/image-only PDF and handle it at call-site.

## See also

- [crw-scrape](../crw-scrape/SKILL.md) — fetch a URL (including a remote PDF
  served over HTTP)
- [crw-extract](../crw-extract/SKILL.md) — typed JSON object from a page against
  a schema
- [crw](../crw/SKILL.md) — ladder overview and routing rules
