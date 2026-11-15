# ADR 0001: Basis wire format (per-field extraction evidence)

- Status: accepted
- Applies to: `crw-core::evidence`, `crw-extract::{structured, basis}`,
  `crw-server` extract/scrape routes, `/v1/capabilities`.
- Wire version: `basisVersion = 1` (`crw_core::evidence::BASIS_VERSION`).

## Context

Structured extraction (`formats:["json"]` on scrape, and `/v1/extract`) returns
a schema-valid object, but the caller has no way to know *where* each value came
from or whether the model grounded it in the page at all. Downstream agent
workflows need a machine-checkable answer to "can I trust this field?" without a
second, human, verification pass.

The requirement is honesty, not completeness: a field that cannot be verified
must say so. The system must never emit a fabricated attribution.

## Decision

### 1. One evidence type, extended in place

`crw-core::evidence` already shipped `Basis` and `EvidenceCitation`, reserved
for this and with zero consumers. We extend those types rather than introduce a
competing one. `value` becomes `Option` (so `notFound` is representable),
`status: FieldStatus` is added, `confidence` stays the qualitative
`low|medium|high` vocabulary shared with change-tracking, and `reasoning`
becomes optional and is not requested in v1.

### 2. Status is the honesty contract

`FieldStatus = supported | unverified | unsupported | notFound` (camelCase on
the wire). The invariants are testable and enforced:

| status | `value` | `citations` | `citation.excerpt` |
| --- | --- | --- | --- |
| `supported` | non-null | exactly 1 | present, verbatim in the source, and it carries the value |
| `unverified` | non-null | exactly 1 | `None` |
| `unsupported` | non-null | empty | n/a |
| `notFound` | `null` | empty | n/a |

- `citations` is empty **iff** status is `unsupported` or `notFound`.
- An excerpt is present **only** on `supported`.
- `unverified`: the document is real and attributable, the span within it is
  not established. `unsupported`: no attribution survives; the value is kept but
  explicitly untrusted.

### 3. Basis is produced in the same tool call; the server hashes the source

The extraction LLM call is extended to return, per top-level scalar leaf,
`{ value, sourceUrl, excerpt, confidence }` inside the same tool call. A second
verifier call is rejected: it doubles the LLM leg for no deterministic gain, and
the deterministic half of verification is done server-side for free.

The `sourceHash` is **not** model-produced. The server stamps it.

Canonical source text = the normalized, cleaned, truncated markdown actually
sent to the model (the exact bytes after the clean/markdown pipeline and after
`truncate_md`). It is hashed **before** the call:

```
EvidenceCitation.sourceHash     = "sha256:" + hex(sha256(canonical_source_text))
EvidenceCitation.sourceTextKind = "llmInput"
```

This is deliberately **not** the same hash as `ScrapeData.sourceHash`
(`crw_diff::snapshot::hash_markdown`, bare hex over the *full* normalized
markdown). `sourceTextKind` is the disambiguator. Both hashes are surfaced per
document: `ScrapeData.sourceHash` (full markdown, for change-tracking and
re-scrape correlation) and `llmInputHash` (the truncated `llmInput` text,
matching every citation's `sourceHash`).

Multi-source extraction (`/v1/enrich`, 2026-11): one model call reads several
documents, each fenced and headed by its URL. Each document has its own
canonical text and hash (`sha256` of that document's text alone, never the
framed prompt), and the model names the document per field. Alignment runs on
that document's own text only, so headers and fence lines can never be
evidence. A search result's summary used as a source carries
`sourceTextKind = "searchSnippet"`; `llmInputHash` is absent on such a call,
since there is one hash per document, on each citation.

### 4. Alignment: null, unsupported and hallucinated fields (server-side)

Schema validation runs first, unchanged; `data` that survives it is
authoritative and is never rewritten. Basis-value alignment then runs
deterministically (`crw_extract::basis::align_basis`):

1. **Document check** — `citation.url` must match the document the server
   fetched, and the recorded hash must match. A URL the model invented is not in
   the set. Fail → citation dropped → `unsupported`.
2. **Value check** — the model's claimed `value` must equal `data[field]`. Fail
   → citation dropped → `unsupported` (the model contradicted itself; `data`
   wins and is never rewritten).
3. **Excerpt containment** — the excerpt must be a verbatim substring of the
   canonical source text (whitespace-collapsed on both sides so a reflow is not
   punished). Fail → `excerpt = None` → `unverified`.
4. **Value-carried** — the excerpt must actually carry the value (not a trivial
   one-character substring). Fail → `unverified`.

A leaf with no basis → `unsupported`. A null leaf → `notFound`.

**A basis/value mismatch fails the test suite.** `status: "supported"` with an
empty citation list, a hash the server did not record, or an excerpt not in the
source, is a hard test failure. This is what makes the contract load-bearing
rather than decorative.

### 5. Capabilities

`Capabilities.extract.perFieldAttribution` is flipped from the hardcoded `false`
to report the truth of the running build. A client that gates its evidence UI on
this flag must not be told `false` by a binary that honours the flag.

## Scope (v1)

**In:** top-level scalar properties (string, number, integer, boolean); 0 or 1
citations per leaf; substring-containment excerpt verification with no offsets
stored.

**Out** (`charStart`/`charEnd` stay `None`): character offsets and offset-based
re-verification, RFC 6901 JSON Pointer leaf paths, nested/array/repeated field
semantics (non-scalar properties carry no evidence entry), multi-citation
leaves, and an extraction-provenance benchmark.

## Honest limits of what these checks prove

- Containment proves the excerpt is *on the page*, not that it is the *right*
  span for the value. The value-carried check narrows this but cannot prove
  semantic relevance for every type.
- Booleans are exempt from the value-carried check: a literal `true` essentially
  never appears in prose ("In stock" grounds `true`), so demanding it be quoted
  would make `supported` unreachable for every boolean. A boolean still passes
  the document, value-equality and excerpt-in-source checks. Disclosed limit.
- The model can still emit a real excerpt that happens to contain the value by
  coincidence. The layer catches fabrication (invented URL, wrong hash, absent
  excerpt), not every possible mis-grounding.

## Consequences

- Ships to self-hosters too: the engine changes are additive and deliberate.
- The request bytes on the non-basis path are byte-for-byte unchanged, so every
  existing caller stays on exactly the path they are on today.
- SDK (`sdks/typescript`), MCP tool definitions (`crw-mcp-proto`) and the
  OpenAPI spec carry the `basis` request flag and the evidence response types.
