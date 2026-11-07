# AGENTS.md

Root instructions for working in this repository. Read this first. Scoped
`AGENTS.md` files exist under `crates/crw-renderer/`, `docs/`, and `sdks/`
for what genuinely differs in those subtrees; they do not repeat this file.

## Two-minute system map

This is `crw`, a single Rust binary that scrapes, crawls, maps, searches and
extracts web content, plus a managed SaaS built on top of it. The Cargo
workspace has 11 crates, all versioned together at the same release number:

- `crw-core` - shared types, config, error taxonomy.
- `crw-renderer` - the fetch/render engine: HTTP-only fetch, escalation to
  headless browsers, anti-bot handling, proxy egress.
- `crw-extract` - HTML-to-markdown/structured extraction, block detection.
- `crw-crawl` - BFS crawl orchestration over the renderer.
- `crw-search` - search backend integration.
- `crw-diff` - change-tracking / diff between scrapes.
- `crw-server` - the Axum HTTP API. It is both a library (embedded by
  `crw-cli` for `crw serve`) and its own binary: `crates/crw-server/Cargo.toml`
  has no explicit `[[bin]]` section, but `src/main.rs` exists, so Cargo
  auto-discovers a `crw-server` binary target. That binary is the production
  container entrypoint (`Dockerfile` `CMD ["crw-server"]`).
- `crw-mcp-proto` - MCP tool/message definitions, shared by embedded and
  standalone MCP servers.
- `crw-mcp` - standalone MCP server binary.
- `crw-browse` - browser-automation MCP server binary.
- `crw-cli` - the `crw` binary itself: parses subcommands and calls into the
  other crates (including embedding `crw-server` for `crw serve`).

Four binaries ship from this workspace: `crw` (from `crw-cli`), `crw-server`
(from `crw-server`), `crw-mcp` (from `crw-mcp`), `crw-browse` (from
`crw-browse`). `crw serve` is `crw-cli` calling into the same `crw-server`
library that the standalone `crw-server` binary wraps.

CLI subcommands (`crw <subcommand>`): `scrape`, `search`, `crawl`, `map`,
`serve`, `bench`, `mcp`, `browse`, `setup`, `doctor`, `smoke`. Bare
`crw <URL>` defaults to `scrape`.

HTTP surfaces served by `crw serve`: native `/v1` and the legacy `/v2` alias
are both re-mounted verbatim under `/firecrawl/*`, so `/firecrawl/v2/x` and
`/v2/x` hit the same handler. Always-public routes (no auth, ever): `/health`,
`/ready`, `/openapi.json`, `/openapi-3.0.json`. Everything else, including
`/metrics` and `/admin/breakers/reset`, sits inside the auth boundary - but
that boundary is a no-op by default: with an empty `[auth] api_keys` list
(the out-of-the-box self-host default), the auth middleware is never
attached and every route above is open. Default bind is `0.0.0.0:3000`.

MCP exposes 10 tools (`crw_scrape`, `crw_crawl`, `crw_check_crawl_status`,
`crw_map`, `crw_extract`, `crw_check_extract_status`, `crw_cancel_extract`,
`crw_enrich`, `crw_search`, `crw_parse_file`); `crw_search` is withheld from
`tools/list` when no search backend is configured, so a bare self-host
advertises 9. `crw_enrich` is embedded-only.

## Source-of-truth matrix

Each derived surface has exactly one generator or one hand-authored source.
No derived surface may be edited by hand without going through its
generator or drift check - that edit will be reverted or will silently
diverge until the next regeneration catches it.

| Concern | Authoritative source | Derived surfaces |
|---|---|---|
| HTTP route existence (interim) | Axum registration | route inventory |
| HTTP public schemas (interim) | embedded/committed OpenAPI artifact | API reference, schema checks |
| Runtime capabilities | typed capability evaluator | `/v1/capabilities`, setup UI, capability docs |
| Configuration fields/defaults | typed Rust config | JSON Schema, config reference, completion |
| CLI commands/options | Clap command definitions | CLI reference and examples |
| Error taxonomy | typed error enums | error reference, SDK helpers |
| Workspace/crate graph | `cargo metadata` | architecture diagram, crate table |
| Product concepts and guidance | authored Markdown | rendered site, llms indexes |
| Agent workflows | canonical skills/references | packaged MCP/plugin copies |

## If you are changing X

- **A route, handler, or auth rule**: `crates/crw-server/src/routes/`,
  `middleware.rs`, `app.rs`. Check both `/v1` and the `/firecrawl` mount
  still agree, and that public routes stay off the auth boundary. Run
  `check` and the `conformance` CI job (native/Firecrawl contract tests).
- **Renderer/escalation behavior**: see `crates/crw-renderer/AGENTS.md`.
  Any change here can move scrape-success, which is a hard gate before
  merge, not after.
- **Config fields or defaults**: the typed Rust config struct is the source;
  regenerate JSON Schema and config docs from it, never hand-edit them.
- **CLI commands/flags**: `crates/crw-cli/src/main.rs` and
  `crates/crw-cli/src/commands/`; the CLI reference is generated from the
  Clap definitions, not hand-written.
- **MCP tools**: `crates/crw-mcp-proto/src/lib.rs` `tool_definitions()`. A
  tool added or removed there must stay in sync with the MCP docs and the
  packaged skill copies (see `docs/AGENTS.md`).
- **Docs content or the docs site**: see `docs/AGENTS.md`.
- **SDKs (Python, TypeScript)**: see `sdks/AGENTS.md`.
- **Anything crossing native `/v1` and Firecrawl-compat `/v2`**: both shapes
  are covered by contract tests; a change to one without the other is a bug,
  not a style choice.

## Verification

- **Fast inner loop**: `make check-fast` - run this while iterating.
- **Full local check**: `make check` - run before considering a change done.
- **Full CI parity**: `make check-ci` - run before pushing when the change
  touches anything CI-sensitive (routes, config, renderer).
- CI's real definition is `.github/workflows/ci.yml`, jobs `check`,
  `sdk-ts`, and `conformance`. If a local target and CI disagree, CI wins;
  fix the local target, don't argue with CI.

## Runtime invariants (product-critical, never trade away for speed)

- A hard-pinned renderer never silently falls back to another renderer.
- `renderJs: false` never invokes a browser renderer.
- Screenshot requests only use capture-capable renderers and fail clearly
  when none is available.
- Proxy configuration fails closed - no silent direct-egress fallback.
- A fallback never replaces usable content with a lower-quality empty
  result.
- SSRF and redirect safety checks are enforced at every outbound boundary.
- Native and Firecrawl-compatible API shapes stay covered by contract
  tests.
- Recall is a hard product invariant: a change that reduces scrape success
  or drops rendered content is not acceptable even when it is faster or
  cheaper.

## Push / release / deploy safety boundaries

Committing, pushing, releasing, tagging, merging to `main`, running
migrations, and touching production each require the maintainer's explicit
approval, every single time - prior approval for one does not carry over to
the next. Versions, tags, and the changelog are owned entirely by the
release automation; they are never edited by hand. A merge to `main` reaches
production automatically through the deploy pipeline, so benchmark and
recall gates matter **before** merge, not after - there is no safety net
between merge and prod.

## Deeper reading

- `docs/docs/architecture.md` (authored) / `docs/architecture/` (generated
  publishing output) - system architecture.
- `docs/docs/crates.md` - the crate table, hand-authored prose. No script
  generates it; `scripts/check-crate-graph-doc.sh` only checks
  `docs/docs/architecture.md` against `cargo metadata`, so a drift in
  `crates.md` is caught by review, not by CI.
- `docs/adr/` - architecture decision records.
- `docs/STYLE_GUIDE.md` - the native-vs-Firecrawl-compat namespace split
  explained for docs authors.
- `crates/crw-renderer/AGENTS.md`, `docs/AGENTS.md`, `sdks/AGENTS.md`.
