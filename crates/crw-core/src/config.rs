use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub renderer: RendererConfig,
    #[serde(default)]
    pub crawler: CrawlerConfig,
    #[serde(default)]
    pub extraction: ExtractionConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub request: RequestConfig,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub map: MapConfig,
    /// `[document]` — binary-document (PDF) parsing knobs.
    #[serde(default)]
    pub document: DocumentConfig,
    /// `[client]` — settings for CLI commands and MCP when they use the hosted
    /// SaaS. Written by `crw setup` into the user-config file.
    #[serde(default)]
    pub client: ClientConfig,
    /// `[mcp]` — MCP tool-response shaping knobs for self-hosted deployments.
    #[serde(default)]
    pub mcp: McpConfig,
}

/// The SDKs' truthiness rule for a flag-like env var, kept identical on
/// purpose: present, and not `0` / `false` / `no` / empty once trimmed and
/// lowercased. See `envTruthy` in `sdks/typescript/src/client.ts` and
/// `_env_truthy` in `sdks/python/src/crw/client.py`. Note `off` and `2` are
/// TRUE under it, as they are there; a helper that disagreed would let
/// `CRW_LOCAL` mean one thing to the SDK and another to the engine.
///
/// `trim` here is Rust's Unicode-whitespace trim. No two of the three runtimes
/// agree on the exact set: Python's `.strip()` also strips U+001C to U+001F,
/// and JavaScript's `.trim()` strips U+FEFF but not U+0085. Only a value padded
/// with one of those can land differently, and the SDKs already disagree with
/// each other there, so matching either exactly is not available.
///
/// Takes the value rather than the variable name, so callers stay testable
/// without touching process env. Use [`env_var_truthy`] to read one from the
/// environment.
/// [`env_truthy`] for a named environment variable.
///
/// Reads through `var_os` and lossily decodes, rather than `var().ok()`, which
/// reports non-UTF-8 as absent. The Python SDK reads those same bytes as
/// truthy, so the `ok()` form would have the SDK go local while the engine it
/// spawned went to the cloud: the exact failure this is here to prevent.
pub fn env_var_truthy(name: &str) -> bool {
    env_truthy(
        std::env::var_os(name)
            .map(|v| v.to_string_lossy().into_owned())
            .as_deref(),
    )
}

pub fn env_truthy(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no"
        )
    })
}

/// `[mcp]` section — controls how the MCP surfaces shape tool responses.
/// Honors the `CRW_MCP__*` env overrides (e.g. `CRW_MCP__HIDE_CREDITS=true`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    /// Strip credit-billing fields (`creditCost`, `creditsUsed`) from every
    /// MCP tool result before it reaches the model's context. These fields
    /// exist for the managed SaaS billing layer; on a self-hosted deployment
    /// they carry no information, cost tokens on every response, and can
    /// distract the model from the fields that matter. Default `false`
    /// preserves the existing response shape for compatibility.
    ///
    /// Only the MCP surfaces are affected (`/mcp` endpoint, `crw-mcp`,
    /// `crw mcp`, in both embedded and proxy mode); the REST API keeps emitting
    /// credit fields so billing integrations and response consumers see no
    /// change. Caller-shaped extraction output is exempt from the strip — see
    /// [`crw_mcp_proto::strip_credit_fields`].
    pub hide_credits: bool,
}

/// `[client]` — Cloud credentials populated by `crw setup` and read by CLI
/// commands such as `crw search`, plus `crw mcp` / `crw-mcp`. Both fields are
/// `Option` so an unconfigured user runs in local mode without surprise
/// overrides.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClientConfig {
    /// Base URL of the hosted CRW API, e.g. `https://api.fastcrw.com`.
    #[serde(default)]
    pub api_url: Option<String>,
    /// API key for the hosted CRW API.
    #[serde(default)]
    pub api_key: Option<String>,
}

/// `[document]` section — controls PDF (and future binary-document) parsing.
/// All knobs honor `CRW_DOCUMENT__*` env overrides.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DocumentConfig {
    /// Master switch for document parsing at runtime (independent of the
    /// compile-time `pdf` cargo feature). When `false`, PDFs are left unparsed.
    pub enabled: bool,
    /// Cap on the number of pages converted per document. `0` = no limit.
    pub max_pages: usize,
    /// Best-effort extraction from scanned/image PDFs (no OCR; usually empty).
    pub attempt_scanned: bool,
    /// Maximum upload size in bytes for `POST /v2/parse`. Defaults to 50 MB,
    /// matching the HTTP renderer's response cap.
    pub max_upload_bytes: usize,
    /// Maximum number of concurrent uploads being parsed at once — bounds peak
    /// memory (each in-flight upload buffers up to `max_upload_bytes`).
    pub upload_concurrency: usize,
    /// Process-wide cap on concurrent PDF parses across ALL surfaces (URL
    /// scrape, crawl, batch, upload). Bounds peak CPU + decompressed memory: a
    /// malicious PDF can decompress far beyond its on-wire size, so this is the
    /// primary memory-DoS guard. Independent of `upload_concurrency` (which
    /// only bounds upload body buffering).
    pub max_concurrent_parses: usize,
    /// Of `max_concurrent_parses`, how many parse slots are RESERVED for
    /// interactive (single-scrape) traffic — a batch/crawl job can never hold
    /// them, so an interactive PDF scrape isn't starved behind a batch that
    /// fills the pool. Clamped so the batch lane keeps ≥1 slot. `Some(0)`
    /// disables the reservation. `None` (absent) → ~1/4 of the ACTUAL configured
    /// `max_concurrent_parses`, floored at 1.
    pub reserved_interactive_parses: Option<usize>,
    /// Wall-clock timeout (ms) for a single PDF parse. A parse exceeding this
    /// returns a timeout error to the caller; protects against pathological
    /// documents that spin the parser. `0` disables the timeout.
    pub parse_timeout_ms: u64,
    /// Decompression-bomb guard: maximum total DECOMPRESSED bytes a document's
    /// FlateDecode streams may inflate to. Checked in bounded memory BEFORE the
    /// parser runs, so a small file that explodes to many GB is rejected with
    /// `pdf_too_large` having allocated only kilobytes. This is the primary
    /// guard against OOM-crashing the host. `0` disables it. Default 100 MiB —
    /// huge for text extraction (millions of words) yet tiny next to host RAM.
    /// Raise only if you must parse image-heavy PDFs.
    pub max_decompressed_bytes: usize,
    /// Run each PDF parse in an isolated child PROCESS (Unix only) instead of
    /// in-process. The child gets a hard OS memory ceiling (`RLIMIT_AS`) and CPU
    /// limit, inherits no env/secrets, and is killed on timeout. A crash, OOM,
    /// or even a hypothetical parser RCE is contained to the child — the main
    /// server (scrape/crawl) keeps running. Costs ~1-3ms spawn overhead per
    /// parse. Recommended for hosts that accept untrusted uploads. Default off.
    pub sandbox: bool,
    /// Hard address-space limit (bytes) for a sandbox child (`RLIMIT_AS`). The
    /// child is aborted by the OS if it allocates beyond this — the ultimate
    /// backstop against memory-DoS even if the decompression guard is bypassed.
    /// Default 512 MiB.
    pub sandbox_memory_bytes: u64,
}

impl Default for DocumentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_pages: 0,
            attempt_scanned: false,
            max_upload_bytes: 52_428_800, // 50 MiB
            upload_concurrency: 4,
            max_concurrent_parses: 4,
            reserved_interactive_parses: None,
            parse_timeout_ms: 30_000,
            max_decompressed_bytes: 104_857_600, // 100 MiB
            sandbox: false,
            sandbox_memory_bytes: 536_870_912, // 512 MiB
        }
    }
}

/// `[map]` section — currently only carries `[map.url_filter]`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MapConfig {
    #[serde(default)]
    pub url_filter: MapUrlFilterConfig,
}

/// `[map.url_filter]` — raw TOML view of the filter knobs. Conversion to
/// the runtime `UrlFilterCfg` lives in `crw-crawl` (which can see both this
/// type and the filter module). Keeping this struct dependency-free here
/// avoids a cycle (`crw-core` does not depend on `crw-crawl`).
#[derive(Debug, Clone, Deserialize)]
pub struct MapUrlFilterConfig {
    /// Tier B — strip tracking params. Default: `true`.
    #[serde(default = "default_true_filter")]
    pub strip_tracking_params: bool,
    /// Tier A — drop action URLs entirely. Default: `true`.
    #[serde(default = "default_true_filter")]
    pub drop_action_urls: bool,
    /// When `true`, `.gov`/`.mil` hosts run Tier A too. Default `false`.
    #[serde(default)]
    pub gov_tld_drop_actions: bool,
    /// Additive on top of `DEFAULT_TRACKING_PARAMS`. Keys are normalized to
    /// canonical form (lowercase, `-` folded to `_`).
    #[serde(default)]
    pub extra_tracking_params: Vec<String>,
    /// Additive on top of `DEFAULT_ACTION_PARAMS`. Keys are normalized to
    /// canonical form (lowercase, `-` folded to `_`).
    #[serde(default)]
    pub extra_action_params: Vec<String>,
    /// Additive on top of `ALWAYS_PRESERVE`. Keys are normalized to
    /// canonical form (lowercase, `-` folded to `_`).
    #[serde(default)]
    pub extra_preserve_params: Vec<String>,
}

impl Default for MapUrlFilterConfig {
    fn default() -> Self {
        Self {
            strip_tracking_params: true,
            drop_action_urls: true,
            gov_tld_drop_actions: false,
            extra_tracking_params: Vec::new(),
            extra_action_params: Vec::new(),
            extra_preserve_params: Vec::new(),
        }
    }
}

fn default_true_filter() -> bool {
    true
}

/// Per-tier CDP overhead in milliseconds — sum of SPA selector poll budget,
/// challenge retry budget, content-stability budget, and fetch overhead.
/// Mirrors the constants in `crw-renderer::cdp`. The drift between the two
/// is regression-tested by `crates/crw-server/tests/cdp_constants_test.rs`
/// (gated behind `feature = "cdp"`).
///
/// Used by [`RendererConfig::min_deadline_for_full_ladder_ms`] so the request
/// deadline accommodates each CDP tier's outer fetch timeout, not just its
/// configured `page_timeout`.
pub const CDP_TIER_OVERHEAD_MS: u64 = 28_000;

/// Hard upper bound on the per-request `wait_for_ms` budget. The Tower outer
/// timeout is sized so a worst-case implicit scrape (no `deadlineMs`,
/// `wait_for` at this maximum) still completes inside it; values above this
/// are clamped by [`AppConfig::effective_deadline_ms`] so the inner deadline
/// can never escape the outer envelope. Documented as `(0, 60000]` in
/// `types.rs::ScrapeRequest::wait_for`.
pub const MAX_WAIT_FOR_MS: u64 = 60_000;

/// Default Camoufox REST per-request budget (ms). Covers the full REST
/// round-trip (create tab → evaluate `outerHTML` → destroy session) plus
/// Camoufox's anti-bot navigation. 60s mirrors the established camofox-browser
/// client navigate budget. Used by [`RendererConfig::camoufox_timeout`].
///
/// There is intentionally no `CAMOUFOX_*_OVERHEAD_MS` analogue to
/// [`CDP_TIER_OVERHEAD_MS`]: Camoufox is a single REST tier, not a CDP tier,
/// and must never be charged CDP overhead.
pub const CAMOUFOX_DEFAULT_TIMEOUT_MS: u64 = 60_000;

/// Default ceiling (ms) for polling a Camoufox tab while a Cloudflare-style JS
/// challenge clears itself, before giving up and reporting the wall. The CDP
/// tiers already run this loop ([`crw-renderer`'s `CHALLENGE_MAX_RETRIES`] ×
/// `CHALLENGE_POLL_INTERVAL_MS` = 9s); Camoufox is a slower engine reached
/// after the CDP tiers have already failed, so it gets a wider ceiling. 20s
/// covers the commonly observed 5-25s clear without eating most of a 60s
/// request budget. Used by [`RendererConfig::camoufox_challenge_wait`].
pub const CAMOUFOX_DEFAULT_CHALLENGE_WAIT_MS: u64 = 20_000;

/// Default cloak (Turnstile-solver sidecar) per-request budget (ms) — the
/// per-attempt solve budget bounding one sidecar mirror call. A cold interactive
/// Turnstile solve is ~21s; 35s leaves margin for the browser solve + curl_cffi
/// replay. Used by [`RendererConfig::cloak_timeout`].
pub const CLOAK_DEFAULT_TIMEOUT_MS: u64 = 35_000;

/// Minimum remaining request budget for the cloak recovery arm to fire (ms),
/// DECOUPLED from [`CLOAK_DEFAULT_TIMEOUT_MS`]: one cold solve (~21s) + margin.
/// Reusing the full per-attempt budget as the floor was a design bug — the
/// serial ladder does not short-circuit on CF detection and burns ~18s first,
/// so a 35s floor could never be cleared under any legal deadline. `pub const`
/// (not `pub(crate)`) so a lean build referencing it only under `#[cfg(cloak)]`
/// does not trip `dead_code` — mirrors [`CAMOUFOX_DEFAULT_TIMEOUT_MS`].
/// NOTE: the 24s margin assumes `chrome_challenge_max_retries` stays OFF (prod
/// default); enabling it grows the ladder tax and this floor must be revisited.
pub const CLOAK_ARM_FLOOR_MS: u64 = 24_000;

/// Fresh, decoupled budget (ms) for the post-ladder cloak recovery arm when
/// [`RendererConfig::cloak_recover_on_cf`] is enabled and the shared request
/// deadline is below [`CLOAK_ARM_FLOOR_MS`]. Matches [`RendererConfig::cloak_timeout`]'s
/// default: a cold interactive Turnstile solve is ~21-30s, and the DataImpulse
/// mobile-proxy replay adds margin — 40s covers a cold solve end to end. Only
/// meaningful when the caller's outer request timeout can absorb it (the SaaS
/// widens its engine deadline to 58s behind `CLOAK_TIER_ENABLED=1` for exactly
/// this reason). `pub const` (not `pub(crate)`) for the same lean-build
/// `dead_code` reason as [`CLOAK_ARM_FLOOR_MS`] above.
pub const CLOAK_ARM_RECOVER_BUDGET_MS: u64 = 40_000;

/// Default cloak arm concurrency, matching the sidecar's own browser cap
/// (`CLOAK_CONCURRENCY`, default 4). Overridable per deployment with
/// `renderer.cloak_pool_size`; see [`RendererConfig::cloak_pool_size`].
pub const CLOAK_DEFAULT_POOL_SIZE: usize = 4;

/// Budget (ms) the cloak-FIRST hint reserves for the normal ladder when it fires
/// before the ladder. Unlike the post-ladder recovery arm (which runs last, so
/// eating the deadline is harmless), cloak-first runs first on the SHARED
/// deadline; without a reserve a slow/failed solve on a MIS-flagged non-CF domain
/// would starve the ladder and turn a would-be success into a Timeout (recall
/// regression). The cloak-first entry gate requires `CLOAK_ARM_FLOOR_MS + this`,
/// and the cloak call is capped at `remaining - this`, so the ladder always keeps
/// at least this much to render a mis-flagged page. `pub const` for the same
/// lean-build `dead_code` reason as [`CLOAK_ARM_FLOOR_MS`].
pub const CLOAK_FIRST_LADDER_RESERVE_MS: u64 = 8_000;

/// Configuration for the `/v1/search` endpoint and its SearXNG backend.
///
/// When `search_backend_url` is unset the endpoint returns HTTP 503 with
/// `error_code: "search_disabled"` — the route remains mounted so that
/// startup doesn't have to know whether search will ever be configured.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchConfig {
    /// Master switch. Defaults to `true`; set to `false` to refuse all
    /// `/v1/search` requests even if a backend URL is configured.
    #[serde(default = "default_true_search")]
    pub enabled: bool,
    /// Base URL of the self-hosted search backend (e.g. `http://search:8080`).
    /// `None` (the default) disables the endpoint with a clear error.
    ///
    /// Read this through [`SearchConfig::resolve_backend_url`], which folds in
    /// the legacy spelling below.
    #[serde(default)]
    pub search_backend_url: Option<String>,
    /// The original name of `search_backend_url`, kept so existing configs and
    /// `CRW_SEARCH__SEARXNG_URL` keep working.
    ///
    /// This is a **separate field rather than a `#[serde(alias)]`** on purpose:
    /// an alias makes serde fail with `duplicate field` as soon as both
    /// spellings resolve at once, which is exactly what production does — the
    /// old name in `config.docker.toml` plus the old env var. Two fields let
    /// both arrive so we can pick a winner instead of refusing to start.
    #[serde(default)]
    pub searxng_url: Option<String>,
    /// OpenAlex API key for the `/v1/search/research/*` endpoints (CC0 data,
    /// commercial use OK). `None` falls back to the keyless polite pool.
    #[serde(default)]
    pub openalex_api_key: Option<String>,
    /// Contact email for OpenAlex's "polite pool" (`mailto=`), recommended for
    /// higher rate limits. `None` omits it.
    #[serde(default)]
    pub openalex_mailto: Option<String>,
    /// Semantic Scholar API key (`x-api-key`) for the research endpoints'
    /// full-text snippet + citation-graph boosters. `None` uses the shared
    /// (1 RPS) unauthenticated tier.
    #[serde(default)]
    pub s2_api_key: Option<String>,
    /// End-to-end timeout for the SearXNG call in milliseconds.
    #[serde(default = "default_search_timeout_ms")]
    pub timeout_ms: u64,
    /// Default `limit` when the request omits it.
    #[serde(default = "default_search_limit")]
    pub default_limit: u32,
    /// Hard cap on `limit` per request. SaaS uses 20.
    #[serde(default = "default_search_max_limit")]
    pub max_limit: u32,
    /// SearXNG engines invoked when the request includes `categories: ["research"]`.
    /// Defaults match the SaaS implementation.
    #[serde(default = "default_research_engines")]
    pub research_engines: Vec<String>,
    /// SearXNG engines invoked when the request includes `categories: ["github"]`.
    #[serde(default = "default_github_engines")]
    pub github_engines: Vec<String>,
    /// Re-rank the flat result pool for the LLM answer / summarize path
    /// (RRF + junk/coverage/geo filter + BM25 + domain dedupe) instead of the
    /// raw SearXNG-score sort. Defaults to `true`. The plain (non-LLM) path is
    /// unaffected and keeps SaaS byte-parity regardless of this flag.
    #[serde(default = "default_true_search")]
    pub rerank_enabled: bool,
    /// Multi-query expansion for the LLM answer / summarize path: before the
    /// SearXNG fetch, generate an entity/keyword-focused rewrite of the query,
    /// fetch both the original and the rewrite, and UNION the candidate pools
    /// (recall can only increase — the original's results are always kept).
    /// Targets "retrieval-miss" failures where the answer's source never
    /// surfaced for the user's phrasing. Costs one extra small LLM call + one
    /// extra SearXNG fetch. Defaults to `false` (gated); the plain path and the
    /// answer layer are untouched, so precision/SaaS-parity are preserved.
    #[serde(default)]
    pub query_expand: bool,
    /// Number of LLM-generated query rewrites to fetch + union when
    /// `query_expand` is on. `1` reproduces the original single-variant
    /// behavior. Higher values request more DIVERSE reformulations
    /// (abbreviation/acronym-expanded, keyword-focused) and fetch their pools
    /// in parallel, raising recall on retrieval-miss queries (e.g. an
    /// unexpanded acronym whose page never surfaced) at the cost of one extra
    /// SearXNG fetch each. Clamped to `MAX_QUERY_EXPAND_VARIANTS` in the route.
    #[serde(default = "default_query_expand_variants")]
    pub query_expand_variants: usize,
    /// Phase C1 (latency-qn): on the answer path with query_expand + scrapeOptions,
    /// scrape the original-query results CONCURRENTLY with the expansion (LLM
    /// rewrite + variant SearXNG fetches), then union and reuse the scrapes.
    /// Final source set is identical to the serial path (rerank over the same
    /// union) → quality-neutral; only the scheduling overlaps the ~5-10s
    /// expansion overhead. Default off.
    #[serde(default)]
    pub pipeline_overlap: bool,
    /// Adaptive multi-round retrieval (the "evidence-scout" loop). When the
    /// round-1 answer ABSTAINS (sources lacked the fact), an LLM scout reads the
    /// round-1 evidence and emits targeted follow-up queries (acronym-expanded,
    /// exact-entity, predicate/date-specific); their results are scraped, unioned
    /// into the pool, and the answer is re-synthesized ONCE. Bounded (one extra
    /// round, capped follow-up queries) so worst-case stays within the request
    /// deadline. Only fires on abstention, so ~most queries keep the single-shot
    /// fast path. Recall-only + monotone-safe: a still-abstaining round-2 is
    /// discarded, keeping round-1. Targets "the answer page never entered the
    /// first pool" — the dominant remaining miss. Defaults to `false` (gated).
    #[serde(default)]
    pub multi_round: bool,
    /// Snippet-first answer path (lazy scrape). When on, the answer is first
    /// synthesized from the FREE SearXNG snippets (title/description) + any
    /// structured sources, WITHOUT scraping. Only if that answer abstains does
    /// the original result set get scraped and the answer re-synthesized once.
    /// Most factoid queries answer from the snippet, so this skips the expensive
    /// scrape (chrome RAM + the p90 latency tail) on the majority of traffic.
    /// Recall-safe + monotone: a still-abstaining post-scrape answer keeps the
    /// snippet answer; abstention always escalates to the full scrape. Defaults
    /// to `false` (gated).
    #[serde(default)]
    pub snippet_first: bool,
    /// Passage-level relevance gate for the LLM answer path: split each scraped
    /// source into passages and feed the answer LLM only the query-relevant
    /// ones (DeepSeek-scored, no new ML deps). Subtractive — removes noise, never
    /// adds sources or forces commits; falls back to the full source on any
    /// failure (byte-identical to off), so it is monotone-safe. Defaults to
    /// `false` (gated); answer prompt + plain path untouched.
    #[serde(default)]
    pub passage_select: bool,
    /// Cheap BM25 variant of `passage_select` for the plain answer path: when a
    /// scraped source exceeds `max_chars_per_source`, keep its query-relevant
    /// passages (sentence-chunked, BM25-ranked, no LLM call) instead of a blind
    /// head-truncation that drops answers buried deep in the page. Two-pass so it
    /// NEVER feeds less than a head-truncation (fills the budget after ranking),
    /// so it is monotone-safe on recall. Defaults to `false` (gated); off =
    /// byte-identical head-truncation, A/B keep/revert in prod. Ignored when
    /// `passage_select` is on (that path LLM-reduces sources instead).
    #[serde(default)]
    pub answer_bm25_select: bool,
    /// Page-2 fallback for the LLM answer / summarize path: if the reranked
    /// (junk-filtered, deduped) candidate pool comes back thinner than the
    /// answer needs (`< answer_top_n`), fetch the SAME query's SearXNG page 2
    /// once and union it in, then re-rank. The trigger is evaluated POST-rerank,
    /// so a junk-heavy first page does not suppress it; the extra fetch only
    /// fires on already-under-yielding queries (QPS never doubles across the
    /// corpus). Recall-only + abstention is untouched (a sparse page1+page2 pool
    /// still abstains). Defaults to `false` (gated); requires `rerank_enabled`.
    #[serde(default)]
    pub page2_fallback: bool,
    /// Calibrated answer path (gated): reduce recoverable OVER-abstentions by
    /// (a) feeding more sources to the answer LLM by default (top_n 5->8, so the
    /// answer in result #6-8 or behind a failed top-5 scrape still reaches it)
    /// and (b) swapping the answer prompt's abstention rule for an anti-hedge
    /// variant — commit when the sources DO contain the answer (even indirectly
    /// / one inference step), abstain ONLY when they genuinely lack it. The
    /// "use ONLY sources" grounding is untouched, so this is the precise inverse
    /// of the cycle-1 blunt "always commit" failure (which forced commits on
    /// no-source cases). Default false; A/B with an INCORRECT-guard before flip.
    #[serde(default)]
    pub answer_calibrated: bool,
    /// Moat-hardening abstention (gated). Appends a clause making the answer
    /// model (a) REJECT a false/unverifiable premise instead of answering as
    /// though it were true, (b) report when sources CONFLICT rather than picking
    /// one confidently, and (c) abstain when not confident. Targets the
    /// adversarial failure SealQA Seal-0 exposed: 32% confident-WRONG
    /// (hallucination) on conflicting-source / false-premise questions, where
    /// the "use ONLY sources" rule alone is insufficient. Complements (does not
    /// replace) `answer_calibrated`. Default false; A/B requires Seal-0
    /// hallucination DOWN with SimpleQA accuracy NOT regressed before flip.
    #[serde(default)]
    pub answer_guarded: bool,
    /// Use SearXNG structured sources (gated, W0). SearXNG's `infoboxes[]` /
    /// `answers[]` arrays carry Wikidata/Wikipedia knowledge-panel facts
    /// (entity attributes like religion/capital/director) that the `results[]`
    /// transform path discards. With this on, those facts are parsed and pinned
    /// as a high-trust source at the FRONT of the answer pool (still
    /// UNTRUSTED-wrapped — widens evidence, never bypasses the safety wrapper).
    /// Targets the obscure-entity recall gap (PopQA). Default false; A/B on
    /// diag500 gold-in-sources with the wrong-non-abstain invariant before flip.
    #[serde(default)]
    pub use_structured_sources: bool,
    /// Deterministic Wikidata entity-relation lookup (gated, W3). For
    /// `<relation> of <entity>` questions (PopQA's obscure long tail that web
    /// search can't surface), classify -> wbsearchentities -> property fetch and
    /// pin the fact as a structured source (UNTRUSTED-wrapped, runs in parallel
    /// with SearXNG, 3s-bounded, any error falls through). Free open data, no
    /// AI, no SPARQL hot-path. Default false; A/B on diag500 PopQA accuracy +
    /// the wrong-non-abstain invariant before flip.
    #[serde(default)]
    pub wikidata_lookup: bool,
    /// Snippet-first grounding for the LLM answer path (gated). With this on, the
    /// SearXNG `description` snippet is prepended to EVERY answer source as
    /// `[snippet] <desc>\n\n<body>` (not merely a fallback for failed scrapes):
    /// the snippet is the engine's own query-relevant answer passage, so putting
    /// it first means it survives the per-source passage budget. It also block-
    /// guards the body — a fetched-but-blocked page ("Wikimedia Error", a bot
    /// wall) is dropped in favor of the clean snippet. When a scrape returned no
    /// markdown at all, the snippet still keeps the result in the pool instead of
    /// dropping the (possibly answer-bearing) page. The snippet is verbatim
    /// upstream text, so it cannot inject a fact not already present — near-zero
    /// INCORRECT exposure. Default false; off = markdown-only (legacy), A/B in prod.
    #[serde(default)]
    pub snippet_fallback: bool,
    /// Relevance gate for the LLM answer / summarize re-rank (gated). After the
    /// lexical-core junk/coverage/geo filters, keep only the rows that cover the
    /// MOST important (non-stopword) query terms present in the pool, so a
    /// partial-match homonym ("best pizza in REDMOND" for "best pizza in
    /// belgrade", coverage 1/2) is evicted the instant a full-match row
    /// ("pizza … belgrade", 2/2) is present. Ranks on the query's OWN tokens —
    /// no geo/country/IP signal — so it holds for self-hosted deployments in any
    /// region. Monotone-safe (degrade fallback applies first; never empties a
    /// non-empty pool). Requires `rerank_enabled`. Default false; A/B against
    /// the frozen rerank benchmark before flip.
    #[serde(default)]
    pub rerank_relevance: bool,
    /// List-format answers for the LLM answer path (gated). When the query has
    /// list intent ("best/top X in Y", "recommend …", "list of …"), the answer
    /// prompt's prose directive is swapped for a ranked-list directive so the
    /// model emits up to 10 named options (`N. <name> — <why>`) instead of a
    /// 3–6 sentence paragraph. A deterministic classifier (`is_list_intent`)
    /// decides per query; factual/non-list queries are untouched. The "use ONLY
    /// sources" grounding, the abstention rule, and the `===CITATIONS===` block
    /// are preserved (no fabrication, citation moat intact). Default false; A/B
    /// against the answer-accuracy benchmark before flip.
    #[serde(default)]
    pub answer_list_format: bool,
}

impl SearchConfig {
    /// The effective search backend URL, preferring the current key name and
    /// falling back to the original one.
    ///
    /// Every caller must go through this rather than reading either field, so
    /// a deployment that still sets only the old name keeps working.
    pub fn resolve_backend_url(&self) -> Option<&str> {
        self.search_backend_url
            .as_deref()
            .or(self.searxng_url.as_deref())
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            search_backend_url: None,
            searxng_url: None,
            openalex_api_key: None,
            openalex_mailto: None,
            s2_api_key: None,
            timeout_ms: default_search_timeout_ms(),
            default_limit: default_search_limit(),
            max_limit: default_search_max_limit(),
            research_engines: default_research_engines(),
            github_engines: default_github_engines(),
            rerank_enabled: true,
            query_expand: false,
            query_expand_variants: default_query_expand_variants(),
            pipeline_overlap: false,
            multi_round: false,
            snippet_first: false,
            passage_select: false,
            answer_bm25_select: false,
            page2_fallback: false,
            answer_calibrated: false,
            answer_guarded: false,
            use_structured_sources: false,
            wikidata_lookup: false,
            snippet_fallback: false,
            rerank_relevance: false,
            answer_list_format: false,
        }
    }
}

fn default_query_expand_variants() -> usize {
    1
}
fn default_true_search() -> bool {
    true
}
fn default_search_timeout_ms() -> u64 {
    15_000
}
fn default_search_limit() -> u32 {
    5
}
fn default_search_max_limit() -> u32 {
    20
}
fn default_research_engines() -> Vec<String> {
    vec![
        "arxiv".into(),
        "crossref".into(),
        "google scholar".into(),
        "semantic scholar".into(),
    ]
}
fn default_github_engines() -> Vec<String> {
    vec!["github".into()]
}

/// Per-request defaults that apply to every scrape, crawl, or map call when
/// the caller does not specify an override. Currently only governs the
/// end-to-end deadline budget (see `crw-core/src/deadline.rs`).
#[derive(Debug, Clone, Deserialize)]
pub struct RequestConfig {
    /// Default end-to-end deadline budget in milliseconds when a request does
    /// not specify `deadlineMs`. The SLO p95 latency metric is computed only
    /// over requests with `deadline_ms <= 8000`; longer values land in a
    /// separate slow-path histogram.
    #[serde(default = "default_deadline_ms")]
    pub deadline_ms_default: u64,
    /// When `true` (default), an implicit deadline (no per-request `deadlineMs`)
    /// is auto-extended to `max(deadline_ms_default, ladder_min)` where
    /// `ladder_min = sum(http+lightpanda+chrome timeouts) + N_cdp_tiers * 28s`.
    /// This prevents `chrome_timeout_ms = 30000` from appearing inert when
    /// `deadline_ms_default` is small (issue #35).
    ///
    /// Set to `false` to enforce a strict SLO regardless of tier sizing —
    /// requests that would have completed under the extended budget will
    /// instead time out at `deadline_ms_default`.
    #[serde(default = "default_true_request")]
    pub auto_extend_deadline_for_ladder: bool,
}

impl Default for RequestConfig {
    fn default() -> Self {
        Self {
            deadline_ms_default: default_deadline_ms(),
            auto_extend_deadline_for_ladder: true,
        }
    }
}

fn default_true_request() -> bool {
    true
}

fn default_deadline_ms() -> u64 {
    8000
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
    /// Maximum requests per second (global). 0 = unlimited.
    #[serde(default = "default_rate_limit_rps")]
    pub rate_limit_rps: u64,
    /// Cross-origin allowlist for browser callers. Empty (default) = NO CORS
    /// headers are emitted, so browsers block cross-origin JS access — the safe
    /// default for a server-to-server API. Populate only if a browser app must
    /// call the engine directly (e.g. `["https://app.example.com"]`). A literal
    /// `"*"` is rejected: wildcard CORS is exactly the permissive default this
    /// setting replaces. Accepts a TOML array or a comma-separated env string via
    /// `CRW_SERVER__CORS_ALLOWED_ORIGINS`.
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    pub cors_allowed_origins: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            request_timeout_secs: default_request_timeout(),
            rate_limit_rps: default_rate_limit_rps(),
            cors_allowed_origins: Vec::new(),
        }
    }
}

fn default_rate_limit_rps() -> u64 {
    10
}

fn default_host() -> String {
    "0.0.0.0".into()
}
fn default_port() -> u16 {
    3000
}
fn default_request_timeout() -> u64 {
    60
}

/// Selects which JS renderer(s) the [`FallbackRenderer`] will build.
///
/// - `Auto` (default): try every configured CDP endpoint (Lightpanda, Playwright, Chrome)
///   in order. If none is configured, JS rendering is disabled but HTTP still works.
/// - `None`: HTTP-only. Never attempt JS rendering.
/// - `Lightpanda` / `Chrome` / `Playwright`: require the matching `[renderer.<name>]`
///   endpoint; fail startup if missing. Only the named backend is used.
///
/// [`FallbackRenderer`]: https://docs.rs/crw-renderer/latest/crw_renderer/struct.FallbackRenderer.html
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RendererMode {
    #[default]
    Auto,
    None,
    Lightpanda,
    Chrome,
    Playwright,
    /// Opt-in Camoufox stealth tier (REST, not CDP). Pinning `mode = "camoufox"`
    /// uses only this tier. See [`CamoufoxEndpoint`]. Requires the `camoufox`
    /// build feature; a build without it rejects this mode at renderer
    /// construction time.
    Camoufox,
    /// Opt-in cloak Turnstile-solver tier (REST recovery arm). See
    /// [`CloakEndpoint`]. Requires the `cloak` build feature; a build without it
    /// rejects this mode at renderer construction time.
    Cloak,
}

/// How the residential proxy tiers (`chrome_proxy`, cloak) write the request
/// country and the sticky session into the proxy username.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyUsernameFormat {
    /// `{user}__cr.{cc};sessid.stick{id}`
    #[default]
    DataImpulse,
    /// `{user}-country-{cc}-sid-{id}`
    NodeMaven,
}

impl ProxyUsernameFormat {
    /// `country` is expected to be already normalized by [`normalize_proxy_country`].
    pub fn compose(self, user: &str, country: Option<&str>, session: Option<&str>) -> String {
        let (country_sep, session_sep) = match self {
            Self::DataImpulse => ("__cr.", ";sessid.stick"),
            Self::NodeMaven => ("-country-", "-sid-"),
        };
        let mut out = user.to_string();
        if let Some(cc) = country {
            out.push_str(country_sep);
            out.push_str(cc);
        }
        if let Some(id) = session {
            out.push_str(session_sep);
            out.push_str(id);
        }
        out
    }
}

/// Lower-cased two-letter country code, or `None` for anything else (wrong
/// length, non-alphabetic, empty), which selects the provider's global pool.
pub fn normalize_proxy_country(country: Option<&str>) -> Option<String> {
    country
        .map(|s| s.trim().to_lowercase())
        .filter(|s| s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic()))
}

#[derive(Debug, Clone, Deserialize)]
pub struct RendererConfig {
    #[serde(default)]
    pub mode: RendererMode,
    /// Generic per-page navigation timeout. Used as the fallback when no
    /// per-tier override is configured. Kept for backward compatibility — the
    /// per-tier knobs below are preferred for new deployments.
    #[serde(default = "default_page_timeout")]
    pub page_timeout_ms: u64,
    /// Override for the HTTP-only fetcher request timeout. Falls back to
    /// `page_timeout_ms` when unset. HTTP responses arrive quickly when they
    /// arrive at all, so 15s is generous and keeps slow upstreams from
    /// hogging the request budget that should be spent on JS retries.
    #[serde(default)]
    pub http_timeout_ms: Option<u64>,
    /// Override for the LightPanda CDP renderer. LightPanda completes most
    /// renders in <10s; if it stalls past 20s it almost always means an
    /// adversarial page that Chrome will render anyway, so failing fast and
    /// escalating beats waiting it out.
    #[serde(default)]
    pub lightpanda_timeout_ms: Option<u64>,
    /// Override for the full-Chromium tier. Chrome is the slow path
    /// (gov/legal SPAs need 30–40s for `networkidle`); the larger budget here
    /// recovers ~6 URLs per fc-wins iteration without affecting the fast path.
    #[serde(default)]
    pub chrome_timeout_ms: Option<u64>,
    #[serde(default = "default_pool_size")]
    pub pool_size: usize,
    /// Concurrency cap for the `chrome_proxy` hard-block recovery arm
    /// (`auto_egress_escalation`), decoupled from the general `pool_size`.
    /// `None` falls back to `pool_size` — self-hosters see identical behavior.
    /// Raise this independently when the arm is shedding recoveries
    /// (`crw_render_route_decision_total{decision="armShed"}` rising) without
    /// touching the main chrome/lightpanda tiers' concurrency.
    #[serde(default)]
    pub chrome_proxy_pool_size: Option<usize>,
    /// Concurrency cap for the cloak recovery arm, i.e. how many pages this box
    /// can have in the stealth tier at once, across every host and customer.
    ///
    /// `None` keeps the historical 4, which is the sidecar's own browser cap.
    /// It is a REAL ceiling on throughput: at ~5s a warm ride that is ~0.8
    /// pages/s box-wide, so a batch customer on a walled host is bounded by it
    /// long before any per-plan limit. Raise it only together with the
    /// sidecar's `CLOAK_CONCURRENCY` and its container memory: each permit can
    /// hold one headful browser (a camoufox mint ~400 MB, a Chromium ride
    /// ~300 MB), and a permit the sidecar cannot serve turns a fast clean
    /// block into a queued one.
    ///
    /// Watch `crw_render_route_decision_total{renderer="cloak",decision="armShed"}`:
    /// rising means requests are being shed at this ceiling.
    #[serde(default)]
    pub cloak_pool_size: Option<usize>,
    /// latency-qn: override the chrome post-navigate challenge-clear retry count
    /// (default 3 → 3×3s=9s). Measured at 28% of render time, mostly on shells
    /// that never clear (fail anyway); Firecrawl/Spider run no such loop. Lower
    /// to trim the anti-bot tail (e.g. 1); 0 disables it. `None` keeps the 3
    /// default. A/B-gated: must hold scrape-success/recall on the bench.
    #[serde(default)]
    pub chrome_challenge_max_retries: Option<u32>,
    /// latency-qn: override the chrome SPA-readiness poll budget (default 8000ms,
    /// `SPA_SELECTOR_MAX_MS`). Measured at 67% of render time. The poll still
    /// exits early on content-ready/network-idle; this caps the wait when the
    /// selector never mounts. A/B-gated on the bench. `None` keeps 8000.
    #[serde(default)]
    pub chrome_spa_selector_max_ms: Option<u64>,
    /// latency-qn: event-driven earliest-ready render exit. When true, the
    /// post-navigate poll exits as soon as the page is genuinely settled (body
    /// innerText ≥ content-floor AND networkAlmostIdle≤2, OR substantial text)
    /// instead of requiring a specific content selector + networkIdle(0) up to
    /// the 8s ceiling. Keeps a mandatory content floor (never snapshots an empty
    /// shell). Default off; A/B-gated on the bench (quality_gate must hold).
    #[serde(default)]
    pub chrome_fast_ready: bool,
    /// latency-qn: conditional hedge. In auto mode, race lightpanda + chrome
    /// CONCURRENTLY (chrome's clock starts immediately instead of after lightpanda
    /// fails) and take the best by tier priority. Cuts the serial prefix (~3.4s
    /// mean / 5.7s p90) on chrome-bound pages. Bounded by a headroom semaphore
    /// (falls back to serial when the pool is busy) so it can't deadlock the
    /// context pool. best-result-wins ⇒ success/recall ≡ serial. Default off.
    #[serde(default)]
    pub chrome_hedge: bool,
    /// Phase 2 (latency-qn): gated auto-egress escalation. When true, the
    /// chrome_proxy (residential/stealth) tier is REMOVED from the normal
    /// HTTP→LP→Chrome ladder and instead fired ONCE, only when the ladder's
    /// result is a hard block (403/429/503/401/520-530 or a CF/bot-wall/vendor
    /// interstitial) AND the remaining deadline can absorb a full chrome_proxy
    /// attempt AND its breaker is closed. The retry is best-result-wins vs the
    /// ladder's result (never replaces usable content with empty). Bench proved
    /// a naive always-on chrome_proxy ladder is net-negative (success −2pp, p90
    /// +69%); this gate is what makes residential recovery net-positive. Off by
    /// default; requires a configured `[renderer.chrome_proxy]` tier to do anything.
    #[serde(default)]
    pub auto_egress_escalation: bool,
    /// Give the post-ladder cloak CF-recovery arm a fresh, decoupled budget
    /// ([`CLOAK_ARM_RECOVER_BUDGET_MS`]) when a Cloudflare challenge is
    /// detected and the shared request deadline is already below
    /// [`CLOAK_ARM_FLOOR_MS`] — instead of skipping the arm outright, as it
    /// does today under a small SaaS-supplied deadline. Only relaxes the
    /// cloak arm's own entry gate; the `chrome_proxy` CF-suppression, the
    /// `cloak_sem` load-shed, and the per-host breaker check are unchanged.
    /// Default off (clean keep/revert A/B); only meaningful when the outer
    /// request timeout can absorb the fresh budget (see
    /// `CLOAK_ARM_RECOVER_BUDGET_MS` doc).
    #[serde(default)]
    pub cloak_recover_on_cf: bool,
    /// Let the cloak recovery arm fire on a Cloudflare challenge that the
    /// auto-egress (chrome_proxy) arm uncovered, not only on one the ladder
    /// itself saw.
    ///
    /// Some origins answer a datacenter IP with the Cloudflare WAF BLOCK page
    /// (error 1020, "Attention Required") and only serve the managed challenge
    /// once the request arrives from a residential exit. The block page carries
    /// one weak marker and no strong one, so
    /// [`crate::detector`]-equivalent challenge detection is correctly false for
    /// it, the request takes the IP-reputation path into chrome_proxy, and the
    /// challenge that arm then uncovers has nowhere to go: the cloak gate was
    /// already decided. Measured on a people-search origin behind a datacenter
    /// WAF block plus a managed challenge, where every scrape returned
    /// `anti_bot` and the solver tier was never reached.
    ///
    /// Off by default. The arm it opens costs a cold interactive solve, so this
    /// is opt-in per deployment and reverts without a redeploy of the engine.
    #[serde(default)]
    pub cloak_after_egress: bool,
    /// Phase 0 (latency-qn): when true, the renderer emits a structured
    /// `target: "latency_breakdown"` tracing event per fetch with total wall
    /// time and the tier that produced the accepted result. Off by default;
    /// turned on only for bench/diagnostic runs so we can see where the p90
    /// budget actually goes (HTTP fast-path vs JS render) before optimizing.
    #[serde(default)]
    pub latency_breakdown: bool,
    /// If set, applies to every request that doesn't specify `renderJs` explicitly.
    /// `Some(true)` = force JS rendering; `Some(false)` = skip JS; `None` = auto-detect.
    ///
    /// Accepts the `force_js` alias for backward compatibility.
    #[serde(default, alias = "force_js")]
    pub render_js_default: Option<bool>,
    #[serde(default)]
    pub lightpanda: Option<CdpEndpoint>,
    #[serde(default)]
    pub playwright: Option<CdpEndpoint>,
    #[serde(default)]
    pub chrome: Option<CdpEndpoint>,
    /// Residential-proxy Chrome tier (opt-in 4th renderer). Same Chromium
    /// browser as `chrome`, but egress routed through a forwarder that adds
    /// upstream proxy auth (e.g. DataImpulse). Tried after Chrome fails —
    /// covers IP-blocked targets where the browser fingerprint is fine but
    /// the VPS egress IP is flagged.
    #[serde(default)]
    pub chrome_proxy: Option<CdpEndpoint>,
    /// Per-tier nav timeout override for `chrome_proxy`. When unset, defaults
    /// to `chrome_timeout() + 15_000` — the proxy hop adds latency, so the
    /// fallback tier needs more headroom than direct Chrome.
    #[serde(default)]
    pub chrome_proxy_timeout_ms: Option<u64>,
    /// Opt-in Camoufox stealth REST endpoint. See [`CamoufoxEndpoint`].
    /// `None` = not configured (default). Orthogonal to the CDP tiers: a
    /// configured endpoint with the default `include_in_auto = false` does NOT
    /// change the existing auto ladder — it is reachable only via an explicit
    /// per-request `renderer = "camoufox"` pin or `mode = "camoufox"`.
    #[serde(default)]
    pub camoufox: Option<CamoufoxEndpoint>,
    /// Per-request Camoufox REST budget override (ms). Falls back to
    /// [`CAMOUFOX_DEFAULT_TIMEOUT_MS`] when unset.
    #[serde(default)]
    pub camoufox_timeout_ms: Option<u64>,
    /// Ceiling (ms) for polling a Camoufox tab while a bot-challenge
    /// interstitial clears, before giving up and reporting the wall. Falls back
    /// to [`CAMOUFOX_DEFAULT_CHALLENGE_WAIT_MS`] when unset; `Some(0)` disables
    /// the poll and restores the single-shot behaviour. The Camoufox analogue
    /// of `chrome_challenge_max_retries`.
    /// Env: `CRW_RENDERER__CAMOUFOX_CHALLENGE_WAIT_MS`.
    #[serde(default)]
    pub camoufox_challenge_wait_ms: Option<u64>,
    /// Opt-in cloak Turnstile-solver sidecar endpoint. See [`CloakEndpoint`].
    /// `None` = not configured (default) → the tier is never constructed and the
    /// engine is byte-identical to a build without it. Fired only as a
    /// CF-challenge recovery arm, never in the normal ladder.
    #[serde(default)]
    pub cloak: Option<CloakEndpoint>,
    /// Per-attempt cloak solve budget override (ms). Falls back to
    /// [`CLOAK_DEFAULT_TIMEOUT_MS`] when unset.
    #[serde(default)]
    pub cloak_timeout_ms: Option<u64>,
    /// Proxy `scheme://host:port` (creds-free, e.g. `http://gw.dataimpulse.com:823`)
    /// for the cloak arm to self-provision a residential exit when no per-request
    /// managed proxy was injected. Creds come from `proxy_base_user`/`_pass`.
    /// `None`/empty = the cloak arm uses only the per-request proxy (today's
    /// behavior) — so an unset value is byte-identical.
    #[serde(default)]
    pub cloak_proxy_host: Option<String>,
    /// In-process Chrome-impersonation HTTP tier (wreq). See
    /// [`ImpersonatedConfig`]. Unlike every other tier there is no endpoint;
    /// `enabled` is the runtime kill switch and defaults to true, so an
    /// absent section means "on with defaults" in any build compiled with the
    /// `impersonated` feature.
    #[serde(default)]
    pub impersonated: ImpersonatedConfig,
    /// Enable Chrome *resource* interception (blocking of media, fonts,
    /// trackers). Default `false`.
    ///
    /// This no longer controls whether `Fetch.enable` runs: the interception
    /// pump is always on, because it also validates every destination the
    /// browser reaches on its own (redirects, JS navigation, iframes, XHR),
    /// which the route-layer URL check cannot see. This flag only decides
    /// whether the ad/resource blocklist runs alongside that check.
    #[serde(default)]
    pub chrome_intercept_resources: bool,
    /// Additionally block `stylesheet` requests when interception is enabled.
    /// Default `false` — kept off in v1 because some extractors depend on
    /// CSS-driven visibility / lazy-content triggers.
    #[serde(default)]
    pub chrome_intercept_stylesheets: bool,
    /// Per-host opt-out for the resource blocklist. Hosts in this list skip
    /// ad/resource blocking even when `chrome_intercept_resources = true`.
    ///
    /// It no longer turns `Fetch.enable` off for those hosts: the destination
    /// check is a security control and has no per-host opt-out.
    #[serde(default)]
    pub chrome_host_intercept_disable: Vec<String>,
    /// Hard chrome-tier navigation budget in ms. Wraps `wait_for_page_ready`
    /// in an inner race; on budget hit the renderer snapshots whatever DOM is
    /// present and returns `truncated = true`. Calibrated as
    /// `p90(successful chrome renders)` clamped to `[8_000, 12_000]`.
    #[serde(default = "default_chrome_nav_budget_ms")]
    pub chrome_nav_budget_ms: u64,
    /// Enable the bounded browser-context pool. Default `false`; v1 ships
    /// `RECYCLE_AFTER_NAV = 1` (recreate every release) before optimising to
    /// reuse-with-clearing. See plan Phase 4. **Gated off when
    /// `chrome_backend = "browserless"`** — browserless v2's
    /// `Target.createBrowserContext` semantics with long-lived sessions are
    /// unproven; lib.rs forces this to `false` with a WARN log in that case.
    #[serde(default)]
    pub chrome_context_pool_enabled: bool,
    /// Per-knob pool configuration. Read only when
    /// `chrome_context_pool_enabled = true` AND backend is `Vanilla`.
    #[serde(default)]
    pub chrome_pool: ChromePoolConfig,
    /// Which Chrome backend the WS URL points at. **Explicit** — never sniff
    /// from URL substrings (k8s svc names, port-forwards, custom routes break
    /// substring detection per plan §C2). Default `Vanilla`.
    #[serde(default)]
    pub chrome_backend: ChromeBackend,
    /// Enable the success-ratio renderer predictor in `HostPreferences`.
    /// Default `false`; flipped after the predictor replay harness gates
    /// on the 1k bench (false-skip < 2 %, false-escalate < 5 %, churn < 3 / 1k).
    #[serde(default)]
    pub use_predictor: bool,
    /// Engine escalation policy (firecrawl-shaped: race + on-error). When
    /// disabled (default), the renderer keeps its current ladder unchanged.
    #[serde(default)]
    pub escalation: EscalationConfig,
    /// Anti-bot detection policy (crawl4ai 3-tier classifier).
    #[serde(default)]
    pub antibot: AntibotConfig,
    /// DataImpulse residential-proxy base username (without `__cr.<cc>`
    /// country suffix). When set alongside [`proxy_base_pass`], the engine
    /// drives Chrome's proxy auth via CDP `Fetch.authRequired` and composes
    /// the country-suffixed username per request. Read only by the
    /// `chrome_proxy` tier. None = no upstream proxy auth (chrome_proxy
    /// tier still functional only if a no-auth or pre-authed proxy is in
    /// front of Chrome).
    #[serde(default)]
    pub proxy_base_user: Option<String>,
    /// DataImpulse base password — see [`proxy_base_user`].
    #[serde(default)]
    pub proxy_base_pass: Option<String>,
    /// Fallback country code used when a request omits `country`. Lowercased
    /// 2-letter ISO 3166-1 alpha-2 (e.g. "us"). None = global pool (no suffix).
    #[serde(default)]
    pub proxy_default_country: Option<String>,
    /// Username format of the residential proxy behind `proxy_base_user`:
    /// `dataimpulse` (default) or `nodemaven`.
    #[serde(default)]
    pub proxy_username_format: ProxyUsernameFormat,
}

/// Engine escalation policy — adds `ChromeStealth` and `ChromeStealthProxy`
/// tiers behind a feature flag. See `plans/recall-next-tier.md` Phase 2.
#[derive(Debug, Clone, Deserialize)]
pub struct EscalationConfig {
    /// Master switch. Default `false` — current ladder runs unchanged.
    #[serde(default)]
    pub enabled: bool,
    /// Per-tier waterfall trigger in ms. If the current engine hasn't returned
    /// after this long, the next tier is started in parallel (firecrawl
    /// `WaterfallNextEngineSignal`).
    #[serde(default = "default_waterfall_timeout_ms")]
    pub waterfall_timeout_ms: u64,
    /// Hard global cap across the whole ladder.
    #[serde(default = "default_escalation_global_timeout_ms")]
    pub global_timeout_ms: u64,
    /// Send `?proxy=residential&proxyCountry=…` to browserless on the
    /// `ChromeStealthProxy` tier. Off by default — bears cost.
    #[serde(default)]
    pub residential_proxy: bool,
    /// Country code passed to browserless when `residential_proxy = true`.
    #[serde(default = "default_proxy_country")]
    pub proxy_country: String,
}

impl Default for EscalationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            waterfall_timeout_ms: default_waterfall_timeout_ms(),
            global_timeout_ms: default_escalation_global_timeout_ms(),
            residential_proxy: false,
            proxy_country: default_proxy_country(),
        }
    }
}

fn default_waterfall_timeout_ms() -> u64 {
    8_000
}
fn default_escalation_global_timeout_ms() -> u64 {
    60_000
}
fn default_proxy_country() -> String {
    "us".to_string()
}

/// Anti-bot classifier policy. Default: detect+log only; escalation requires
/// `escalate_on_signal = true` AND `escalation.enabled = true`.
#[derive(Debug, Clone, Deserialize)]
pub struct AntibotConfig {
    /// Run the classifier inside the renderer failover loop on every fetch
    /// result. Cheap; default on. NOTE: this gates only the in-loop classifier
    /// (see `crw-renderer`); the API-surface block verdict is classified
    /// separately and unconditionally at the scrape choke
    /// (`crw_crawl::single::classify_block`) and is not suppressed by this flag.
    /// To disable in-loop escalation, use `escalate_in_failover`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// When the classifier returns a non-`None` signal, advance to the next
    /// engine tier (requires `escalation.enabled`).
    #[serde(default)]
    pub escalate_on_signal: bool,
    /// When the classifier flags a block during the renderer failover loop,
    /// treat the result as a soft failure so the loop advances to the next
    /// tier — ending at `chrome_proxy` (residential). Default `true`. Set
    /// `false` to keep the classifier running (error_code + telemetry) while
    /// disabling in-loop escalation — the one-line kill switch.
    #[serde(default = "default_true")]
    pub escalate_in_failover: bool,
}

impl Default for AntibotConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            escalate_on_signal: false,
            escalate_in_failover: true,
        }
    }
}

fn default_chrome_nav_budget_ms() -> u64 {
    12_000
}

/// Per-knob configuration for the bounded browser-context pool. Loaded under
/// `[renderer.chrome_pool]`. Inactive unless
/// `chrome_context_pool_enabled = true` AND `chrome_backend = "vanilla"`.
#[derive(Debug, Clone, Deserialize)]
pub struct ChromePoolConfig {
    /// Pool size. `None` → `max(2, num_cpus / 2)`. Caps simultaneous
    /// in-flight chrome requests per pool.
    #[serde(default)]
    pub size: Option<usize>,
    /// Interactive render-slot reserve (the B lane), the 4th sibling of
    /// `reserved_interactive_{parses,extracts,llm}`. `None` → `pool/4` (the
    /// historical `render_reserve` default), so batch keeps `size - reserve`
    /// render slots and interactive is guaranteed `reserve`. Bounds one tenant's
    /// batch/crawl from squeezing other tenants' interactive scrapes below
    /// `reserve` Chrome slots. Resolved via `resolve_interactive_reserve` and
    /// clamped by `BatchGate::new` (never zeroes batch).
    #[serde(default)]
    pub reserved_interactive_renders: Option<usize>,
    /// Recycle policy: v1 always recreates the context after each release.
    /// Reserved for a future "reuse N navigations then recreate" mode.
    #[serde(default = "default_recycle_after_navs")]
    pub recycle_after_navs: u32,
    /// Idle slots older than this are health-checked on next acquire.
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    /// `Browser.getVersion` probe deadline (idle-slot liveness).
    #[serde(default = "default_health_check_secs")]
    pub health_check_secs: u64,
    /// SIGTERM drain window before phase 3 force-close.
    #[serde(default = "default_shutdown_drain_secs")]
    pub shutdown_drain_secs: u64,
}

impl Default for ChromePoolConfig {
    fn default() -> Self {
        Self {
            size: None,
            reserved_interactive_renders: None,
            recycle_after_navs: default_recycle_after_navs(),
            idle_timeout_secs: default_idle_timeout_secs(),
            health_check_secs: default_health_check_secs(),
            shutdown_drain_secs: default_shutdown_drain_secs(),
        }
    }
}

fn default_recycle_after_navs() -> u32 {
    1
}
fn default_idle_timeout_secs() -> u64 {
    300
}
fn default_health_check_secs() -> u64 {
    60
}
fn default_shutdown_drain_secs() -> u64 {
    30
}

/// Chrome backend kind. Set explicitly under `[renderer]` as
/// `chrome_backend = "vanilla"` or `chrome_backend = "browserless"`. **Never
/// inferred from URL substrings** — k8s service names, port-forwards, and
/// custom routes break substring detection. See plan §C2.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChromeBackend {
    /// chromedp/headless-shell or vanilla Chrome with `/json/version`. Pool
    /// is enabled here when `chrome_context_pool_enabled = true`.
    #[default]
    Vanilla,
    /// Browserless v2 / commercial CDP endpoint. Pool is **gated off** in v1
    /// — see plan §"Out of scope (v1)".
    Browserless,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            mode: RendererMode::default(),
            page_timeout_ms: default_page_timeout(),
            http_timeout_ms: None,
            lightpanda_timeout_ms: None,
            chrome_timeout_ms: None,
            pool_size: default_pool_size(),
            chrome_proxy_pool_size: None,
            cloak_pool_size: None,
            chrome_challenge_max_retries: None,
            chrome_spa_selector_max_ms: None,
            chrome_fast_ready: false,
            chrome_hedge: false,
            auto_egress_escalation: false,
            cloak_recover_on_cf: false,
            cloak_after_egress: false,
            latency_breakdown: false,
            render_js_default: None,
            lightpanda: None,
            playwright: None,
            chrome: None,
            chrome_proxy: None,
            chrome_proxy_timeout_ms: None,
            camoufox: None,
            camoufox_timeout_ms: None,
            camoufox_challenge_wait_ms: None,
            cloak: None,
            cloak_timeout_ms: None,
            cloak_proxy_host: None,
            impersonated: ImpersonatedConfig::default(),
            chrome_intercept_resources: false,
            chrome_intercept_stylesheets: false,
            chrome_host_intercept_disable: Vec::new(),
            chrome_nav_budget_ms: default_chrome_nav_budget_ms(),
            chrome_context_pool_enabled: false,
            chrome_pool: ChromePoolConfig::default(),
            chrome_backend: ChromeBackend::default(),
            use_predictor: false,
            escalation: EscalationConfig::default(),
            antibot: AntibotConfig::default(),
            proxy_base_user: None,
            proxy_base_pass: None,
            proxy_default_country: None,
            proxy_username_format: ProxyUsernameFormat::default(),
        }
    }
}
fn default_page_timeout() -> u64 {
    30000
}

impl RendererConfig {
    /// Resolved per-tier nav timeout in milliseconds. Resolution rules:
    ///   1. If the explicit per-tier field is set, use it verbatim.
    ///   2. Otherwise fall back to `page_timeout_ms` (which itself defaults
    ///      to 30s for backward compatibility with pre-multi-tier configs).
    ///
    /// New deployments are encouraged to set the per-tier knobs to 15/20/45s
    /// (see config.docker.toml) — these match the bench-tuned values that
    /// recover slow gov sites in the chrome tier without giving the http
    /// tier permission to hog the request budget.
    pub fn http_timeout(&self) -> u64 {
        self.http_timeout_ms.unwrap_or(self.page_timeout_ms)
    }
    pub fn lightpanda_timeout(&self) -> u64 {
        self.lightpanda_timeout_ms.unwrap_or(self.page_timeout_ms)
    }
    pub fn chrome_timeout(&self) -> u64 {
        self.chrome_timeout_ms.unwrap_or(self.page_timeout_ms)
    }
    pub fn chrome_proxy_timeout(&self) -> u64 {
        self.chrome_proxy_timeout_ms
            .unwrap_or_else(|| self.chrome_timeout().saturating_add(15_000))
    }
    pub fn chrome_proxy_pool_size(&self) -> usize {
        self.chrome_proxy_pool_size.unwrap_or(self.pool_size).max(1)
    }
    /// Cloak arm concurrency. Defaults to [`CLOAK_DEFAULT_POOL_SIZE`] rather
    /// than `pool_size`: the stealth tier is bounded by the sidecar's browsers,
    /// not by this engine's render pool, so it must not scale with it.
    pub fn cloak_pool_size(&self) -> usize {
        self.cloak_pool_size
            .unwrap_or(CLOAK_DEFAULT_POOL_SIZE)
            .max(1)
    }
    pub fn camoufox_timeout(&self) -> u64 {
        self.camoufox_timeout_ms
            .unwrap_or(CAMOUFOX_DEFAULT_TIMEOUT_MS)
    }
    /// Camoufox bot-challenge poll ceiling (ms). Unconditional (no `#[cfg]`),
    /// like [`Self::camoufox_timeout`].
    pub fn camoufox_challenge_wait(&self) -> u64 {
        self.camoufox_challenge_wait_ms
            .unwrap_or(CAMOUFOX_DEFAULT_CHALLENGE_WAIT_MS)
    }
    /// Per-attempt cloak solve budget (ms). Unconditional (no `#[cfg]`) so
    /// `tier_timeouts_from` can reference it in every build, like
    /// [`Self::camoufox_timeout`].
    pub fn cloak_timeout(&self) -> u64 {
        self.cloak_timeout_ms.unwrap_or(CLOAK_DEFAULT_TIMEOUT_MS)
    }

    /// True when the cloak REST tier participates in the *auto* ladder for the
    /// current mode. Mirrors [`Self::camoufox_in_ladder`] exactly, including the
    /// leading `!cfg!(feature = "cloak")` short-circuit (compiled to a constant
    /// and dead-code-eliminated in a lean build) that keeps the unconditional
    /// `RendererMode::Cloak` variant inert without the feature. Do NOT remove
    /// that early return. (The cloak tier is normally a recovery arm, not a
    /// ladder tier, so this is `true` only when explicitly pinned or opted in.)
    pub fn cloak_in_ladder(&self) -> bool {
        if !cfg!(feature = "cloak") || matches!(self.mode, RendererMode::None) {
            return false;
        }
        let configured = |c: &CloakEndpoint| !c.base_url.trim().is_empty();
        match self.mode {
            RendererMode::Cloak => self.cloak.as_ref().is_some_and(configured),
            RendererMode::Auto => self
                .cloak
                .as_ref()
                .is_some_and(|c| c.include_in_auto && configured(c)),
            _ => false,
        }
    }

    /// True when the post-ladder cloak CF-recovery arm can fire: the `cloak`
    /// feature is built in, a cloak endpoint is configured, and
    /// [`RendererConfig::cloak_recover_on_cf`] is on. Unlike [`cloak_in_ladder`],
    /// this is independent of `include_in_auto` — the recovery arm lives OUTSIDE
    /// the ladder and fires on a fresh [`CLOAK_ARM_RECOVER_BUDGET_MS`] budget, so
    /// the outer request-timeout envelope must reserve room for it even though it
    /// is not a ladder tier. `false` in the lean (no-`cloak`) build.
    pub fn cloak_recovery_active(&self) -> bool {
        cfg!(feature = "cloak")
            && self.cloak_recover_on_cf
            && !matches!(self.mode, RendererMode::None)
            && self
                .cloak
                .as_ref()
                .is_some_and(|c| !c.base_url.trim().is_empty())
    }

    /// True when the Camoufox REST tier participates in the *auto* ladder for
    /// the current mode — i.e. it would be tried for a non-pinned request.
    /// Distinct from merely "configured": a configured endpoint with
    /// `include_in_auto = false` returns `false` here unless `mode == Camoufox`.
    ///
    /// | mode                              | result                          |
    /// |-----------------------------------|---------------------------------|
    /// | `None`                            | `false` (no renderers at all)   |
    /// | `Camoufox`                        | `true` when configured (pinned) |
    /// | `Auto` + `include_in_auto = true` | `true`                          |
    /// | `Auto` + `include_in_auto = false`| `false` (opt-in default)        |
    /// | `Lightpanda`/`Chrome`/`Playwright`| `false`                         |
    ///
    /// This method is intentionally NOT `#[cfg(feature = "camoufox")]`: callers
    /// in the deadline math (`min_deadline_for_full_ladder_ms`,
    /// `effective_deadline_ms`) reference it unconditionally. The leading
    /// `cfg!(feature = "camoufox")` runtime check (compiled to a constant and
    /// dead-code-eliminated by LLVM in the lean build) makes it always return
    /// `false` without the feature, so HTTP-only builds stay byte-identical.
    /// Do not remove that early return thinking it is redundant — it is what
    /// keeps `RendererMode::Camoufox` (an unconditional enum variant) inert in
    /// lean builds.
    pub fn camoufox_in_ladder(&self) -> bool {
        if !cfg!(feature = "camoufox") || matches!(self.mode, RendererMode::None) {
            return false;
        }
        // Mirror the construction filter in `FallbackRenderer::new`, which only
        // builds the tier when `base_url` is non-empty. Without this guard a
        // degenerate config (blank `base_url` + `include_in_auto = true`) would
        // claim ladder membership for a tier that is never constructed, leaking
        // a phantom +camoufox_timeout into the deadline budget.
        let configured = |c: &CamoufoxEndpoint| !c.base_url.trim().is_empty();
        match self.mode {
            RendererMode::Camoufox => self.camoufox.as_ref().is_some_and(configured),
            RendererMode::Auto => self
                .camoufox
                .as_ref()
                .is_some_and(|c| c.include_in_auto && configured(c)),
            _ => false,
        }
    }

    /// True when the Chrome-impersonation HTTP tier (wreq) participates in the
    /// fetch chain. Deliberately NOT gated on mode: the tier is an HTTP tier,
    /// and `mode = "none"` means "no JS", not "no fetching strategies", so a
    /// `none` deployment keeps it. Always false in a build without the
    /// `impersonated` feature, keeping lean builds byte-identical. Do not
    /// remove the leading `cfg!` short-circuit; it is what keeps the
    /// default-on runtime flag inert without the feature.
    pub fn impersonated_in_chain(&self) -> bool {
        cfg!(feature = "impersonated") && self.impersonated.enabled
    }

    /// Per-request budget (ms) for the impersonated tier. Falls back to the
    /// HTTP tier timeout: the tier is shaped like the HTTP tier and its
    /// failures are bounded the same way.
    pub fn impersonated_timeout(&self) -> u64 {
        self.impersonated
            .timeout_ms
            .unwrap_or_else(|| self.http_timeout())
    }

    /// Compose the DataImpulse-style proxy credentials for a single request.
    ///
    /// Resolution order for the country suffix:
    /// 1. `country` argument (per-request override)
    /// 2. `self.proxy_default_country` (server default)
    /// 3. No suffix → DataImpulse global pool
    ///
    /// Returns `None` when no base credentials are configured — caller treats
    /// this as "no auth required". An invalid country code (wrong length,
    /// non-alphabetic) silently falls through to the default; that keeps a
    /// malformed `?country=` query from creating an unauthenticated request
    /// while still letting through a well-known default.
    pub fn effective_proxy_credentials(&self, country: Option<&str>) -> Option<(String, String)> {
        let user = self.proxy_base_user.as_ref()?;
        let pass = self.proxy_base_pass.as_ref()?;
        let cc = normalize_proxy_country(country.or(self.proxy_default_country.as_deref()));
        Some((
            self.proxy_username_format
                .compose(user, cc.as_deref(), None),
            pass.clone(),
        ))
    }

    /// Number of active CDP tiers (lightpanda + playwright + chrome) under
    /// the current `mode`. Mirrors the predicate used at runtime in
    /// `crw-renderer/src/lib.rs` when constructing the renderer ladder:
    /// `want(mode) && config.<tier>.is_some()`.
    ///
    /// Returns `0` when the binary is built without the `cdp` feature — in
    /// that case no JS renderer can be constructed regardless of the config,
    /// so the deadline auto-extension policy must collapse to HTTP-only.
    pub fn cdp_tier_count(&self) -> usize {
        if !cfg!(feature = "cdp") {
            return 0;
        }
        let want =
            |m: RendererMode| -> bool { matches!(self.mode, RendererMode::Auto) || self.mode == m };
        let mut n = 0;
        if want(RendererMode::Lightpanda) && self.lightpanda.is_some() {
            n += 1;
        }
        if want(RendererMode::Playwright) && self.playwright.is_some() {
            n += 1;
        }
        if want(RendererMode::Chrome) && self.chrome.is_some() {
            n += 1;
        }
        n
    }

    /// Minimum request deadline budget (ms) required so that every configured
    /// tier can use its full allowance when fallback exhausts the chain.
    /// Sums the per-tier timeouts and adds [`CDP_TIER_OVERHEAD_MS`] for each
    /// active CDP tier, matching the runtime ladder built in
    /// `crw-renderer/src/lib.rs`.
    pub fn min_deadline_for_full_ladder_ms(&self) -> u64 {
        let want =
            |m: RendererMode| -> bool { matches!(self.mode, RendererMode::Auto) || self.mode == m };

        let mut sum: u64 = 0;
        // HTTP prefetch runs ahead of any JS tier (content-type sniffing,
        // direct PDF/binary handling) regardless of pinned mode. Skipped only
        // when mode is `None` (no fetching at all).
        if !matches!(self.mode, RendererMode::None) {
            sum = sum.saturating_add(self.http_timeout());
        }

        // Chrome-impersonation HTTP tier contribution. A bounded single HTTP
        // request (no CDP overhead), fired between the plain HTTP fetch and the
        // browser ladder. `impersonated_in_chain()` is always `false` in the
        // lean build, so this line is inert there.
        if self.impersonated_in_chain() {
            sum = sum.saturating_add(self.impersonated_timeout());
        }

        // Camoufox REST contribution. Added BEFORE the cdp early-return below so
        // an HTTP-only + camoufox-enabled build still extends the deadline.
        // Camoufox is a single REST tier: it is never counted in
        // `cdp_tier_count` and never charged `CDP_TIER_OVERHEAD_MS`.
        // `camoufox_in_ladder()` is always `false` in the lean build, so this
        // line is inert there.
        if self.camoufox_in_ladder() {
            sum = sum.saturating_add(self.camoufox_timeout());
        }

        // Cloak REST contribution, symmetric to camoufox. `cloak_in_ladder()` is
        // always `false` in the lean build, so this line is inert there. (The
        // cloak tier is normally a recovery arm, not a ladder tier, so this only
        // contributes when explicitly pinned / opted into the auto ladder.)
        if self.cloak_in_ladder() {
            sum = sum.saturating_add(self.cloak_timeout());
        } else if self.cloak_recovery_active() {
            // The post-ladder cloak CF-recovery arm fires on a fresh
            // `CLOAK_ARM_RECOVER_BUDGET_MS` budget (not a ladder tier), so the
            // outer request-timeout envelope must reserve room for it — otherwise
            // the static Tower timeout, sized only from the ladder sum, can abort
            // a cloak solve mid-flight (wasting the sidecar slot + proxy egress and
            // skipping the breaker-outcome record). Only reserved when the
            // recovery flag is on, so it is inert by default.
            // Mirrors the renderer: the fresh arm budget is the larger of the
            // constant and the configured cloak tier timeout.
            sum = sum.saturating_add(CLOAK_ARM_RECOVER_BUDGET_MS.max(self.cloak_timeout()));
        }

        // CDP tiers only contribute when the binary was built with the `cdp`
        // feature; otherwise no JS renderer is constructable at runtime and
        // including their budgets would over-extend the deadline.
        if !cfg!(feature = "cdp") {
            return sum;
        }

        let mut cdp_tier_count: u64 = 0;
        if want(RendererMode::Lightpanda) && self.lightpanda.is_some() {
            sum = sum.saturating_add(self.lightpanda_timeout());
            cdp_tier_count += 1;
        }
        if want(RendererMode::Playwright) && self.playwright.is_some() {
            sum = sum.saturating_add(self.chrome_timeout());
            cdp_tier_count += 1;
        }
        if want(RendererMode::Chrome) && self.chrome.is_some() {
            sum = sum.saturating_add(self.chrome_timeout());
            cdp_tier_count += 1;
        }
        sum.saturating_add(cdp_tier_count.saturating_mul(CDP_TIER_OVERHEAD_MS))
    }
}
fn default_pool_size() -> usize {
    4
}

#[derive(Debug, Clone, Deserialize)]
pub struct CdpEndpoint {
    pub ws_url: String,
}

/// Opt-in Camoufox stealth renderer endpoint (REST, not CDP). When present it
/// is selectable via `mode = "camoufox"` or a per-request `renderer =
/// "camoufox"` pin, and additionally joins the Auto ladder ONLY when
/// `include_in_auto = true`. A configured endpoint with the default
/// `include_in_auto = false` does NOT change the existing auto ladder.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CamoufoxEndpoint {
    /// Base URL of the camofox-browser REST server, e.g. `http://localhost:9377`.
    pub base_url: String,
    /// Optional bearer token sent as `Authorization: Bearer <key>`. Empty string
    /// (the default) means no auth header is added.
    #[serde(default)]
    pub api_key: String,
    /// Whether this tier joins the Auto fallback ladder. Default `false`:
    /// configured-but-not-in-auto, reachable only via an explicit pin or
    /// `mode = "camoufox"`.
    #[serde(default)]
    pub include_in_auto: bool,
}

/// In-process Chrome-impersonation HTTP tier (wreq), loaded under
/// `[renderer.impersonated]`. Unlike every other tier there is no endpoint;
/// `enabled` is the runtime kill switch and defaults TRUE. Non-Option on
/// purpose: an absent section means "on with defaults", which is the
/// deployment contract. The whole section is inert in a build without the
/// `impersonated` cargo feature (`impersonated_in_chain()` folds to false).
#[derive(Debug, Clone, Deserialize)]
pub struct ImpersonatedConfig {
    /// Runtime kill switch. Default true: the tier is ON in any build
    /// compiled with the `impersonated` feature.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Per-request timeout override (ms). Falls back to the HTTP tier
    /// timeout: the tier is shaped like the HTTP tier and its failures are
    /// bounded the same way.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl Default for ImpersonatedConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            timeout_ms: None,
        }
    }
}

/// Endpoint for the "cloak" Turnstile-solver sidecar (a `cloudflarebypassforscraping`
/// / CloakBrowser REST server). Mirrors [`CamoufoxEndpoint`]. Absent config
/// (`cloak = None`) means the tier is never constructed — see
/// [`RendererConfig::cloak_in_ladder`]. The cloak tier is a CF-challenge recovery
/// arm, not an auto-ladder tier, so `include_in_auto` is normally left `false`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CloakEndpoint {
    /// Base URL of the cloak sidecar's mirror endpoint, e.g. `http://cloak-sidecar:8000`.
    pub base_url: String,
    /// Optional bearer token sent as `Authorization: Bearer <key>`. Empty = no auth.
    #[serde(default)]
    pub api_key: String,
    /// Whether this tier joins the Auto fallback ladder. Default `false`.
    #[serde(default)]
    pub include_in_auto: bool,
}

/// Stealth mode configuration for evading bot detection.
#[derive(Debug, Clone, Deserialize)]
pub struct StealthConfig {
    /// Enable stealth mode globally.
    #[serde(default)]
    pub enabled: bool,
    /// Custom user-agent pool. Empty = use built-in pool.
    #[serde(default)]
    pub user_agents: Vec<String>,
    /// Jitter factor for rate limiting (0.0–1.0, default 0.2 = ±20%).
    #[serde(default = "default_jitter")]
    pub jitter_factor: f64,
    /// Inject realistic browser headers (Accept, Sec-Fetch-*, etc.).
    #[serde(default = "default_true")]
    pub inject_headers: bool,
}

impl Default for StealthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            user_agents: vec![],
            jitter_factor: default_jitter(),
            inject_headers: true,
        }
    }
}

fn default_jitter() -> f64 {
    0.2
}

/// Built-in realistic user-agent pool used when stealth is enabled.
pub const BUILTIN_UA_POOL: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:140.0) Gecko/20100101 Firefox/140.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_5) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Safari/605.1.15",
];

#[derive(Debug, Clone, Deserialize)]
pub struct CrawlerConfig {
    #[serde(default = "default_concurrency")]
    pub max_concurrency: usize,
    #[serde(default = "default_rps")]
    pub requests_per_second: f64,
    #[serde(default = "default_true")]
    pub respect_robots_txt: bool,
    #[serde(default = "default_ua")]
    pub user_agent: String,
    #[serde(default = "default_depth")]
    pub default_max_depth: u32,
    #[serde(default = "default_max_pages")]
    pub default_max_pages: u32,
    /// Proxy URL for crawler requests. Supports HTTP, HTTPS, and SOCKS5
    /// (e.g. "http://proxy:8080" or "socks5://user:pass@proxy:1080"). An empty
    /// or whitespace-only value (e.g. a present-but-empty `CRW_CRAWLER__PROXY`)
    /// is normalized to `None` — see [`deserialize_opt_nonempty_string`].
    #[serde(default, deserialize_with = "deserialize_opt_nonempty_string")]
    pub proxy: Option<String>,
    /// Pool of proxy URLs to rotate among (HTTP, HTTPS, SOCKS5). When non-empty
    /// this takes precedence over the single `proxy` field. Empty (default) =
    /// no rotation. Accepts a TOML array, a JSON-array string, or a
    /// comma-separated string (for `CRW_CRAWLER__PROXY_LIST`).
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    pub proxy_list: Vec<String>,
    /// Strategy for selecting from `proxy_list`: `round_robin`, `random`, or
    /// `sticky_per_host` (default). Ignored when the list is empty.
    #[serde(default)]
    pub proxy_rotation: crate::proxy::ProxyRotation,
    /// TTL in seconds for completed crawl jobs before cleanup (default: 3600)
    #[serde(default = "default_job_ttl")]
    pub job_ttl_secs: u64,
    #[serde(default)]
    pub stealth: StealthConfig,
    /// Floor for the per-host limiter interval, in milliseconds. When a host
    /// advertises `Crawl-delay` in robots.txt, the higher of the two wins.
    /// Default `0` — robots.txt is the authoritative source, this is a
    /// per-deployment safety net.
    #[serde(default)]
    pub per_host_min_interval_ms: u64,
    /// Maximum concurrent in-flight requests against a single eTLD+1.
    /// Default `1` — strict ethics posture; operators raise consciously via
    /// config when scraping their own infrastructure.
    #[serde(default = "default_per_host_max_concurrent")]
    pub per_host_max_concurrent: u32,
    /// Extra per-host in-flight slots RESERVED for interactive (single-scrape)
    /// traffic, on top of `per_host_max_concurrent`. Batch/crawl stay bounded to
    /// `per_host_max_concurrent` per host (target politeness), while an
    /// interactive hit gets its own dedicated slot so it isn't queued behind a
    /// batch crawling that host. Total in-flight per host is therefore bounded by
    /// `per_host_max_concurrent + this` (the target is still protected — no
    /// unbounded hammering). `0` = interactive shares the batch slots (legacy
    /// single-flight). Default 1.
    #[serde(default = "default_per_host_interactive_reserve")]
    pub per_host_interactive_reserve: u32,
    /// Maximum number of URLs accepted by a single `/v2/batch/scrape`
    /// request (sanity cap; plan-level caps live in the SaaS). Default 10000.
    #[serde(default = "default_max_batch_urls")]
    pub max_batch_urls: usize,
    /// Maximum number of URLs accepted by a single `/v1/extract` request. Much
    /// tighter than `max_batch_urls` because each URL triggers an LLM call.
    /// Default 50.
    #[serde(default = "default_max_extract_urls")]
    pub max_extract_urls: usize,
    /// Ceiling on a single batch job's OUTER pipeline width (`for_each_concurrent`
    /// = how many URL tasks are alive at once). A per-request `maxConcurrency`
    /// (injected by the SaaS per plan tier) is clamped to `[1, this]`; absent →
    /// `max_concurrency`. This is the OUTER width — it may exceed the inner
    /// reserved lanes (extract/render/…), extra URL tasks just queue there.
    /// Distinct from `max_batch_urls` (submit-size cap). Default 100.
    #[serde(default = "default_max_batch_concurrency")]
    pub max_batch_concurrency: usize,
    /// Process-wide cap on the TOTAL number of in-flight `/v2/batch/scrape`
    /// URL-pipelines across all batch-scrape jobs, so `N jobs × width` can't
    /// explode into memory / socket pressure. (Batch scrape is the only wide
    /// fan-out; crawl is BFS-sequential and `/v2/extract` is one URL at a time,
    /// both bounded by the crawl-job cap.) Each live pipeline takes one permit
    /// before it fetches. **`0` (or absent) means UNBOUNDED** — NOT a lock. Do NOT set `0`
    /// to "disable by zero" expecting a small number; `0` skips the cap entirely.
    /// This is the opposite convention from the reserved-lane `reserved_* = 0`
    /// (which means "no reservation"). Default 0 (unbounded).
    #[serde(default)]
    pub max_aggregate_batch_pipelines: usize,
}

fn default_per_host_max_concurrent() -> u32 {
    1
}

fn default_per_host_interactive_reserve() -> u32 {
    1
}

impl Default for CrawlerConfig {
    fn default() -> Self {
        Self {
            max_concurrency: default_concurrency(),
            requests_per_second: default_rps(),
            respect_robots_txt: true,
            user_agent: default_ua(),
            default_max_depth: default_depth(),
            default_max_pages: default_max_pages(),
            proxy: None,
            proxy_list: Vec::new(),
            proxy_rotation: crate::proxy::ProxyRotation::default(),
            job_ttl_secs: default_job_ttl(),
            stealth: StealthConfig::default(),
            per_host_min_interval_ms: 0,
            per_host_max_concurrent: default_per_host_max_concurrent(),
            per_host_interactive_reserve: default_per_host_interactive_reserve(),
            max_batch_urls: default_max_batch_urls(),
            max_extract_urls: default_max_extract_urls(),
            max_batch_concurrency: default_max_batch_concurrency(),
            max_aggregate_batch_pipelines: 0,
        }
    }
}

fn default_max_batch_urls() -> usize {
    10_000
}

fn default_max_extract_urls() -> usize {
    50
}

fn default_max_batch_concurrency() -> usize {
    100
}

fn default_concurrency() -> usize {
    10
}
fn default_rps() -> f64 {
    10.0
}
fn default_true() -> bool {
    true
}
fn default_ua() -> String {
    // Modern Chrome UA. The legacy "CRW/0.1" was rejected by UA-filtering sites
    // (opencorporates, killeenisd, wsj) returning 403/404. Kept in sync with the
    // Sec-Ch-Ua client hint in `crw-renderer/src/http_only.rs`.
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36"
        .into()
}
fn default_depth() -> u32 {
    2
}
fn default_max_pages() -> u32 {
    100
}
fn default_job_ttl() -> u64 {
    3600
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractionConfig {
    #[serde(default = "default_format")]
    pub default_format: String,
    #[serde(default = "default_true_ext")]
    pub only_main_content: bool,
    #[serde(default)]
    pub llm: Option<LlmConfig>,
    /// Hostname → CSS selector overrides applied before readability narrowing.
    /// Match is exact host (no wildcard); user-supplied selector still wins.
    #[serde(default)]
    pub domain_selectors: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub llm_fallback: LlmFallbackConfig,
    /// Bytes below which an HTTP-tier extraction is treated as "thin"
    /// and triggers a JS-renderer escalation. Default 100.
    #[serde(default = "default_http_retry_threshold")]
    pub http_retry_threshold_bytes: usize,
    /// Bytes below which a LightPanda-tier extraction is treated as
    /// "thin" and triggers a Chrome escalation. Default 2000 (LP often
    /// returns SPA husks of 90–500B that pass HTML-shape checks).
    #[serde(default = "default_lightpanda_retry_threshold")]
    pub lightpanda_retry_threshold_bytes: usize,
    /// Process-wide cap on concurrent HTML → markdown extractions (html5ever +
    /// htmd). Extraction is CPU-bound and runs on the blocking pool; this bound
    /// keeps a burst of concurrent scrapes from oversubscribing the cores and
    /// starving the async reactor. Defaults to ~2/3 of available cores (≈8 on a
    /// 12-vCPU host), floored at 2.
    #[serde(default = "default_max_concurrent_extracts")]
    pub max_concurrent_extracts: usize,
    /// Of `max_concurrent_extracts`, how many extract slots are RESERVED for
    /// interactive (single-scrape) traffic — a batch/crawl job can never hold
    /// them, so an interactive scrape isn't starved behind a batch that fills
    /// the pool. Clamped so the batch lane keeps ≥1 slot. `Some(0)` disables the
    /// reservation (single FIFO lane, legacy behaviour). `None` (absent) →
    /// ~1/4 of the ACTUAL configured pool, floored at 1 (so a custom
    /// `max_concurrent_extracts` scales the reserve with it).
    #[serde(default)]
    pub reserved_interactive_extracts: Option<usize>,
    /// Normalize HTML tables (span expansion, header synthesis, headerless-table promotion) before
    /// htmd conversion, and stop the indented-code pass from swallowing list-nested tables. Changes
    /// markdown bytes for any page containing a <table>: this flips /monitor's content hash and can
    /// reorder the extract alternates ladder on borderline pages. Default false; A/B in prod.
    #[serde(default)]
    pub normalize_tables: bool,
}

fn default_http_retry_threshold() -> usize {
    100
}

fn default_lightpanda_retry_threshold() -> usize {
    2000
}

fn default_max_concurrent_extracts() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (cpus * 2 / 3).max(2)
}

impl Default for ExtractionConfig {
    fn default() -> Self {
        Self {
            default_format: default_format(),
            only_main_content: true,
            llm: None,
            domain_selectors: std::collections::HashMap::new(),
            llm_fallback: LlmFallbackConfig::default(),
            http_retry_threshold_bytes: default_http_retry_threshold(),
            lightpanda_retry_threshold_bytes: default_lightpanda_retry_threshold(),
            max_concurrent_extracts: default_max_concurrent_extracts(),
            reserved_interactive_extracts: None,
            normalize_tables: false,
        }
    }
}

/// Resolve an optional interactive reserve against the actual pool `total`:
/// `None` → ~1/4 of `total` floored at 1; `Some(n)` → `n` (0 disables). Kept
/// below `total` by the [`crate::ReservedSemaphore`] constructor.
pub fn resolve_interactive_reserve(reserve: Option<usize>, total: usize) -> usize {
    reserve.unwrap_or_else(|| (total / 4).max(1))
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmFallbackConfig {
    #[serde(default)]
    pub enable: bool,
    #[serde(default = "default_llm_quality_threshold")]
    pub quality_threshold: f32,
    #[serde(default = "default_llm_max_html_bytes")]
    pub max_html_bytes: usize,
    /// When true (and `enable` is true), invoke the LLM on every page rather
    /// than only when DOM-based extraction scores below `quality_threshold`.
    /// Mirrors the "LLM as primary extractor" pattern used by Reader-LM,
    /// Firecrawl, and similar services. Higher cost, higher recall.
    #[serde(default)]
    pub always_run: bool,
}

impl Default for LlmFallbackConfig {
    fn default() -> Self {
        Self {
            enable: false,
            quality_threshold: default_llm_quality_threshold(),
            max_html_bytes: default_llm_max_html_bytes(),
            always_run: false,
        }
    }
}

fn default_llm_quality_threshold() -> f32 {
    0.3
}
fn default_llm_max_html_bytes() -> usize {
    100_000
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    #[serde(default = "default_llm_provider")]
    pub provider: String,
    pub api_key: String,
    #[serde(default = "default_llm_model")]
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default = "default_llm_max_tokens")]
    pub max_tokens: u32,
    /// Azure OpenAI API version (e.g. "2024-05-01-preview"). Required when
    /// `provider = "azure"`; ignored otherwise.
    #[serde(default)]
    pub azure_api_version: Option<String>,
    /// Max parallel LLM calls for fan-out (e.g. per-result search summaries).
    /// Bounded to avoid hitting provider rate limits.
    #[serde(default = "default_llm_max_concurrency")]
    pub max_concurrency: usize,
    /// Of `max_concurrency`, how many LLM-call slots are RESERVED for interactive
    /// (single-scrape) traffic — a batch/crawl job can never hold them, so an
    /// interactive `formats:["json"]`/summary request isn't starved behind a
    /// batch's LLM fan-out. Clamped so the batch lane keeps ≥1 slot. `Some(0)`
    /// disables the reservation. `None` (absent) → `max(1, max_concurrency/4)`
    /// of the ACTUAL configured `max_concurrency`.
    #[serde(default)]
    pub reserved_interactive_llm: Option<usize>,
    /// Byte cap on content sent to the LLM in a single call. Content beyond
    /// the cap is truncated on a UTF-8 char boundary.
    #[serde(default = "default_llm_max_html_bytes")]
    pub max_html_bytes: usize,
    /// When set, opencore refuses LLM-touching requests that lack this header
    /// AND do not supply `llm_api_key` in the body. SaaS deploys set this so
    /// direct public callers can't access LLM features.
    #[serde(default)]
    pub require_byok_header: Option<String>,
    /// Sampling temperature for the LLM call. `None` (default) sends no
    /// `temperature` key, preserving each provider's default (DeepSeek = 1) and
    /// current prod behavior. The benchmark/eval harness sets `0.0` (with a
    /// seed) to make answers deterministic so a real +2-3pp lever is
    /// distinguishable from sampling noise. Prod stays `None` until temp=0 is
    /// proven not to raise abstention.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Optional reasoning-effort hint forwarded to OpenAI-compatible providers
    /// (`reasoning_effort`) and Responses-compatible providers
    /// (`reasoning.effort`). `None` (default) and the empty string send no key,
    /// preserving each provider's default behavior.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: default_llm_provider(),
            api_key: String::new(),
            model: default_llm_model(),
            base_url: None,
            max_tokens: default_llm_max_tokens(),
            azure_api_version: None,
            max_concurrency: default_llm_max_concurrency(),
            reserved_interactive_llm: None,
            max_html_bytes: default_llm_max_html_bytes(),
            require_byok_header: None,
            temperature: None,
            reasoning_effort: None,
        }
    }
}

fn default_llm_max_concurrency() -> usize {
    4
}

fn default_llm_provider() -> String {
    "anthropic".into()
}
fn default_llm_model() -> String {
    "claude-sonnet-4-20250514".into()
}
fn default_llm_max_tokens() -> u32 {
    4096
}

fn default_format() -> String {
    "markdown".into()
}
fn default_true_ext() -> bool {
    true
}

/// Custom deserializer for Vec<String> that accepts:
/// - TOML array: `api_keys = ["key1", "key2"]`
/// - JSON array: `["key1", "key2"]` (for env vars)
/// - Comma-separated: `key1,key2` (for simple env var usage)
fn deserialize_string_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Vec(Vec<String>),
        Str(String),
    }

    match StringOrVec::deserialize(deserializer)? {
        StringOrVec::Vec(v) => Ok(v),
        StringOrVec::Str(s) => {
            let s = s.trim();
            // Try JSON array first
            if s.starts_with('[') {
                serde_json::from_str(s).map_err(serde::de::Error::custom)
            } else {
                // Comma-separated fallback
                Ok(s.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect())
            }
        }
    }
}

/// Deserializer for an optional string that normalizes an empty or
/// whitespace-only value to `None`.
///
/// Env-based config (the `config` crate with `try_parsing`) surfaces a
/// present-but-empty variable such as `CRW_CRAWLER__PROXY=""` as `Some("")`
/// rather than `None`. Left as `Some("")`, that empty string flows into
/// `reqwest::Proxy::all("")`, which rejects it with "builder error" and breaks
/// the map/crawl discovery path (issue #154). This mirrors how
/// [`deserialize_string_vec`] already drops empty entries for `proxy_list`.
///
/// Applied via `#[serde(default, deserialize_with = ...)]`, so a *missing* key
/// is handled by `Default` (the helper never runs) and only a present value —
/// from env or a TOML `proxy = ""` — reaches this function.
fn deserialize_opt_nonempty_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    let trimmed = s.trim();
    Ok(if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthConfig {
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    pub api_keys: Vec<String>,
}

/// Path of the per-user config file written by `crw setup`. Returns `None` if
/// the home directory cannot be resolved (e.g. headless container with no
/// `$HOME`). Honors `$CRW_USER_CONFIG_DIR` for tests so we don't have to
/// monkey-patch `$HOME`.
pub fn user_config_path() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("CRW_USER_CONFIG_DIR") {
        return Some(std::path::PathBuf::from(dir).join("config.toml"));
    }
    let home = std::env::var_os("HOME")?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config")
            .join("crw")
            .join("config.toml"),
    )
}

fn non_empty_trimmed_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

impl AppConfig {
    /// Load config from config.default.toml + per-user config + environment
    /// variable overrides.
    ///
    /// Precedence (highest wins):
    ///   1. `CRW_*` env vars (CI/Docker), including the public
    ///      `CRW_API_URL` / `CRW_API_KEY` client aliases
    ///   2. `$CRW_CONFIG` file (or `config.local.toml` in cwd)
    ///   3. `~/.config/crw/config.toml` (written by `crw setup`)
    ///   4. `config.default.toml` (bundled defaults)
    ///
    /// Env stays on top so a one-off `CRW_FOO=bar crw …` always wins over
    /// whatever the user has saved, matching how every other shell tool works.
    pub fn load() -> Result<Self, config::ConfigError> {
        let mut builder = config::Config::builder()
            .add_source(config::File::with_name("config.default").required(false));

        // User-level config — written atomically by `crw setup`. Optional, so
        // a never-configured machine simply reads defaults + env.
        if let Some(user_cfg) = user_config_path()
            && user_cfg.exists()
        {
            builder = builder.add_source(config::File::from(user_cfg).required(false));
        }

        // Load optional override config file (e.g. config.docker.toml in containers).
        if let Ok(extra) = std::env::var("CRW_CONFIG") {
            builder = builder.add_source(config::File::with_name(&extra).required(true));
        } else {
            builder = builder.add_source(config::File::with_name("config.local").required(false));
        }

        let cfg = builder
            .add_source(
                config::Environment::with_prefix("CRW")
                    .prefix_separator("_")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?;
        let mut app: Self = cfg.try_deserialize()?;

        // `CRW_API_URL` / `CRW_API_KEY` are the public CLI, SDK and MCP
        // variables. The generic nested-config mapper above naturally maps
        // `CRW_CLIENT__API_URL` / `CRW_CLIENT__API_KEY`, so apply the public
        // aliases explicitly at the same highest-precedence environment
        // layer. Keeping this in the one resolver prevents doctor, smoke,
        // search, MCP and future commands from disagreeing about the target.
        if let Some(api_url) = non_empty_trimmed_env("CRW_API_URL") {
            app.client.api_url = Some(api_url);
        }
        if let Some(api_key) = non_empty_trimmed_env("CRW_API_KEY") {
            app.client.api_key = Some(api_key);
        }

        Ok(app)
    }

    /// Compute the effective end-to-end request deadline (ms). Implements the
    /// issue-#35 auto-extension policy:
    ///
    /// 1. If the caller supplied an explicit `requested_deadline_ms`, return it
    ///    verbatim — operators trust the request budget over our heuristic.
    /// 2. Otherwise, when `request.auto_extend_deadline_for_ladder` is on,
    ///    return `max(deadline_ms_default, ladder_min + wait_for_extra)`.
    ///    `ladder_min` covers the configured tier ladder; `wait_for_extra`
    ///    compensates for callers that bumped `wait_for_ms` above the default
    ///    SPA budget (8s) — without it, a long `wait_for` would silently
    ///    re-clamp inside CDP.
    /// 3. When the policy is disabled, return `deadline_ms_default` unchanged.
    ///
    /// `wait_for_ms` is the per-request override (ScrapeRequest::wait_for /
    /// CrawlRequest::wait_for); pass `None` for sub-fetches that don't
    /// surface a wait_for to the caller (search/map enrichment).
    pub fn effective_deadline_ms(
        &self,
        requested_deadline_ms: Option<u64>,
        wait_for_ms: Option<u64>,
    ) -> u64 {
        if let Some(explicit) = requested_deadline_ms {
            return explicit;
        }
        let default_ms = self.request.deadline_ms_default;
        if !self.request.auto_extend_deadline_for_ladder {
            return default_ms;
        }
        // Issue #35 is specifically about CDP tier overhead silently clamping
        // chrome_timeout_ms. HTTP-only deployments don't suffer the same
        // problem (the HTTP renderer respects deadline.remaining without the
        // extra fetch/challenge/stability overhead). Skip the extension when
        // no CDP tiers are configured so HTTP-only users keep the strict
        // operator-configured default.
        //
        // The opt-in Camoufox REST tier also warrants the extension when it is
        // in the ladder (e.g. a camoufox-only, no-CDP deployment) — otherwise
        // its 60s budget would be clamped to the strict default and starved.
        // `camoufox_in_ladder()` is always `false` in the lean build, so
        // HTTP-only deployments keep byte-identical behaviour here.
        if self.renderer.cdp_tier_count() == 0
            && !self.renderer.camoufox_in_ladder()
            && !self.renderer.cloak_in_ladder()
        {
            return default_ms;
        }
        let ladder_min = self.renderer.min_deadline_for_full_ladder_ms();
        // Mirrors crw_renderer::cdp::SPA_SELECTOR_MAX_MS. The CDP module
        // adds `wait_for_ms.unwrap_or(SPA_SELECTOR_MAX_MS)` to its internal
        // timeout, so when the caller exceeds the default we need to extend
        // the deadline per active CDP tier.
        const SPA_DEFAULT_MS: u64 = 8_000;
        // Clamp `wait_for_ms` to MAX_WAIT_FOR_MS so the inner deadline never
        // exceeds the Tower envelope, which is sized off the same constant in
        // `effective_request_timeout_secs`. A pathological caller passing
        // `wait_for: 600_000` without `deadlineMs` would otherwise be cancelled
        // by Tower before the inner CDP loop noticed the bigger budget.
        let extra = if let Some(w) = wait_for_ms {
            let bounded = w.min(MAX_WAIT_FOR_MS);
            let per_tier = bounded.saturating_sub(SPA_DEFAULT_MS);
            per_tier.saturating_mul(self.renderer.cdp_tier_count() as u64)
        } else {
            0
        };
        default_ms.max(ladder_min.saturating_add(extra))
    }

    /// Tower middleware outer timeout (seconds). Must accommodate the longest
    /// legitimate handler runtime so a healthy request isn't cancelled by the
    /// outer layer before the inner deadline fires.
    ///
    /// Covers the three route envelopes:
    /// - `/scrape`, `/mcp` — auto-extended scrape deadline.
    /// - `/search` — SearXNG fetch + bounded enrichment fan-out
    ///   (`ceil(max_limit / max_concurrency)` batches × scrape_ms).
    /// - `/crawl/jobs/:id`, `/map` — handler-side caps up to 300s.
    ///
    /// When auto-extend is disabled, returns the operator-configured baseline
    /// unchanged.
    pub fn effective_request_timeout_secs(&self) -> u64 {
        let baseline = self.server.request_timeout_secs;
        if !self.request.auto_extend_deadline_for_ladder {
            return baseline;
        }
        const OUTER_BUFFER_SECS: u64 = 5;
        // `/map` handler caps `req.timeout.unwrap_or(120).min(300)`; the outer
        // must cover the upper bound so callers passing `timeout=300` aren't
        // cancelled mid-flight.
        const MAP_REQUEST_TIMEOUT_CEILING_MS: u64 = 300_000;
        // Cover the worst-case implicit scrape: caller bumps `wait_for` to the
        // configured maximum without supplying `deadlineMs`. The same
        // [`MAX_WAIT_FOR_MS`] constant is used inside `effective_deadline_ms`
        // to clamp the inner extension, so the inner deadline can never
        // exceed this outer envelope.
        let scrape_ms = self.effective_deadline_ms(None, Some(MAX_WAIT_FOR_MS));

        // Search enrichment: bounded by max_concurrency. Worst case sequential
        // batching with low concurrency: ceil(max_limit / max_concurrency)
        // batches each bounded by scrape_ms.
        let conc = (self.crawler.max_concurrency.max(1)) as u64;
        let max_results = self.search.max_limit as u64;
        let enrich_batches = max_results.div_ceil(conc);
        let search_enrichment_ms = enrich_batches.saturating_mul(scrape_ms);
        let search_ms = self.search.timeout_ms.saturating_add(search_enrichment_ms);

        let max_handler_ms = scrape_ms.max(search_ms).max(MAP_REQUEST_TIMEOUT_CEILING_MS);
        let needed_secs = max_handler_ms
            .div_ceil(1_000)
            .saturating_add(OUTER_BUFFER_SECS);
        baseline.max(needed_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env var tests modify process-wide state; serialize them to avoid cross-test
    /// interference (e.g. `force_js` alias + `render_js_default` direct both set).
    #[test]
    fn env_truthy_matches_the_sdk_rule() {
        // Pure: no process env, so this cannot race a sibling.
        for truthy in ["1", "true", "yes", "TRUE", " 1 ", "off", "2", "anything"] {
            assert!(env_truthy(Some(truthy)), "{truthy:?} should be truthy");
        }
        for falsy in ["0", "false", "no", "", "  ", "FALSE", " No "] {
            assert!(!env_truthy(Some(falsy)), "{falsy:?} should be falsy");
        }
        assert!(!env_truthy(None));
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn clear_renderer_env() {
        for k in [
            "CRW_RENDERER__MODE",
            "CRW_RENDERER__FORCE_JS",
            "CRW_RENDERER__RENDER_JS_DEFAULT",
            "CRW_RENDERER__LIGHTPANDA__WS_URL",
            "CRW_RENDERER__CAMOUFOX__BASE_URL",
            "CRW_RENDERER__CAMOUFOX__API_KEY",
            "CRW_RENDERER__CAMOUFOX__INCLUDE_IN_AUTO",
            "CRW_SERVER__PORT",
        ] {
            unsafe { std::env::remove_var(k) };
        }
    }

    #[test]
    fn renderer_mode_parses_variants() {
        #[derive(Deserialize)]
        struct Wrap {
            mode: RendererMode,
        }
        let cases = [
            ("mode = \"auto\"", RendererMode::Auto),
            ("mode = \"none\"", RendererMode::None),
            ("mode = \"lightpanda\"", RendererMode::Lightpanda),
            ("mode = \"chrome\"", RendererMode::Chrome),
            ("mode = \"playwright\"", RendererMode::Playwright),
            ("mode = \"camoufox\"", RendererMode::Camoufox),
        ];
        for (toml_str, expected) in cases {
            let w: Wrap = toml::from_str(toml_str).unwrap();
            assert_eq!(w.mode, expected, "toml: {toml_str}");
        }
    }

    #[test]
    fn renderer_mode_bogus_errors() {
        #[derive(Deserialize)]
        struct Wrap {
            #[allow(dead_code)]
            mode: RendererMode,
        }
        let err: Result<Wrap, _> = toml::from_str("mode = \"bogus\"");
        assert!(err.is_err(), "bogus mode should fail to parse");
    }

    #[test]
    fn renderer_config_default_mode_is_auto() {
        let cfg = RendererConfig::default();
        assert_eq!(cfg.mode, RendererMode::Auto);
        assert_eq!(cfg.render_js_default, None);
    }

    #[test]
    fn cloak_pool_size_defaults_to_the_sidecar_cap_not_the_render_pool() {
        // Deliberately NOT `pool_size`: the stealth tier is bounded by the
        // sidecar's browsers, so a box with a big render pool must not open
        // more cloak permits than the sidecar can serve.
        let cfg = RendererConfig {
            pool_size: 160,
            cloak_pool_size: None,
            ..Default::default()
        };
        assert_eq!(cfg.cloak_pool_size(), CLOAK_DEFAULT_POOL_SIZE);
    }

    #[test]
    fn cloak_pool_size_explicit_override_wins_and_clamps_zero() {
        let cfg = RendererConfig {
            cloak_pool_size: Some(12),
            ..Default::default()
        };
        assert_eq!(cfg.cloak_pool_size(), 12);
        let zero = RendererConfig {
            cloak_pool_size: Some(0),
            ..Default::default()
        };
        assert_eq!(zero.cloak_pool_size(), 1);
    }

    #[test]
    fn chrome_proxy_pool_size_falls_back_to_pool_size() {
        let cfg = RendererConfig {
            pool_size: 8,
            chrome_proxy_pool_size: None,
            ..Default::default()
        };
        assert_eq!(cfg.chrome_proxy_pool_size(), 8);
    }

    #[test]
    fn chrome_proxy_pool_size_explicit_override_wins() {
        let cfg = RendererConfig {
            pool_size: 8,
            chrome_proxy_pool_size: Some(16),
            ..Default::default()
        };
        assert_eq!(cfg.chrome_proxy_pool_size(), 16);
    }

    #[test]
    fn chrome_proxy_pool_size_clamps_zero_to_one() {
        let cfg = RendererConfig {
            pool_size: 8,
            chrome_proxy_pool_size: Some(0),
            ..Default::default()
        };
        assert_eq!(cfg.chrome_proxy_pool_size(), 1);
    }

    #[cfg(feature = "camoufox")]
    #[test]
    fn camoufox_in_ladder_semantics() {
        let ep = || CamoufoxEndpoint {
            base_url: "http://localhost:9377".into(),
            api_key: String::new(),
            include_in_auto: false,
        };
        // (1) configured + Auto + include_in_auto=false -> NOT in auto ladder.
        let c = RendererConfig {
            mode: RendererMode::Auto,
            camoufox: Some(ep()),
            ..Default::default()
        };
        assert!(
            !c.camoufox_in_ladder(),
            "opt-in default must stay out of auto"
        );
        // (2) Auto + include_in_auto=true -> in ladder.
        let c = RendererConfig {
            mode: RendererMode::Auto,
            camoufox: Some(CamoufoxEndpoint {
                include_in_auto: true,
                ..ep()
            }),
            ..Default::default()
        };
        assert!(c.camoufox_in_ladder());
        // (3) mode=Camoufox pin + include_in_auto=false -> in ladder (pinned).
        let c = RendererConfig {
            mode: RendererMode::Camoufox,
            camoufox: Some(ep()),
            ..Default::default()
        };
        assert!(c.camoufox_in_ladder());
        // (4) mode=None -> never.
        let c = RendererConfig {
            mode: RendererMode::None,
            camoufox: Some(ep()),
            ..Default::default()
        };
        assert!(!c.camoufox_in_ladder());
        // (5) other CDP-pinned modes -> never.
        let c = RendererConfig {
            mode: RendererMode::Chrome,
            camoufox: Some(CamoufoxEndpoint {
                include_in_auto: true,
                ..ep()
            }),
            ..Default::default()
        };
        assert!(!c.camoufox_in_ladder());
        // (6) blank base_url must NOT count as in-ladder even with the flag set
        // (mirrors the construction filter — no phantom deadline extension).
        let c = RendererConfig {
            mode: RendererMode::Auto,
            camoufox: Some(CamoufoxEndpoint {
                base_url: "   ".into(),
                include_in_auto: true,
                ..ep()
            }),
            ..Default::default()
        };
        assert!(!c.camoufox_in_ladder());
        // (7) blank base_url + mode=camoufox pin -> also not in ladder.
        let c = RendererConfig {
            mode: RendererMode::Camoufox,
            camoufox: Some(CamoufoxEndpoint {
                base_url: String::new(),
                ..ep()
            }),
            ..Default::default()
        };
        assert!(!c.camoufox_in_ladder());
    }

    #[cfg(not(feature = "camoufox"))]
    #[test]
    fn camoufox_in_ladder_always_false_without_feature() {
        // Without the feature the tier can never join the ladder, even if a
        // (deserialized) endpoint is present and mode is pinned to camoufox.
        let c = RendererConfig {
            mode: RendererMode::Camoufox,
            camoufox: Some(CamoufoxEndpoint {
                base_url: "http://localhost:9377".into(),
                api_key: String::new(),
                include_in_auto: true,
            }),
            ..Default::default()
        };
        assert!(!c.camoufox_in_ladder());
    }

    #[test]
    fn cloak_absent_config_is_none_and_not_in_ladder() {
        // Default config: no [renderer.cloak] → cloak is None and never in the
        // ladder (byte-identical to a build without the tier).
        let c = RendererConfig::default();
        assert!(c.cloak.is_none());
        assert!(!c.cloak_in_ladder());
        assert_eq!(c.cloak_timeout(), CLOAK_DEFAULT_TIMEOUT_MS);
    }

    #[cfg(not(feature = "cloak"))]
    #[test]
    fn cloak_in_ladder_always_false_without_feature() {
        // Without the feature the tier can never join the ladder, even if a
        // (deserialized) endpoint is present and mode is pinned to cloak.
        let c = RendererConfig {
            mode: RendererMode::Cloak,
            cloak: Some(CloakEndpoint {
                base_url: "http://cloak-sidecar:8000".into(),
                api_key: String::new(),
                include_in_auto: true,
            }),
            ..Default::default()
        };
        assert!(!c.cloak_in_ladder());
    }

    #[cfg(feature = "camoufox")]
    #[test]
    fn camoufox_only_no_cdp_deadline_not_starved() {
        // A camoufox-only deployment (no CDP tiers) with auto-extend on must
        // get a deadline of at least http_timeout + camoufox_timeout, never
        // clamped to the strict default.
        let mut app = AppConfig::default();
        app.request.auto_extend_deadline_for_ladder = true;
        app.renderer.mode = RendererMode::Auto;
        app.renderer.camoufox = Some(CamoufoxEndpoint {
            base_url: "http://localhost:9377".into(),
            api_key: String::new(),
            include_in_auto: true,
        });
        let d = app.effective_deadline_ms(None, None);
        let floor = app.renderer.http_timeout() + app.renderer.camoufox_timeout();
        assert!(
            d >= floor,
            "camoufox-only deadline {d} starved below {floor}"
        );
        // cdp_tier_count must remain 0 — camoufox is REST, never a CDP tier.
        assert_eq!(app.renderer.cdp_tier_count(), 0);
    }

    #[test]
    fn render_js_default_force_js_alias() {
        let cfg: RendererConfig = toml::from_str("force_js = true").unwrap();
        assert_eq!(cfg.render_js_default, Some(true));
    }

    #[test]
    fn render_js_default_direct_field() {
        let cfg: RendererConfig = toml::from_str("render_js_default = false").unwrap();
        assert_eq!(cfg.render_js_default, Some(false));
    }

    #[test]
    fn env_var_renderer_mode_chrome() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_RENDERER__MODE", "chrome") };
        let cfg = AppConfig::load().unwrap();
        clear_renderer_env();
        assert_eq!(cfg.renderer.mode, RendererMode::Chrome);
    }

    #[test]
    fn env_var_force_js_alias_works() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_RENDERER__FORCE_JS", "true") };
        let cfg = AppConfig::load().unwrap();
        clear_renderer_env();
        assert_eq!(cfg.renderer.render_js_default, Some(true));
    }

    #[test]
    fn env_var_render_js_default_direct() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_RENDERER__RENDER_JS_DEFAULT", "true") };
        let cfg = AppConfig::load().unwrap();
        clear_renderer_env();
        assert_eq!(cfg.renderer.render_js_default, Some(true));
    }

    #[test]
    fn request_config_defaults_match_plan() {
        let r = RequestConfig::default();
        assert_eq!(r.deadline_ms_default, 8000);
        assert!(r.auto_extend_deadline_for_ladder);
    }

    #[test]
    fn mcp_hide_credits_defaults_false() {
        let cfg: AppConfig = toml::from_str("").unwrap();
        assert!(!cfg.mcp.hide_credits);
    }

    #[test]
    fn mcp_hide_credits_from_toml() {
        let cfg: AppConfig = toml::from_str("[mcp]\nhide_credits = true").unwrap();
        assert!(cfg.mcp.hide_credits);
    }

    #[test]
    fn env_var_mcp_hide_credits() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("CRW_MCP__HIDE_CREDITS", "true") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_MCP__HIDE_CREDITS") };
        assert!(cfg.mcp.hide_credits);
    }

    /// The search backend URL was renamed from `searxng_url` to
    /// `search_backend_url`. Production sets the OLD env var *and* carries the
    /// old key in `config.docker.toml`, so two things must hold or live search
    /// silently returns `search_disabled`: the legacy spelling still resolves,
    /// and both spellings arriving together must not fail deserialization.
    ///
    /// A `#[serde(alias)]` satisfies the first and breaks the second with
    /// `duplicate field`, which is why the legacy name is its own field.
    ///
    /// One test rather than three: each case mutates the same process-wide env,
    /// so splitting them only races them against each other.
    #[test]
    fn search_backend_url_legacy_key_compat() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let load_with = |vars: &[(&str, &str)]| {
            for (k, v) in vars {
                unsafe { std::env::set_var(k, v) };
            }
            let out = AppConfig::load();
            for (k, _) in vars {
                unsafe { std::env::remove_var(k) };
            }
            out
        };

        let cfg = load_with(&[("CRW_SEARCH__SEARXNG_URL", "http://legacy:8080")])
            .expect("legacy-only config must deserialize");
        assert_eq!(
            cfg.search.resolve_backend_url(),
            Some("http://legacy:8080"),
            "legacy CRW_SEARCH__SEARXNG_URL must still resolve"
        );

        let cfg = load_with(&[("CRW_SEARCH__SEARCH_BACKEND_URL", "http://current:8080")])
            .expect("current-only config must deserialize");
        assert_eq!(
            cfg.search.resolve_backend_url(),
            Some("http://current:8080"),
            "the canonical CRW_SEARCH__SEARCH_BACKEND_URL must work"
        );

        // The production shape: both spellings present at once.
        let cfg = load_with(&[
            ("CRW_SEARCH__SEARXNG_URL", "http://legacy:8080"),
            ("CRW_SEARCH__SEARCH_BACKEND_URL", "http://current:8080"),
        ])
        .expect("both spellings together must not fail deserialization");
        assert_eq!(
            cfg.search.resolve_backend_url(),
            Some("http://current:8080"),
            "the current key name wins when both are set"
        );

        // Production's literal shape, not an approximation of it: CRW_CONFIG
        // selects config.docker.toml (which carries the legacy key) while the
        // container also exports the legacy env var pointing somewhere else.
        // The env var must win, and loading must not fail.
        // CRW_CONFIG resolves relative to the process cwd, which under `cargo
        // test` is the crate dir rather than the workspace root.
        let docker_cfg = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config.docker");
        let cfg = load_with(&[
            ("CRW_CONFIG", docker_cfg),
            ("CRW_SEARCH__SEARXNG_URL", "http://search-orchestrator:8080"),
        ])
        .expect("the deployed config shape must load");
        assert_eq!(
            cfg.search.resolve_backend_url(),
            Some("http://search-orchestrator:8080"),
            "the deployed env var must beat the legacy key in config.docker.toml"
        );
    }

    #[test]
    fn default_app_config_enables_auto_extend() {
        // Programmatic Default must mirror serde defaults — issue #35.
        let cfg = AppConfig::default();
        assert!(cfg.request.auto_extend_deadline_for_ladder);
        assert_eq!(cfg.request.deadline_ms_default, 8000);
    }

    fn renderer_with_chrome_only(chrome_ms: u64) -> RendererConfig {
        RendererConfig {
            mode: RendererMode::Chrome,
            page_timeout_ms: chrome_ms,
            chrome_timeout_ms: Some(chrome_ms),
            chrome: Some(CdpEndpoint {
                ws_url: "ws://chrome:9222".into(),
            }),
            ..Default::default()
        }
    }

    #[test]
    #[cfg(feature = "cdp")]
    fn min_deadline_full_ladder_chrome_only() {
        // chrome-only mode: http (page_timeout) + chrome + 1 * 28000.
        let r = renderer_with_chrome_only(30_000);
        // page_timeout_ms is set to chrome_ms here, so http_timeout() → 30s.
        assert_eq!(
            r.min_deadline_for_full_ladder_ms(),
            30_000 + 30_000 + 28_000
        );
    }

    #[test]
    #[cfg(feature = "cdp")]
    fn min_deadline_full_ladder_auto_three_tiers() {
        let r = RendererConfig {
            mode: RendererMode::Auto,
            page_timeout_ms: 15_000,
            http_timeout_ms: Some(15_000),
            lightpanda_timeout_ms: Some(2_500),
            chrome_timeout_ms: Some(30_000),
            lightpanda: Some(CdpEndpoint {
                ws_url: "ws://lp:9222".into(),
            }),
            chrome: Some(CdpEndpoint {
                ws_url: "ws://chrome:9222".into(),
            }),
            ..Default::default()
        };
        // http(15) + lp(2.5) + chrome(30) + 2*28 = 47.5 + 56 = 103_500.
        assert_eq!(
            r.min_deadline_for_full_ladder_ms(),
            15_000 + 2_500 + 30_000 + 2 * 28_000
        );
        assert_eq!(r.cdp_tier_count(), 2);
    }

    #[test]
    #[cfg(all(feature = "cdp", feature = "cloak"))]
    fn min_deadline_reserves_cloak_recovery_budget_when_enabled() {
        // Cloak configured as a RECOVERY arm (include_in_auto = false), with the
        // recovery flag on: the ladder-min must reserve CLOAK_ARM_RECOVER_BUDGET_MS
        // so the outer request-timeout envelope can absorb a fresh cloak solve.
        let base = RendererConfig {
            mode: RendererMode::Auto,
            page_timeout_ms: 15_000,
            http_timeout_ms: Some(15_000),
            lightpanda_timeout_ms: Some(2_500),
            chrome_timeout_ms: Some(30_000),
            lightpanda: Some(CdpEndpoint {
                ws_url: "ws://lp:9222".into(),
            }),
            chrome: Some(CdpEndpoint {
                ws_url: "ws://chrome:9222".into(),
            }),
            cloak: Some(CloakEndpoint {
                base_url: "http://cloak:8000".into(),
                api_key: String::new(),
                include_in_auto: false,
            }),
            ..Default::default()
        };
        let ladder = 15_000 + 2_500 + 30_000 + 2 * 28_000;
        // Flag off (default): recovery arm not reserved — byte-identical to today.
        assert!(!base.cloak_recovery_active());
        assert_eq!(base.min_deadline_for_full_ladder_ms(), ladder);
        // Flag on: reserves the fresh recovery budget on top of the ladder sum.
        let on = RendererConfig {
            cloak_recover_on_cf: true,
            ..base
        };
        assert!(on.cloak_recovery_active());
        assert_eq!(
            on.min_deadline_for_full_ladder_ms(),
            ladder + CLOAK_ARM_RECOVER_BUDGET_MS
        );
    }

    #[test]
    fn effective_deadline_explicit_bypasses_auto_extend() {
        let mut cfg = AppConfig::default();
        cfg.request.auto_extend_deadline_for_ladder = true;
        cfg.renderer = renderer_with_chrome_only(30_000);
        // Explicit override beats both default and ladder_min.
        assert_eq!(cfg.effective_deadline_ms(Some(5_000), None), 5_000);
        assert_eq!(cfg.effective_deadline_ms(Some(500_000), None), 500_000);
    }

    #[test]
    #[cfg(feature = "cdp")]
    fn effective_deadline_auto_extend_raises_to_ladder_min() {
        let mut cfg = AppConfig::default();
        cfg.request.auto_extend_deadline_for_ladder = true;
        cfg.request.deadline_ms_default = 8_000;
        cfg.renderer = renderer_with_chrome_only(30_000);
        let expected = cfg.renderer.min_deadline_for_full_ladder_ms();
        assert!(expected > 8_000);
        assert_eq!(cfg.effective_deadline_ms(None, None), expected);
    }

    #[test]
    fn effective_deadline_default_wins_when_higher_than_ladder() {
        let mut cfg = AppConfig::default();
        cfg.request.auto_extend_deadline_for_ladder = true;
        cfg.request.deadline_ms_default = 1_000_000;
        cfg.renderer = renderer_with_chrome_only(30_000);
        assert_eq!(cfg.effective_deadline_ms(None, None), 1_000_000);
    }

    #[test]
    fn effective_deadline_auto_extend_disabled_returns_baseline() {
        let mut cfg = AppConfig::default();
        cfg.request.auto_extend_deadline_for_ladder = false;
        cfg.request.deadline_ms_default = 8_000;
        cfg.renderer = renderer_with_chrome_only(30_000);
        assert_eq!(cfg.effective_deadline_ms(None, None), 8_000);
    }

    #[test]
    #[cfg(feature = "cdp")]
    fn effective_deadline_extends_for_long_wait_for() {
        let mut cfg = AppConfig::default();
        cfg.request.auto_extend_deadline_for_ladder = true;
        cfg.request.deadline_ms_default = 8_000;
        cfg.renderer = renderer_with_chrome_only(30_000);
        let base = cfg.renderer.min_deadline_for_full_ladder_ms();
        let tier_count = cfg.renderer.cdp_tier_count() as u64;
        // wait_for = 20000 → per-tier extra = 12000 over SPA_DEFAULT_MS (8000).
        let with_wait = cfg.effective_deadline_ms(None, Some(20_000));
        assert_eq!(with_wait, base + 12_000 * tier_count);
        // wait_for below SPA default → no extra.
        assert_eq!(cfg.effective_deadline_ms(None, Some(2_000)), base);
    }

    #[test]
    fn effective_request_timeout_covers_map_ceiling() {
        let mut cfg = AppConfig::default();
        cfg.request.auto_extend_deadline_for_ladder = true;
        cfg.request.deadline_ms_default = 8_000;
        cfg.renderer = renderer_with_chrome_only(30_000);
        cfg.search.timeout_ms = 15_000;
        cfg.crawler.max_concurrency = 10;
        cfg.search.max_limit = 20;
        cfg.server.request_timeout_secs = 60;
        // Map ceiling 300s + 5s buffer = 305s minimum.
        assert!(cfg.effective_request_timeout_secs() >= 305);
    }

    #[test]
    fn effective_request_timeout_disabled_returns_baseline() {
        let mut cfg = AppConfig::default();
        cfg.request.auto_extend_deadline_for_ladder = false;
        cfg.server.request_timeout_secs = 60;
        assert_eq!(cfg.effective_request_timeout_secs(), 60);
    }

    #[test]
    fn effective_request_timeout_respects_operator_override() {
        let mut cfg = AppConfig::default();
        cfg.request.auto_extend_deadline_for_ladder = true;
        cfg.server.request_timeout_secs = 600; // operator-configured high
        cfg.renderer = renderer_with_chrome_only(30_000);
        // Operator's explicit 600s should win over the auto-computed 305s.
        assert_eq!(cfg.effective_request_timeout_secs(), 600);
    }

    #[test]
    fn effective_request_timeout_search_sequential_batching() {
        // Low concurrency forces ceil(max_limit/conc) batches → larger search_ms.
        let mut cfg = AppConfig::default();
        cfg.request.auto_extend_deadline_for_ladder = true;
        cfg.request.deadline_ms_default = 8_000;
        cfg.renderer = renderer_with_chrome_only(30_000);
        cfg.search.timeout_ms = 15_000;
        cfg.search.max_limit = 20;
        cfg.crawler.max_concurrency = 1;
        cfg.server.request_timeout_secs = 60;
        // The Tower envelope must cover the worst-case implicit scrape with
        // `wait_for` bumped to MAX_WAIT_FOR_MS (60s), because callers can do
        // that without supplying `deadlineMs`. Mirror that in the expected.
        let secs = cfg.effective_request_timeout_secs();
        let scrape_ms = cfg.effective_deadline_ms(None, Some(60_000));
        let expected_search_ms = 15_000 + 20 * scrape_ms;
        let expected_max_ms = scrape_ms.max(expected_search_ms).max(300_000);
        let expected_secs = expected_max_ms.div_ceil(1_000) + 5;
        assert_eq!(secs, 60u64.max(expected_secs));
    }

    #[test]
    #[cfg(not(feature = "cdp"))]
    fn cdp_tier_count_zero_without_cdp_feature() {
        // Even when chrome/lightpanda are configured, a binary built without
        // the `cdp` feature can never construct a JS renderer. The deadline
        // policy must observe that and collapse to HTTP-only behavior.
        let r = RendererConfig {
            mode: RendererMode::Auto,
            page_timeout_ms: 15_000,
            chrome_timeout_ms: Some(30_000),
            chrome: Some(CdpEndpoint {
                ws_url: "ws://chrome:9222".into(),
            }),
            lightpanda: Some(CdpEndpoint {
                ws_url: "ws://lp:9222".into(),
            }),
            ..Default::default()
        };
        assert_eq!(r.cdp_tier_count(), 0);
        // Only the HTTP tier contributes to the ladder budget.
        assert_eq!(r.min_deadline_for_full_ladder_ms(), 15_000);
    }

    #[test]
    fn effective_deadline_skipped_for_http_only_mode() {
        // P2 from codex review: HTTP-only deployments don't suffer the CDP
        // clamping problem (no fetch/challenge/stability overhead). The
        // auto-extension must NOT silently bump their default from 8s to 30s
        // just because page_timeout_ms defaults high.
        let mut cfg = AppConfig::default();
        cfg.request.auto_extend_deadline_for_ladder = true;
        cfg.request.deadline_ms_default = 8_000;
        cfg.renderer = RendererConfig {
            mode: RendererMode::Auto,
            page_timeout_ms: 30_000,
            // No CDP endpoints configured.
            lightpanda: None,
            playwright: None,
            chrome: None,
            ..Default::default()
        };
        assert_eq!(cfg.renderer.cdp_tier_count(), 0);
        assert_eq!(cfg.effective_deadline_ms(None, None), 8_000);
        assert_eq!(cfg.effective_deadline_ms(None, Some(30_000)), 8_000);
    }

    #[test]
    #[cfg(feature = "cdp")]
    fn min_deadline_full_ladder_playwright_only() {
        // Playwright tier contributes one chrome_timeout + one CDP overhead,
        // matching the runtime predicate in `crw-renderer/src/lib.rs`.
        let r = RendererConfig {
            mode: RendererMode::Playwright,
            page_timeout_ms: 15_000,
            http_timeout_ms: Some(15_000),
            chrome_timeout_ms: Some(30_000),
            playwright: Some(CdpEndpoint {
                ws_url: "ws://playwright:9222".into(),
            }),
            ..Default::default()
        };
        assert_eq!(r.cdp_tier_count(), 1);
        // http(15) + chrome-equivalent(30) + 1 * 28 overhead.
        assert_eq!(
            r.min_deadline_for_full_ladder_ms(),
            15_000 + 30_000 + 28_000
        );
    }

    #[test]
    fn renderer_phase_toggles_default_off_or_safe() {
        let r = RendererConfig::default();
        assert!(!r.chrome_intercept_resources);
        assert!(!r.chrome_intercept_stylesheets);
        assert!(r.chrome_host_intercept_disable.is_empty());
        assert_eq!(r.chrome_nav_budget_ms, 12_000);
        assert!(!r.chrome_context_pool_enabled);
        assert!(!r.use_predictor);
    }

    #[test]
    fn crawler_per_host_limiter_defaults() {
        let c = CrawlerConfig::default();
        assert_eq!(c.per_host_min_interval_ms, 0);
        assert_eq!(c.per_host_max_concurrent, 1);
    }

    #[test]
    fn env_var_overrides_toml_defaults() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_renderer_env();
        unsafe {
            std::env::set_var("CRW_SERVER__PORT", "4444");
            std::env::set_var("CRW_RENDERER__LIGHTPANDA__WS_URL", "ws://test:9999/");
        }
        let cfg = AppConfig::load().unwrap();
        clear_renderer_env();

        assert_eq!(cfg.server.port, 4444, "env var should override server.port");
        assert_eq!(
            cfg.renderer.lightpanda.as_ref().unwrap().ws_url,
            "ws://test:9999/",
            "env var should override renderer.lightpanda.ws_url"
        );
    }

    #[test]
    fn crawler_proxy_empty_env_normalizes_to_none() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_renderer_env();
        // Isolate from any developer ~/.config/crw/config.toml and stray CRW_CONFIG.
        let tmp = std::env::temp_dir().join(format!("crw-proxy-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe {
            std::env::set_var("CRW_USER_CONFIG_DIR", &tmp);
            std::env::remove_var("CRW_CONFIG");
        }

        let load = || {
            // CRW_CONFIG and the user config dir are pinned for the whole test.
            AppConfig::load().unwrap().crawler.proxy
        };

        // (1) absent -> None (serde default).
        unsafe { std::env::remove_var("CRW_CRAWLER__PROXY") };
        assert_eq!(load(), None, "absent proxy env should be None");

        // (2) present-but-empty -> None (the issue #154 case).
        unsafe { std::env::set_var("CRW_CRAWLER__PROXY", "") };
        assert_eq!(load(), None, "empty proxy env should normalize to None");

        // (3) whitespace-only -> None.
        unsafe { std::env::set_var("CRW_CRAWLER__PROXY", "   ") };
        assert_eq!(
            load(),
            None,
            "whitespace proxy env should normalize to None"
        );

        // (4) a real value -> Some (trimmed).
        unsafe { std::env::set_var("CRW_CRAWLER__PROXY", "  http://proxy:8080  ") };
        assert_eq!(
            load(),
            Some("http://proxy:8080".to_string()),
            "valid proxy env should be preserved and trimmed"
        );

        unsafe {
            std::env::remove_var("CRW_CRAWLER__PROXY");
            std::env::remove_var("CRW_USER_CONFIG_DIR");
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn user_config_path_honors_override_env() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("crw-cfg-test-{}", std::process::id()));
        unsafe {
            std::env::set_var("CRW_USER_CONFIG_DIR", &tmp);
        }
        let p = user_config_path().unwrap();
        unsafe {
            std::env::remove_var("CRW_USER_CONFIG_DIR");
        }
        assert_eq!(p, tmp.join("config.toml"));
    }

    #[test]
    fn user_config_file_is_picked_up_by_load() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_renderer_env();
        let tmp = std::env::temp_dir().join(format!("crw-load-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let cfg_path = tmp.join("config.toml");
        std::fs::write(
            &cfg_path,
            r#"
[client]
api_url = "https://api.example.com"
api_key = "test-key-123"

[search]
search_backend_url = "http://localhost:9999"

[extraction.llm]
provider = "deepseek"
api_key = "sk-test"
model = "deepseek-chat"
"#,
        )
        .unwrap();

        unsafe {
            std::env::set_var("CRW_USER_CONFIG_DIR", &tmp);
        }
        let cfg = AppConfig::load().unwrap();
        unsafe {
            std::env::remove_var("CRW_USER_CONFIG_DIR");
        }
        std::fs::remove_dir_all(&tmp).ok();

        assert_eq!(
            cfg.client.api_url.as_deref(),
            Some("https://api.example.com")
        );
        assert_eq!(cfg.client.api_key.as_deref(), Some("test-key-123"));
        assert_eq!(
            cfg.search.search_backend_url.as_deref(),
            Some("http://localhost:9999")
        );
        let llm = cfg.extraction.llm.expect("llm config present");
        assert_eq!(llm.provider, "deepseek");
        assert_eq!(llm.api_key, "sk-test");
    }

    #[test]
    fn public_client_env_aliases_override_user_config() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp =
            std::env::temp_dir().join(format!("crw-client-alias-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("config.toml"),
            r#"
[client]
api_url = "https://from-file.example"
api_key = "file-key"
"#,
        )
        .unwrap();

        unsafe {
            std::env::set_var("CRW_USER_CONFIG_DIR", &tmp);
            std::env::set_var("CRW_API_URL", "  http://self-hosted:3000  ");
            std::env::set_var("CRW_API_KEY", "  env-key  ");
        }
        let cfg = AppConfig::load().unwrap();
        unsafe {
            std::env::remove_var("CRW_USER_CONFIG_DIR");
            std::env::remove_var("CRW_API_URL");
            std::env::remove_var("CRW_API_KEY");
        }
        std::fs::remove_dir_all(&tmp).ok();

        assert_eq!(
            cfg.client.api_url.as_deref(),
            Some("http://self-hosted:3000")
        );
        assert_eq!(cfg.client.api_key.as_deref(), Some("env-key"));
    }

    #[test]
    fn env_var_beats_user_config() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_renderer_env();
        let tmp = std::env::temp_dir().join(format!("crw-prec-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("config.toml"),
            r#"
[search]
search_backend_url = "http://from-file:8080"
"#,
        )
        .unwrap();

        unsafe {
            std::env::set_var("CRW_USER_CONFIG_DIR", &tmp);
            std::env::set_var("CRW_SEARCH__SEARCH_BACKEND_URL", "http://from-env:8080");
        }
        let cfg = AppConfig::load().unwrap();
        unsafe {
            std::env::remove_var("CRW_USER_CONFIG_DIR");
            std::env::remove_var("CRW_SEARCH__SEARCH_BACKEND_URL");
        }
        std::fs::remove_dir_all(&tmp).ok();

        assert_eq!(
            cfg.search.search_backend_url.as_deref(),
            Some("http://from-env:8080"),
            "env var must win over user config file"
        );
    }

    #[test]
    fn effective_proxy_credentials_appends_country_suffix() {
        let cfg = RendererConfig {
            proxy_base_user: Some("abc".into()),
            proxy_base_pass: Some("pw".into()),
            proxy_default_country: Some("de".into()),
            ..Default::default()
        };
        let (u, p) = cfg.effective_proxy_credentials(Some("us")).unwrap();
        assert_eq!(u, "abc__cr.us");
        assert_eq!(p, "pw");
        // Per-request wins over default.
        let (u, _) = cfg.effective_proxy_credentials(Some("GB")).unwrap();
        assert_eq!(u, "abc__cr.gb", "uppercase input is normalized");
        // Default country used when per-request omits it.
        let (u, _) = cfg.effective_proxy_credentials(None).unwrap();
        assert_eq!(u, "abc__cr.de");
    }

    #[test]
    fn effective_proxy_credentials_invalid_country_uses_global_pool() {
        let cfg = RendererConfig {
            proxy_base_user: Some("abc".into()),
            proxy_base_pass: Some("pw".into()),
            ..Default::default()
        };
        // 3-letter ISO code → rejected, no suffix (global pool).
        let (u, _) = cfg.effective_proxy_credentials(Some("usa")).unwrap();
        assert_eq!(u, "abc");
        // Digits → rejected.
        let (u, _) = cfg.effective_proxy_credentials(Some("u1")).unwrap();
        assert_eq!(u, "abc");
        // Empty string after trim → rejected.
        let (u, _) = cfg.effective_proxy_credentials(Some("  ")).unwrap();
        assert_eq!(u, "abc");
    }

    #[test]
    fn effective_proxy_credentials_nodemaven_format() {
        let cfg = RendererConfig {
            proxy_base_user: Some("acct".into()),
            proxy_base_pass: Some("pw".into()),
            proxy_default_country: Some("de".into()),
            proxy_username_format: ProxyUsernameFormat::NodeMaven,
            ..Default::default()
        };
        let (u, p) = cfg.effective_proxy_credentials(Some("US")).unwrap();
        assert_eq!(u, "acct-country-us");
        assert_eq!(p, "pw");
        let (u, _) = cfg.effective_proxy_credentials(None).unwrap();
        assert_eq!(u, "acct-country-de");
        let (u, _) = cfg.effective_proxy_credentials(Some("usa")).unwrap();
        assert_eq!(
            u, "acct",
            "invalid country falls through to the global pool"
        );
    }

    #[test]
    fn proxy_username_format_composes_country_and_session() {
        let di = ProxyUsernameFormat::DataImpulse;
        let nm = ProxyUsernameFormat::NodeMaven;
        assert_eq!(
            di.compose("u", Some("us"), Some("s1")),
            "u__cr.us;sessid.sticks1"
        );
        assert_eq!(di.compose("u", None, Some("s1")), "u;sessid.sticks1");
        assert_eq!(
            nm.compose("u", Some("us"), Some("s1")),
            "u-country-us-sid-s1"
        );
        assert_eq!(nm.compose("u", None, Some("s1")), "u-sid-s1");
        assert_eq!(nm.compose("u", None, None), "u");
    }

    #[test]
    fn proxy_username_format_parses_lowercase_names() {
        #[derive(Deserialize)]
        struct Wrap {
            f: ProxyUsernameFormat,
        }
        let w: Wrap = toml::from_str("f = \"nodemaven\"").unwrap();
        assert_eq!(w.f, ProxyUsernameFormat::NodeMaven);
        let w: Wrap = toml::from_str("f = \"dataimpulse\"").unwrap();
        assert_eq!(w.f, ProxyUsernameFormat::DataImpulse);
    }

    #[test]
    fn effective_proxy_credentials_no_base_returns_none() {
        let cfg = RendererConfig::default();
        assert!(cfg.effective_proxy_credentials(Some("us")).is_none());

        let only_user = RendererConfig {
            proxy_base_user: Some("abc".into()),
            ..Default::default()
        };
        assert!(only_user.effective_proxy_credentials(Some("us")).is_none());
    }

    // ==================================================================
    // ServerConfig
    // ==================================================================

    #[test]
    fn server_config_defaults() {
        let s = ServerConfig::default();
        assert_eq!(s.host, "0.0.0.0");
        assert_eq!(s.port, 3000);
        assert_eq!(s.request_timeout_secs, 60);
        assert_eq!(s.rate_limit_rps, 10);
        assert!(s.cors_allowed_origins.is_empty());
    }

    #[test]
    fn server_config_toml_full_override() {
        let s: ServerConfig = toml::from_str(
            r#"
            host = "127.0.0.1"
            port = 8080
            request_timeout_secs = 45
            rate_limit_rps = 0
            cors_allowed_origins = ["https://a.example", "https://b.example"]
            "#,
        )
        .unwrap();
        assert_eq!(s.host, "127.0.0.1");
        assert_eq!(s.port, 8080);
        assert_eq!(s.request_timeout_secs, 45);
        assert_eq!(s.rate_limit_rps, 0);
        assert_eq!(
            s.cors_allowed_origins,
            vec!["https://a.example", "https://b.example"]
        );
    }

    #[test]
    fn server_config_cors_from_comma_string() {
        let s: ServerConfig =
            toml::from_str(r#"cors_allowed_origins = "https://a,https://b""#).unwrap();
        assert_eq!(s.cors_allowed_origins, vec!["https://a", "https://b"]);
    }

    #[test]
    fn server_config_cors_from_json_array_string() {
        let s: ServerConfig =
            toml::from_str(r#"cors_allowed_origins = "[\"https://a\",\"https://b\"]""#).unwrap();
        assert_eq!(s.cors_allowed_origins, vec!["https://a", "https://b"]);
    }

    #[test]
    fn server_config_partial_toml_keeps_other_defaults() {
        let s: ServerConfig = toml::from_str("port = 9999").unwrap();
        assert_eq!(s.port, 9999);
        assert_eq!(s.host, "0.0.0.0", "unset fields must keep their default");
        assert_eq!(s.request_timeout_secs, 60);
    }

    #[test]
    fn env_var_server_host_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_SERVER__HOST", "10.0.0.1") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_SERVER__HOST") };
        assert_eq!(cfg.server.host, "10.0.0.1");
    }

    #[test]
    fn env_var_server_rate_limit_rps() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_SERVER__RATE_LIMIT_RPS", "500") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_SERVER__RATE_LIMIT_RPS") };
        assert_eq!(cfg.server.rate_limit_rps, 500);
    }

    #[test]
    fn env_var_server_request_timeout_secs() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_SERVER__REQUEST_TIMEOUT_SECS", "90") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_SERVER__REQUEST_TIMEOUT_SECS") };
        assert_eq!(cfg.server.request_timeout_secs, 90);
    }

    #[test]
    fn env_var_server_port_malformed_number_errors() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_SERVER__PORT", "not-a-port") };
        let result = AppConfig::load();
        unsafe { std::env::remove_var("CRW_SERVER__PORT") };
        assert!(result.is_err(), "non-numeric port must fail to load");
    }

    // ==================================================================
    // RequestConfig
    // ==================================================================

    #[test]
    fn request_config_toml_full_override() {
        let r: RequestConfig = toml::from_str(
            r#"
            deadline_ms_default = 12345
            auto_extend_deadline_for_ladder = false
            "#,
        )
        .unwrap();
        assert_eq!(r.deadline_ms_default, 12345);
        assert!(!r.auto_extend_deadline_for_ladder);
    }

    #[test]
    fn request_config_empty_toml_uses_defaults() {
        let r: RequestConfig = toml::from_str("").unwrap();
        assert_eq!(r.deadline_ms_default, 8000);
        assert!(r.auto_extend_deadline_for_ladder);
    }

    #[test]
    fn env_var_request_deadline_ms_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_REQUEST__DEADLINE_MS_DEFAULT", "20000") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_REQUEST__DEADLINE_MS_DEFAULT") };
        assert_eq!(cfg.request.deadline_ms_default, 20000);
    }

    #[test]
    fn env_var_request_auto_extend_false() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_REQUEST__AUTO_EXTEND_DEADLINE_FOR_LADDER", "false") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_REQUEST__AUTO_EXTEND_DEADLINE_FOR_LADDER") };
        assert!(!cfg.request.auto_extend_deadline_for_ladder);
    }

    // ==================================================================
    // RendererMode / RendererConfig field-level coverage
    // ==================================================================

    #[test]
    fn renderer_mode_default_direct() {
        assert_eq!(RendererMode::default(), RendererMode::Auto);
    }

    #[test]
    fn renderer_mode_serialize_round_trip() {
        let cases = [
            RendererMode::Auto,
            RendererMode::None,
            RendererMode::Lightpanda,
            RendererMode::Chrome,
            RendererMode::Playwright,
            RendererMode::Camoufox,
            RendererMode::Cloak,
        ];
        for mode in cases {
            let s = serde_json::to_string(&mode).unwrap();
            let back: RendererMode = serde_json::from_str(&s).unwrap();
            assert_eq!(back, mode, "round trip failed for {s}");
        }
    }

    #[test]
    fn renderer_config_default_all_fields() {
        let r = RendererConfig::default();
        assert_eq!(r.mode, RendererMode::Auto);
        assert_eq!(r.page_timeout_ms, 30_000);
        assert_eq!(r.http_timeout_ms, None);
        assert_eq!(r.lightpanda_timeout_ms, None);
        assert_eq!(r.chrome_timeout_ms, None);
        assert_eq!(r.pool_size, 4);
        assert_eq!(r.chrome_proxy_pool_size, None);
        assert_eq!(r.chrome_challenge_max_retries, None);
        assert_eq!(r.chrome_spa_selector_max_ms, None);
        assert!(!r.chrome_fast_ready);
        assert!(!r.chrome_hedge);
        assert!(!r.auto_egress_escalation);
        assert!(!r.cloak_recover_on_cf);
        assert!(!r.latency_breakdown);
        assert_eq!(r.render_js_default, None);
        assert!(r.lightpanda.is_none());
        assert!(r.playwright.is_none());
        assert!(r.chrome.is_none());
        assert!(r.chrome_proxy.is_none());
        assert_eq!(r.chrome_proxy_timeout_ms, None);
        assert!(r.camoufox.is_none());
        assert_eq!(r.camoufox_timeout_ms, None);
        assert_eq!(r.camoufox_challenge_wait_ms, None);
        assert!(r.cloak.is_none());
        assert_eq!(r.cloak_timeout_ms, None);
        assert_eq!(r.cloak_proxy_host, None);
        assert!(!r.chrome_intercept_resources);
        assert!(!r.chrome_intercept_stylesheets);
        assert!(r.chrome_host_intercept_disable.is_empty());
        assert_eq!(r.chrome_nav_budget_ms, 12_000);
        assert!(!r.chrome_context_pool_enabled);
        assert_eq!(r.chrome_backend, ChromeBackend::Vanilla);
        assert!(!r.use_predictor);
        assert_eq!(r.proxy_base_user, None);
        assert_eq!(r.proxy_base_pass, None);
        assert_eq!(r.proxy_default_country, None);
    }

    #[test]
    fn renderer_config_toml_parse_chrome_endpoint() {
        let r: RendererConfig = toml::from_str(
            r#"
            mode = "chrome"
            [chrome]
            ws_url = "ws://chrome-host:9222"
            "#,
        )
        .unwrap();
        assert_eq!(r.mode, RendererMode::Chrome);
        assert_eq!(r.chrome.unwrap().ws_url, "ws://chrome-host:9222");
    }

    #[test]
    fn renderer_config_toml_parse_lightpanda_and_playwright_endpoints() {
        let r: RendererConfig = toml::from_str(
            r#"
            [lightpanda]
            ws_url = "ws://lp:9222"
            [playwright]
            ws_url = "ws://pw:9222"
            "#,
        )
        .unwrap();
        assert_eq!(r.lightpanda.unwrap().ws_url, "ws://lp:9222");
        assert_eq!(r.playwright.unwrap().ws_url, "ws://pw:9222");
    }

    #[test]
    fn cdp_endpoint_missing_ws_url_errors() {
        let result: Result<CdpEndpoint, _> = toml::from_str("");
        assert!(result.is_err(), "ws_url has no default, must be required");
    }

    #[test]
    fn camoufox_endpoint_default() {
        let c = CamoufoxEndpoint::default();
        assert_eq!(c.base_url, "");
        assert_eq!(c.api_key, "");
        assert!(!c.include_in_auto);
    }

    #[test]
    fn camoufox_endpoint_toml_parse_only_base_url() {
        let c: CamoufoxEndpoint = toml::from_str(r#"base_url = "http://cam:9377""#).unwrap();
        assert_eq!(c.base_url, "http://cam:9377");
        assert_eq!(c.api_key, "", "api_key must default to empty string");
        assert!(!c.include_in_auto);
    }

    #[test]
    fn camoufox_endpoint_missing_base_url_errors() {
        let result: Result<CamoufoxEndpoint, _> = toml::from_str("");
        assert!(result.is_err(), "base_url has no default, must be required");
    }

    #[test]
    fn cloak_endpoint_default() {
        let c = CloakEndpoint::default();
        assert_eq!(c.base_url, "");
        assert_eq!(c.api_key, "");
        assert!(!c.include_in_auto);
    }

    #[test]
    fn cloak_endpoint_toml_parse_full() {
        let c: CloakEndpoint = toml::from_str(
            r#"
            base_url = "http://cloak:8000"
            api_key = "secret"
            include_in_auto = true
            "#,
        )
        .unwrap();
        assert_eq!(c.base_url, "http://cloak:8000");
        assert_eq!(c.api_key, "secret");
        assert!(c.include_in_auto);
    }

    #[test]
    fn chrome_backend_default_is_vanilla() {
        assert_eq!(ChromeBackend::default(), ChromeBackend::Vanilla);
    }

    #[test]
    fn chrome_backend_toml_parse_browserless() {
        #[derive(Deserialize)]
        struct Wrap {
            chrome_backend: ChromeBackend,
        }
        let w: Wrap = toml::from_str(r#"chrome_backend = "browserless""#).unwrap();
        assert_eq!(w.chrome_backend, ChromeBackend::Browserless);
    }

    #[test]
    fn chrome_backend_bogus_value_errors() {
        #[derive(Deserialize)]
        struct Wrap {
            #[allow(dead_code)]
            chrome_backend: ChromeBackend,
        }
        let result: Result<Wrap, _> = toml::from_str(r#"chrome_backend = "netscape""#);
        assert!(result.is_err());
    }

    #[test]
    fn http_timeout_falls_back_to_page_timeout_then_explicit_wins() {
        let mut r = RendererConfig {
            page_timeout_ms: 25_000,
            ..Default::default()
        };
        assert_eq!(r.http_timeout(), 25_000);
        r.http_timeout_ms = Some(9_000);
        assert_eq!(r.http_timeout(), 9_000);
    }

    #[test]
    fn lightpanda_timeout_falls_back_to_page_timeout_then_explicit_wins() {
        let mut r = RendererConfig {
            page_timeout_ms: 25_000,
            ..Default::default()
        };
        assert_eq!(r.lightpanda_timeout(), 25_000);
        r.lightpanda_timeout_ms = Some(3_000);
        assert_eq!(r.lightpanda_timeout(), 3_000);
    }

    #[test]
    fn chrome_timeout_falls_back_to_page_timeout_then_explicit_wins() {
        let mut r = RendererConfig {
            page_timeout_ms: 25_000,
            ..Default::default()
        };
        assert_eq!(r.chrome_timeout(), 25_000);
        r.chrome_timeout_ms = Some(40_000);
        assert_eq!(r.chrome_timeout(), 40_000);
    }

    #[test]
    fn chrome_proxy_timeout_falls_back_to_chrome_plus_15s() {
        let r = RendererConfig {
            chrome_timeout_ms: Some(30_000),
            ..Default::default()
        };
        assert_eq!(r.chrome_proxy_timeout(), 45_000);
    }

    #[test]
    fn chrome_proxy_timeout_explicit_override_wins() {
        let r = RendererConfig {
            chrome_timeout_ms: Some(30_000),
            chrome_proxy_timeout_ms: Some(99_000),
            ..Default::default()
        };
        assert_eq!(r.chrome_proxy_timeout(), 99_000);
    }

    #[test]
    fn camoufox_timeout_default_and_override() {
        let r = RendererConfig::default();
        assert_eq!(r.camoufox_timeout(), CAMOUFOX_DEFAULT_TIMEOUT_MS);
        let r2 = RendererConfig {
            camoufox_timeout_ms: Some(1_234),
            ..Default::default()
        };
        assert_eq!(r2.camoufox_timeout(), 1_234);
    }

    #[test]
    fn camoufox_challenge_wait_default_and_override() {
        let r = RendererConfig::default();
        assert_eq!(
            r.camoufox_challenge_wait(),
            CAMOUFOX_DEFAULT_CHALLENGE_WAIT_MS
        );
        let r2 = RendererConfig {
            camoufox_challenge_wait_ms: Some(4_321),
            ..Default::default()
        };
        assert_eq!(r2.camoufox_challenge_wait(), 4_321);
        // Explicit 0 must disable the poll, not fall back to the default.
        let r3 = RendererConfig {
            camoufox_challenge_wait_ms: Some(0),
            ..Default::default()
        };
        assert_eq!(r3.camoufox_challenge_wait(), 0);
    }

    #[test]
    fn cloak_timeout_default_and_override() {
        let r = RendererConfig::default();
        assert_eq!(r.cloak_timeout(), CLOAK_DEFAULT_TIMEOUT_MS);
        let r2 = RendererConfig {
            cloak_timeout_ms: Some(5_678),
            ..Default::default()
        };
        assert_eq!(r2.cloak_timeout(), 5_678);
    }

    #[test]
    #[cfg(not(feature = "cdp"))]
    fn min_deadline_full_ladder_mode_none_is_zero() {
        let r = RendererConfig {
            mode: RendererMode::None,
            ..Default::default()
        };
        assert_eq!(r.min_deadline_for_full_ladder_ms(), 0);
        assert_eq!(r.cdp_tier_count(), 0);
    }

    // ==================================================================
    // ChromePoolConfig / resolve_interactive_reserve
    // ==================================================================

    #[test]
    fn chrome_pool_config_defaults() {
        let p = ChromePoolConfig::default();
        assert_eq!(p.size, None);
        assert_eq!(p.reserved_interactive_renders, None);
        assert_eq!(p.recycle_after_navs, 1);
        assert_eq!(p.idle_timeout_secs, 300);
        assert_eq!(p.health_check_secs, 60);
        assert_eq!(p.shutdown_drain_secs, 30);
    }

    #[test]
    fn chrome_pool_config_toml_parse() {
        let p: ChromePoolConfig = toml::from_str(
            r#"
            size = 16
            reserved_interactive_renders = 4
            recycle_after_navs = 10
            idle_timeout_secs = 60
            health_check_secs = 15
            shutdown_drain_secs = 5
            "#,
        )
        .unwrap();
        assert_eq!(p.size, Some(16));
        assert_eq!(p.reserved_interactive_renders, Some(4));
        assert_eq!(p.recycle_after_navs, 10);
        assert_eq!(p.idle_timeout_secs, 60);
        assert_eq!(p.health_check_secs, 15);
        assert_eq!(p.shutdown_drain_secs, 5);
    }

    #[test]
    fn resolve_interactive_reserve_none_is_quarter_of_total() {
        assert_eq!(resolve_interactive_reserve(None, 16), 4);
        assert_eq!(resolve_interactive_reserve(None, 8), 2);
    }

    #[test]
    fn resolve_interactive_reserve_none_floors_at_one() {
        // total/4 rounds down to 0 for small totals; floored at 1.
        assert_eq!(resolve_interactive_reserve(None, 3), 1);
        assert_eq!(resolve_interactive_reserve(None, 0), 1);
    }

    #[test]
    fn resolve_interactive_reserve_some_zero_disables_reservation() {
        assert_eq!(resolve_interactive_reserve(Some(0), 16), 0);
    }

    #[test]
    fn resolve_interactive_reserve_some_explicit_value_wins() {
        assert_eq!(resolve_interactive_reserve(Some(7), 16), 7);
        // Explicit value is not clamped by this function (caller clamps).
        assert_eq!(resolve_interactive_reserve(Some(999), 16), 999);
    }

    // ==================================================================
    // EscalationConfig
    // ==================================================================

    #[test]
    fn escalation_config_defaults() {
        let e = EscalationConfig::default();
        assert!(!e.enabled);
        assert_eq!(e.waterfall_timeout_ms, 8_000);
        assert_eq!(e.global_timeout_ms, 60_000);
        assert!(!e.residential_proxy);
        assert_eq!(e.proxy_country, "us");
    }

    #[test]
    fn escalation_config_toml_parse() {
        let e: EscalationConfig = toml::from_str(
            r#"
            enabled = true
            waterfall_timeout_ms = 5000
            global_timeout_ms = 30000
            residential_proxy = true
            proxy_country = "de"
            "#,
        )
        .unwrap();
        assert!(e.enabled);
        assert_eq!(e.waterfall_timeout_ms, 5000);
        assert_eq!(e.global_timeout_ms, 30000);
        assert!(e.residential_proxy);
        assert_eq!(e.proxy_country, "de");
    }

    #[test]
    fn env_var_renderer_escalation_enabled() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_RENDERER__ESCALATION__ENABLED", "true") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_RENDERER__ESCALATION__ENABLED") };
        assert!(cfg.renderer.escalation.enabled);
    }

    #[test]
    fn env_var_renderer_escalation_proxy_country() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_RENDERER__ESCALATION__PROXY_COUNTRY", "fr") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_RENDERER__ESCALATION__PROXY_COUNTRY") };
        assert_eq!(cfg.renderer.escalation.proxy_country, "fr");
    }

    // ==================================================================
    // AntibotConfig
    // ==================================================================

    #[test]
    fn antibot_config_defaults() {
        let a = AntibotConfig::default();
        assert!(a.enabled);
        assert!(!a.escalate_on_signal);
        assert!(a.escalate_in_failover);
    }

    #[test]
    fn antibot_config_toml_parse() {
        let a: AntibotConfig = toml::from_str(
            r#"
            enabled = false
            escalate_on_signal = true
            escalate_in_failover = false
            "#,
        )
        .unwrap();
        assert!(!a.enabled);
        assert!(a.escalate_on_signal);
        assert!(!a.escalate_in_failover);
    }

    #[test]
    fn env_var_renderer_antibot_escalate_on_signal() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_RENDERER__ANTIBOT__ESCALATE_ON_SIGNAL", "true") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_RENDERER__ANTIBOT__ESCALATE_ON_SIGNAL") };
        assert!(cfg.renderer.antibot.escalate_on_signal);
    }

    // ==================================================================
    // StealthConfig
    // ==================================================================

    #[test]
    fn stealth_config_defaults() {
        let s = StealthConfig::default();
        assert!(!s.enabled);
        assert!(s.user_agents.is_empty());
        assert_eq!(s.jitter_factor, 0.2);
        assert!(s.inject_headers);
    }

    #[test]
    fn stealth_config_toml_parse_user_agents() {
        let s: StealthConfig = toml::from_str(
            r#"
            enabled = true
            user_agents = ["ua-one", "ua-two"]
            jitter_factor = 0.5
            inject_headers = false
            "#,
        )
        .unwrap();
        assert!(s.enabled);
        assert_eq!(s.user_agents, vec!["ua-one", "ua-two"]);
        assert_eq!(s.jitter_factor, 0.5);
        assert!(!s.inject_headers);
    }

    #[test]
    fn stealth_config_jitter_factor_not_range_checked_at_parse_time() {
        // No validation exists on this field today; document the current
        // (permissive) behavior rather than assume a clamp that isn't there.
        let s: StealthConfig = toml::from_str("jitter_factor = 5.0").unwrap();
        assert_eq!(s.jitter_factor, 5.0);
    }

    #[test]
    fn env_var_renderer_stealth_enabled() {
        // stealth lives under crawler, not renderer — see CrawlerConfig::stealth.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_CRAWLER__STEALTH__ENABLED", "true") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_CRAWLER__STEALTH__ENABLED") };
        assert!(cfg.crawler.stealth.enabled);
    }

    // ==================================================================
    // CrawlerConfig
    // ==================================================================

    #[test]
    fn crawler_config_default_max_concurrency_is_10() {
        assert_eq!(CrawlerConfig::default().max_concurrency, 10);
    }

    #[test]
    fn crawler_config_default_max_batch_urls_is_10000() {
        assert_eq!(CrawlerConfig::default().max_batch_urls, 10_000);
    }

    #[test]
    fn crawler_config_default_max_extract_urls_is_50() {
        assert_eq!(CrawlerConfig::default().max_extract_urls, 50);
    }

    #[test]
    fn crawler_config_default_max_batch_concurrency_is_100() {
        assert_eq!(CrawlerConfig::default().max_batch_concurrency, 100);
    }

    #[test]
    fn crawler_config_default_max_aggregate_batch_pipelines_is_unbounded() {
        assert_eq!(CrawlerConfig::default().max_aggregate_batch_pipelines, 0);
    }

    #[test]
    fn crawler_config_default_requests_per_second() {
        assert_eq!(CrawlerConfig::default().requests_per_second, 10.0);
    }

    #[test]
    fn crawler_config_default_respect_robots_txt_true() {
        assert!(CrawlerConfig::default().respect_robots_txt);
    }

    #[test]
    fn crawler_config_default_user_agent_is_modern_chrome() {
        let ua = CrawlerConfig::default().user_agent;
        assert!(ua.contains("Chrome/150.0.0.0"));
        assert!(!ua.contains("CRW/0.1"), "legacy UA must not resurface");
    }

    #[test]
    fn crawler_config_default_depth_and_pages() {
        let c = CrawlerConfig::default();
        assert_eq!(c.default_max_depth, 2);
        assert_eq!(c.default_max_pages, 100);
    }

    #[test]
    fn crawler_config_default_job_ttl_secs_is_one_hour() {
        assert_eq!(CrawlerConfig::default().job_ttl_secs, 3600);
    }

    #[test]
    fn crawler_config_default_proxy_and_proxy_list_empty() {
        let c = CrawlerConfig::default();
        assert_eq!(c.proxy, None);
        assert!(c.proxy_list.is_empty());
    }

    #[test]
    fn crawler_config_default_proxy_rotation_is_sticky_per_host() {
        assert_eq!(
            CrawlerConfig::default().proxy_rotation,
            crate::proxy::ProxyRotation::StickyPerHost
        );
    }

    #[test]
    fn crawler_config_default_stealth_matches_stealth_default() {
        assert!(!CrawlerConfig::default().stealth.enabled);
    }

    #[test]
    fn crawler_config_per_host_interactive_reserve_default_is_one() {
        assert_eq!(CrawlerConfig::default().per_host_interactive_reserve, 1);
    }

    #[test]
    fn crawler_config_toml_parse_all_scalar_fields() {
        let c: CrawlerConfig = toml::from_str(
            r#"
            max_concurrency = 50
            requests_per_second = 25.5
            respect_robots_txt = false
            user_agent = "custom-agent/1.0"
            default_max_depth = 5
            default_max_pages = 1000
            job_ttl_secs = 7200
            per_host_min_interval_ms = 250
            per_host_max_concurrent = 3
            per_host_interactive_reserve = 2
            max_batch_urls = 500
            max_extract_urls = 10
            max_batch_concurrency = 40
            max_aggregate_batch_pipelines = 200
            "#,
        )
        .unwrap();
        assert_eq!(c.max_concurrency, 50);
        assert_eq!(c.requests_per_second, 25.5);
        assert!(!c.respect_robots_txt);
        assert_eq!(c.user_agent, "custom-agent/1.0");
        assert_eq!(c.default_max_depth, 5);
        assert_eq!(c.default_max_pages, 1000);
        assert_eq!(c.job_ttl_secs, 7200);
        assert_eq!(c.per_host_min_interval_ms, 250);
        assert_eq!(c.per_host_max_concurrent, 3);
        assert_eq!(c.per_host_interactive_reserve, 2);
        assert_eq!(c.max_batch_urls, 500);
        assert_eq!(c.max_extract_urls, 10);
        assert_eq!(c.max_batch_concurrency, 40);
        assert_eq!(c.max_aggregate_batch_pipelines, 200);
    }

    #[test]
    fn crawler_proxy_list_from_toml_array() {
        let c: CrawlerConfig =
            toml::from_str(r#"proxy_list = ["http://p1:8080", "http://p2:8080"]"#).unwrap();
        assert_eq!(c.proxy_list, vec!["http://p1:8080", "http://p2:8080"]);
    }

    #[test]
    fn crawler_proxy_list_from_comma_string() {
        let c: CrawlerConfig =
            toml::from_str(r#"proxy_list = "http://p1:8080,http://p2:8080""#).unwrap();
        assert_eq!(c.proxy_list, vec!["http://p1:8080", "http://p2:8080"]);
    }

    #[test]
    fn crawler_proxy_list_from_json_array_string() {
        let c: CrawlerConfig =
            toml::from_str(r#"proxy_list = "[\"http://p1:8080\",\"http://p2:8080\"]""#).unwrap();
        assert_eq!(c.proxy_list, vec!["http://p1:8080", "http://p2:8080"]);
    }

    #[test]
    fn crawler_proxy_list_comma_string_filters_empty_entries() {
        let c: CrawlerConfig =
            toml::from_str(r#"proxy_list = "http://p1:8080,,  ,http://p2:8080""#).unwrap();
        assert_eq!(c.proxy_list, vec!["http://p1:8080", "http://p2:8080"]);
    }

    #[test]
    fn crawler_proxy_list_empty_string_yields_empty_vec() {
        let c: CrawlerConfig = toml::from_str(r#"proxy_list = """#).unwrap();
        assert!(c.proxy_list.is_empty());
    }

    #[test]
    fn env_var_crawler_max_concurrency_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_CRAWLER__MAX_CONCURRENCY", "42") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_CRAWLER__MAX_CONCURRENCY") };
        assert_eq!(cfg.crawler.max_concurrency, 42);
    }

    #[test]
    fn env_var_crawler_max_batch_urls_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_CRAWLER__MAX_BATCH_URLS", "99") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_CRAWLER__MAX_BATCH_URLS") };
        assert_eq!(cfg.crawler.max_batch_urls, 99);
    }

    #[test]
    fn env_var_crawler_respect_robots_txt_false() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_CRAWLER__RESPECT_ROBOTS_TXT", "false") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_CRAWLER__RESPECT_ROBOTS_TXT") };
        assert!(!cfg.crawler.respect_robots_txt);
    }

    #[test]
    fn env_var_crawler_max_concurrency_malformed_errors() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_CRAWLER__MAX_CONCURRENCY", "ten") };
        let result = AppConfig::load();
        unsafe { std::env::remove_var("CRW_CRAWLER__MAX_CONCURRENCY") };
        assert!(result.is_err());
    }

    // ==================================================================
    // ExtractionConfig
    // ==================================================================

    #[test]
    fn extraction_config_defaults() {
        let e = ExtractionConfig::default();
        assert_eq!(e.default_format, "markdown");
        assert!(e.only_main_content);
        assert!(e.llm.is_none());
        assert!(e.domain_selectors.is_empty());
        assert_eq!(e.http_retry_threshold_bytes, 100);
        assert_eq!(e.lightpanda_retry_threshold_bytes, 2000);
        assert!(e.max_concurrent_extracts >= 2, "must be floored at 2");
        assert_eq!(e.reserved_interactive_extracts, None);
        assert!(!e.normalize_tables);
    }

    #[test]
    fn extraction_config_llm_fallback_default_matches_struct_default() {
        let e = ExtractionConfig::default();
        assert!(!e.llm_fallback.enable);
        assert_eq!(e.llm_fallback.quality_threshold, 0.3);
    }

    #[test]
    fn extraction_config_toml_parse_domain_selectors_map() {
        let e: ExtractionConfig = toml::from_str(
            r##"
            [domain_selectors]
            "example.com" = "article.main"
            "docs.example.com" = "#content"
            "##,
        )
        .unwrap();
        assert_eq!(
            e.domain_selectors.get("example.com").map(String::as_str),
            Some("article.main")
        );
        assert_eq!(
            e.domain_selectors
                .get("docs.example.com")
                .map(String::as_str),
            Some("#content")
        );
    }

    #[test]
    fn extraction_config_toml_scalar_overrides() {
        let e: ExtractionConfig = toml::from_str(
            r#"
            default_format = "html"
            only_main_content = false
            http_retry_threshold_bytes = 250
            lightpanda_retry_threshold_bytes = 4000
            max_concurrent_extracts = 16
            reserved_interactive_extracts = 3
            normalize_tables = true
            "#,
        )
        .unwrap();
        assert_eq!(e.default_format, "html");
        assert!(!e.only_main_content);
        assert_eq!(e.http_retry_threshold_bytes, 250);
        assert_eq!(e.lightpanda_retry_threshold_bytes, 4000);
        assert_eq!(e.max_concurrent_extracts, 16);
        assert_eq!(e.reserved_interactive_extracts, Some(3));
        assert!(e.normalize_tables);
    }

    #[test]
    fn extraction_config_empty_toml_uses_all_defaults() {
        let e: ExtractionConfig = toml::from_str("").unwrap();
        assert_eq!(e.default_format, "markdown");
        assert!(e.only_main_content);
    }

    // ==================================================================
    // LlmFallbackConfig
    // ==================================================================

    #[test]
    fn llm_fallback_config_defaults() {
        let l = LlmFallbackConfig::default();
        assert!(!l.enable);
        assert_eq!(l.quality_threshold, 0.3);
        assert_eq!(l.max_html_bytes, 100_000);
        assert!(!l.always_run);
    }

    #[test]
    fn llm_fallback_config_toml_parse() {
        let l: LlmFallbackConfig = toml::from_str(
            r#"
            enable = true
            quality_threshold = 0.8
            max_html_bytes = 50000
            always_run = true
            "#,
        )
        .unwrap();
        assert!(l.enable);
        assert_eq!(l.quality_threshold, 0.8);
        assert_eq!(l.max_html_bytes, 50000);
        assert!(l.always_run);
    }

    #[test]
    fn env_var_extraction_llm_fallback_enable() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_EXTRACTION__LLM_FALLBACK__ENABLE", "true") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_EXTRACTION__LLM_FALLBACK__ENABLE") };
        assert!(cfg.extraction.llm_fallback.enable);
    }

    // ==================================================================
    // LlmConfig — provider/api_key/model/base_url nesting under
    // extraction.llm, per the documented CRW_EXTRACTION__LLM__* env vars.
    // ==================================================================

    #[test]
    fn llm_config_programmatic_default() {
        let l = LlmConfig::default();
        assert_eq!(l.provider, "anthropic");
        assert_eq!(l.api_key, "");
        assert_eq!(l.model, "claude-sonnet-4-20250514");
        assert_eq!(l.base_url, None);
        assert_eq!(l.max_tokens, 4096);
        assert_eq!(l.azure_api_version, None);
        assert_eq!(l.max_concurrency, 4);
        assert_eq!(l.reserved_interactive_llm, None);
        assert_eq!(l.max_html_bytes, 100_000);
        assert_eq!(l.require_byok_header, None);
        assert_eq!(l.temperature, None);
        assert_eq!(l.reasoning_effort, None);
    }

    #[test]
    fn llm_config_deserialize_requires_api_key() {
        // api_key has no #[serde(default)] — a config that omits it must fail
        // to deserialize rather than silently substitute an empty string.
        let result: Result<LlmConfig, _> = toml::from_str(r#"provider = "deepseek""#);
        assert!(result.is_err(), "missing api_key must be a hard error");
    }

    #[test]
    fn llm_config_toml_parse_minimal_only_api_key() {
        let l: LlmConfig = toml::from_str(r#"api_key = "sk-test""#).unwrap();
        assert_eq!(l.api_key, "sk-test");
        assert_eq!(l.provider, "anthropic", "provider falls back to default");
        assert_eq!(l.model, "claude-sonnet-4-20250514");
        assert_eq!(l.max_tokens, 4096);
    }

    #[test]
    fn llm_config_toml_parse_full() {
        let l: LlmConfig = toml::from_str(
            r#"
            provider = "azure"
            api_key = "sk-azure"
            model = "gpt-4o"
            base_url = "https://my-azure.example/v1"
            max_tokens = 8192
            azure_api_version = "2024-05-01-preview"
            max_concurrency = 12
            reserved_interactive_llm = 2
            max_html_bytes = 250000
            require_byok_header = "x-crw-llm-key"
            temperature = 0.0
            reasoning_effort = "high"
            "#,
        )
        .unwrap();
        assert_eq!(l.provider, "azure");
        assert_eq!(l.api_key, "sk-azure");
        assert_eq!(l.model, "gpt-4o");
        assert_eq!(l.base_url.as_deref(), Some("https://my-azure.example/v1"));
        assert_eq!(l.max_tokens, 8192);
        assert_eq!(l.azure_api_version.as_deref(), Some("2024-05-01-preview"));
        assert_eq!(l.max_concurrency, 12);
        assert_eq!(l.reserved_interactive_llm, Some(2));
        assert_eq!(l.max_html_bytes, 250000);
        assert_eq!(l.require_byok_header.as_deref(), Some("x-crw-llm-key"));
        assert_eq!(l.temperature, Some(0.0));
        assert_eq!(l.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn env_var_extraction_llm_provider_api_key_model_base_url() {
        // The exact double-underscore nesting documented for
        // CRW_EXTRACTION__LLM__{PROVIDER,API_KEY,MODEL,BASE_URL}.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe {
            std::env::set_var("CRW_EXTRACTION__LLM__PROVIDER", "openai");
            std::env::set_var("CRW_EXTRACTION__LLM__API_KEY", "sk-env-key");
            std::env::set_var("CRW_EXTRACTION__LLM__MODEL", "gpt-4o-mini");
            std::env::set_var("CRW_EXTRACTION__LLM__BASE_URL", "https://api.openai.com/v1");
        }
        let cfg = AppConfig::load().unwrap();
        unsafe {
            std::env::remove_var("CRW_EXTRACTION__LLM__PROVIDER");
            std::env::remove_var("CRW_EXTRACTION__LLM__API_KEY");
            std::env::remove_var("CRW_EXTRACTION__LLM__MODEL");
            std::env::remove_var("CRW_EXTRACTION__LLM__BASE_URL");
        }
        let llm = cfg
            .extraction
            .llm
            .expect("env vars must construct LlmConfig");
        assert_eq!(llm.provider, "openai");
        assert_eq!(llm.api_key, "sk-env-key");
        assert_eq!(llm.model, "gpt-4o-mini");
        assert_eq!(llm.base_url.as_deref(), Some("https://api.openai.com/v1"));
    }

    #[test]
    fn env_var_extraction_llm_partial_set_still_requires_api_key() {
        // Setting only PROVIDER via env, with no user config file supplying
        // api_key, must still fail deserialization (api_key is required).
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        let tmp = std::env::temp_dir().join(format!("crw-llm-partial-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).ok();
        unsafe {
            std::env::set_var("CRW_USER_CONFIG_DIR", &tmp);
            std::env::set_var("CRW_EXTRACTION__LLM__PROVIDER", "openai");
        }
        let result = AppConfig::load();
        unsafe {
            std::env::remove_var("CRW_USER_CONFIG_DIR");
            std::env::remove_var("CRW_EXTRACTION__LLM__PROVIDER");
        }
        std::fs::remove_dir_all(&tmp).ok();
        assert!(
            result.is_err(),
            "provider alone must not satisfy the required api_key field"
        );
    }

    // ==================================================================
    // AuthConfig
    // ==================================================================

    #[test]
    fn auth_config_default_empty() {
        assert!(AuthConfig::default().api_keys.is_empty());
    }

    #[test]
    fn auth_config_api_keys_from_toml_array() {
        let a: AuthConfig = toml::from_str(r#"api_keys = ["key-one", "key-two"]"#).unwrap();
        assert_eq!(a.api_keys, vec!["key-one", "key-two"]);
    }

    #[test]
    fn auth_config_api_keys_from_comma_string() {
        let a: AuthConfig = toml::from_str(r#"api_keys = "key-one,key-two""#).unwrap();
        assert_eq!(a.api_keys, vec!["key-one", "key-two"]);
    }

    #[test]
    fn auth_config_api_keys_from_json_array_string() {
        let a: AuthConfig = toml::from_str(r#"api_keys = "[\"key-one\",\"key-two\"]""#).unwrap();
        assert_eq!(a.api_keys, vec!["key-one", "key-two"]);
    }

    #[test]
    fn auth_config_api_keys_comma_string_filters_empty_entries() {
        let a: AuthConfig = toml::from_str(r#"api_keys = "key-one,,key-two,""#).unwrap();
        assert_eq!(a.api_keys, vec!["key-one", "key-two"]);
    }

    #[test]
    fn env_var_auth_api_keys() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_AUTH__API_KEYS", "envkey1,envkey2") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_AUTH__API_KEYS") };
        assert_eq!(cfg.auth.api_keys, vec!["envkey1", "envkey2"]);
    }

    // ==================================================================
    // MapConfig / MapUrlFilterConfig
    // ==================================================================

    #[test]
    fn map_config_default_matches_url_filter_default() {
        let m = MapConfig::default();
        assert!(m.url_filter.strip_tracking_params);
        assert!(m.url_filter.drop_action_urls);
        assert!(!m.url_filter.gov_tld_drop_actions);
    }

    #[test]
    fn map_url_filter_config_defaults() {
        let f = MapUrlFilterConfig::default();
        assert!(f.strip_tracking_params);
        assert!(f.drop_action_urls);
        assert!(!f.gov_tld_drop_actions);
        assert!(f.extra_tracking_params.is_empty());
        assert!(f.extra_action_params.is_empty());
        assert!(f.extra_preserve_params.is_empty());
    }

    #[test]
    fn default_true_filter_helper() {
        assert!(default_true_filter());
    }

    #[test]
    fn map_url_filter_toml_parse_extra_params() {
        let f: MapUrlFilterConfig = toml::from_str(
            r#"
            strip_tracking_params = false
            drop_action_urls = false
            gov_tld_drop_actions = true
            extra_tracking_params = ["ref", "src"]
            extra_action_params = ["delete", "logout"]
            extra_preserve_params = ["page"]
            "#,
        )
        .unwrap();
        assert!(!f.strip_tracking_params);
        assert!(!f.drop_action_urls);
        assert!(f.gov_tld_drop_actions);
        assert_eq!(f.extra_tracking_params, vec!["ref", "src"]);
        assert_eq!(f.extra_action_params, vec!["delete", "logout"]);
        assert_eq!(f.extra_preserve_params, vec!["page"]);
    }

    #[test]
    fn map_url_filter_partial_toml_keeps_other_defaults() {
        let f: MapUrlFilterConfig = toml::from_str("gov_tld_drop_actions = true").unwrap();
        assert!(f.gov_tld_drop_actions);
        assert!(f.strip_tracking_params, "unset field keeps default true");
        assert!(f.drop_action_urls, "unset field keeps default true");
    }

    #[test]
    fn env_var_map_url_filter_gov_tld_drop_actions() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_MAP__URL_FILTER__GOV_TLD_DROP_ACTIONS", "true") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_MAP__URL_FILTER__GOV_TLD_DROP_ACTIONS") };
        assert!(cfg.map.url_filter.gov_tld_drop_actions);
    }

    // ==================================================================
    // DocumentConfig
    // ==================================================================

    #[test]
    fn document_config_defaults_comprehensive() {
        let d = DocumentConfig::default();
        assert!(d.enabled);
        assert_eq!(d.max_pages, 0, "0 = no limit");
        assert!(!d.attempt_scanned);
        assert_eq!(d.max_upload_bytes, 52_428_800, "50 MiB");
        assert_eq!(d.upload_concurrency, 4);
        assert_eq!(d.max_concurrent_parses, 4);
        assert_eq!(d.reserved_interactive_parses, None);
        assert_eq!(d.parse_timeout_ms, 30_000);
        assert_eq!(d.max_decompressed_bytes, 104_857_600, "100 MiB");
        assert!(!d.sandbox);
        assert_eq!(d.sandbox_memory_bytes, 536_870_912, "512 MiB");
    }

    #[test]
    fn document_config_toml_parse_full() {
        let d: DocumentConfig = toml::from_str(
            r#"
            enabled = false
            max_pages = 20
            attempt_scanned = true
            max_upload_bytes = 1000
            upload_concurrency = 8
            max_concurrent_parses = 16
            reserved_interactive_parses = 2
            parse_timeout_ms = 5000
            max_decompressed_bytes = 2000
            sandbox = true
            sandbox_memory_bytes = 999
            "#,
        )
        .unwrap();
        assert!(!d.enabled);
        assert_eq!(d.max_pages, 20);
        assert!(d.attempt_scanned);
        assert_eq!(d.max_upload_bytes, 1000);
        assert_eq!(d.upload_concurrency, 8);
        assert_eq!(d.max_concurrent_parses, 16);
        assert_eq!(d.reserved_interactive_parses, Some(2));
        assert_eq!(d.parse_timeout_ms, 5000);
        assert_eq!(d.max_decompressed_bytes, 2000);
        assert!(d.sandbox);
        assert_eq!(d.sandbox_memory_bytes, 999);
    }

    #[test]
    fn document_config_empty_toml_uses_container_default() {
        // DocumentConfig uses a struct-level #[serde(default)], so a partial
        // TOML falls back to the whole Default impl for unset fields.
        let d: DocumentConfig = toml::from_str("max_pages = 5").unwrap();
        assert_eq!(d.max_pages, 5);
        assert!(d.enabled, "unset field keeps Default::default()");
        assert_eq!(d.max_upload_bytes, 52_428_800);
    }

    #[test]
    fn env_var_document_enabled_false() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_DOCUMENT__ENABLED", "false") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_DOCUMENT__ENABLED") };
        assert!(!cfg.document.enabled);
    }

    #[test]
    fn env_var_document_max_pages() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_DOCUMENT__MAX_PAGES", "12") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_DOCUMENT__MAX_PAGES") };
        assert_eq!(cfg.document.max_pages, 12);
    }

    // ==================================================================
    // ClientConfig
    // ==================================================================

    #[test]
    fn client_config_default_both_none() {
        let c = ClientConfig::default();
        assert_eq!(c.api_url, None);
        assert_eq!(c.api_key, None);
    }

    #[test]
    fn client_config_toml_parse() {
        let c: ClientConfig = toml::from_str(
            r#"
            api_url = "https://api.fastcrw.com"
            api_key = "crw_live_abc"
            "#,
        )
        .unwrap();
        assert_eq!(c.api_url.as_deref(), Some("https://api.fastcrw.com"));
        assert_eq!(c.api_key.as_deref(), Some("crw_live_abc"));
    }

    #[test]
    fn client_config_partial_toml_leaves_api_key_none() {
        let c: ClientConfig = toml::from_str(r#"api_url = "https://api.fastcrw.com""#).unwrap();
        assert_eq!(c.api_url.as_deref(), Some("https://api.fastcrw.com"));
        assert_eq!(c.api_key, None);
    }

    #[test]
    fn app_config_client_api_url_has_no_baked_in_default() {
        // NOTE: crw-core's ClientConfig always defaults api_url to None — there
        // is no bundled cloud host baked into this crate's config resolution.
        // The cloud-vs-localhost default lives downstream in crw-cli (e.g.
        // `crates/crw-cli/src/commands/search.rs::resolve_search_target`),
        // which is out of scope for this file. Document the actual behavior
        // here rather than assert a value config.rs does not produce.
        assert_eq!(AppConfig::default().client.api_url, None);
    }

    // ==================================================================
    // SearchConfig
    // ==================================================================

    #[test]
    fn search_config_defaults_comprehensive() {
        let s = SearchConfig::default();
        assert!(s.enabled);
        assert_eq!(s.search_backend_url, None);
        assert_eq!(s.searxng_url, None);
        assert_eq!(s.openalex_api_key, None);
        assert_eq!(s.openalex_mailto, None);
        assert_eq!(s.s2_api_key, None);
        assert_eq!(s.timeout_ms, 15_000);
        assert_eq!(s.default_limit, 5);
        assert_eq!(s.max_limit, 20);
        assert_eq!(
            s.research_engines,
            vec!["arxiv", "crossref", "google scholar", "semantic scholar"]
        );
        assert_eq!(s.github_engines, vec!["github"]);
        assert!(s.rerank_enabled);
        assert!(!s.query_expand);
        assert_eq!(s.query_expand_variants, 1);
        assert!(!s.pipeline_overlap);
        assert!(!s.multi_round);
        assert!(!s.snippet_first);
        assert!(!s.passage_select);
        assert!(!s.answer_bm25_select);
        assert!(!s.page2_fallback);
        assert!(!s.answer_calibrated);
        assert!(!s.answer_guarded);
        assert!(!s.use_structured_sources);
        assert!(!s.wikidata_lookup);
        assert!(!s.snippet_fallback);
        assert!(!s.rerank_relevance);
        assert!(!s.answer_list_format);
    }

    #[test]
    fn search_config_resolve_backend_url_none_when_both_unset() {
        assert_eq!(SearchConfig::default().resolve_backend_url(), None);
    }

    #[test]
    fn search_config_toml_parse_gated_flags() {
        let s: SearchConfig = toml::from_str(
            r#"
            query_expand = true
            query_expand_variants = 3
            answer_guarded = true
            wikidata_lookup = true
            use_structured_sources = true
            "#,
        )
        .unwrap();
        assert!(s.query_expand);
        assert_eq!(s.query_expand_variants, 3);
        assert!(s.answer_guarded);
        assert!(s.wikidata_lookup);
        assert!(s.use_structured_sources);
        // Untouched fields keep their defaults.
        assert!(s.rerank_enabled);
        assert!(!s.multi_round);
    }

    #[test]
    fn env_var_search_query_expand_variants() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_SEARCH__QUERY_EXPAND_VARIANTS", "5") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_SEARCH__QUERY_EXPAND_VARIANTS") };
        assert_eq!(cfg.search.query_expand_variants, 5);
    }

    #[test]
    fn env_var_search_answer_guarded() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_SEARCH__ANSWER_GUARDED", "true") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_SEARCH__ANSWER_GUARDED") };
        assert!(cfg.search.answer_guarded);
    }

    #[test]
    fn env_var_search_max_limit_malformed_errors() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_SEARCH__MAX_LIMIT", "twenty") };
        let result = AppConfig::load();
        unsafe { std::env::remove_var("CRW_SEARCH__MAX_LIMIT") };
        assert!(result.is_err());
    }

    // ==================================================================
    // McpConfig (extra coverage beyond the existing three tests)
    // ==================================================================

    #[test]
    fn mcp_config_explicit_false_stays_false() {
        let cfg: AppConfig = toml::from_str("[mcp]\nhide_credits = false").unwrap();
        assert!(!cfg.mcp.hide_credits);
    }

    #[test]
    fn env_var_mcp_hide_credits_false_overrides_true_file() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        let tmp = std::env::temp_dir().join(format!("crw-mcp-false-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("config.toml"), "[mcp]\nhide_credits = true\n").unwrap();
        unsafe {
            std::env::set_var("CRW_USER_CONFIG_DIR", &tmp);
            std::env::set_var("CRW_MCP__HIDE_CREDITS", "false");
        }
        let cfg = AppConfig::load().unwrap();
        unsafe {
            std::env::remove_var("CRW_USER_CONFIG_DIR");
            std::env::remove_var("CRW_MCP__HIDE_CREDITS");
        }
        std::fs::remove_dir_all(&tmp).ok();
        assert!(
            !cfg.mcp.hide_credits,
            "env var must win over user config file"
        );
    }

    // ==================================================================
    // AppConfig top level / malformed & unknown-key handling
    // ==================================================================

    #[test]
    fn app_config_default_assembles_all_section_defaults() {
        let a = AppConfig::default();
        assert_eq!(a.server.port, 3000);
        assert_eq!(a.renderer.mode, RendererMode::Auto);
        assert_eq!(a.crawler.max_concurrency, 10);
        assert_eq!(a.extraction.default_format, "markdown");
        assert!(a.auth.api_keys.is_empty());
        assert_eq!(a.request.deadline_ms_default, 8000);
        assert!(a.search.enabled);
        assert!(a.map.url_filter.strip_tracking_params);
        assert!(a.document.enabled);
        assert_eq!(a.client.api_url, None);
        assert!(!a.mcp.hide_credits);
    }

    #[test]
    fn app_config_empty_toml_uses_every_default() {
        let a: AppConfig = toml::from_str("").unwrap();
        assert_eq!(a.server.port, 3000);
        assert_eq!(a.crawler.max_concurrency, 10);
        assert_eq!(a.crawler.max_batch_urls, 10_000);
    }

    #[test]
    fn app_config_unknown_toml_keys_are_ignored() {
        // No #[serde(deny_unknown_fields)] anywhere in this file — an unknown
        // top-level or nested key must not fail deserialization.
        let a: AppConfig = toml::from_str(
            r#"
            totally_unknown_top_level_key = "surprise"

            [server]
            port = 4321
            another_unknown_key = 12345
            "#,
        )
        .unwrap();
        assert_eq!(a.server.port, 4321);
    }

    #[test]
    fn app_config_malformed_toml_syntax_errors() {
        let result: Result<AppConfig, _> = toml::from_str("this is not [ valid toml");
        assert!(result.is_err());
    }

    #[test]
    fn app_config_wrong_type_for_numeric_field_errors() {
        let result: Result<AppConfig, _> = toml::from_str(
            r#"
            [server]
            port = "not-a-number"
            "#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn app_config_deeply_nested_unknown_section_ignored() {
        let a: AppConfig = toml::from_str(
            r#"
            [some_future_section]
            [some_future_section.nested]
            knob = true
            "#,
        )
        .unwrap();
        // Falls through to defaults everywhere else.
        assert_eq!(a.server.port, 3000);
    }

    #[test]
    fn app_config_toml_unicode_and_long_strings_round_trip() {
        let long_ua = "x".repeat(5000);
        let toml_str = format!(
            r#"
            [crawler]
            user_agent = "{long_ua}"

            [server]
            host = "アクセス制御.example"
            "#
        );
        let a: AppConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(a.crawler.user_agent.len(), 5000);
        assert_eq!(a.server.host, "アクセス制御.example");
    }

    #[test]
    fn app_config_empty_string_fields_are_preserved_not_defaulted() {
        // An explicit empty string is a valid value distinct from "unset" for
        // plain (non-Option, non-normalizing) String fields.
        let a: AppConfig = toml::from_str(
            r#"[crawler]
user_agent = """#,
        )
        .unwrap();
        assert_eq!(a.crawler.user_agent, "");
    }

    // ==================================================================
    // effective_deadline_ms / effective_request_timeout_secs — extra edges
    // ==================================================================

    #[test]
    fn effective_deadline_explicit_zero_is_respected() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.effective_deadline_ms(Some(0), None), 0);
    }

    #[test]
    #[cfg(feature = "cdp")]
    fn effective_deadline_wait_for_exactly_at_spa_default_adds_nothing() {
        let mut cfg = AppConfig::default();
        cfg.request.auto_extend_deadline_for_ladder = true;
        cfg.renderer = renderer_with_chrome_only(30_000);
        let base = cfg.effective_deadline_ms(None, None);
        // wait_for exactly at the 8000ms SPA default → saturating_sub is 0.
        assert_eq!(cfg.effective_deadline_ms(None, Some(8_000)), base);
    }

    #[test]
    #[cfg(feature = "cdp")]
    fn effective_deadline_wait_for_clamped_to_max_wait_for_ms() {
        let mut cfg = AppConfig::default();
        cfg.request.auto_extend_deadline_for_ladder = true;
        cfg.renderer = renderer_with_chrome_only(30_000);
        // A pathological wait_for far beyond MAX_WAIT_FOR_MS must be clamped,
        // not applied verbatim.
        let huge = cfg.effective_deadline_ms(None, Some(10_000_000));
        let at_cap = cfg.effective_deadline_ms(None, Some(MAX_WAIT_FOR_MS));
        assert_eq!(huge, at_cap);
    }

    #[test]
    fn effective_request_timeout_baseline_never_below_map_ceiling_plus_buffer() {
        let mut cfg = AppConfig::default();
        cfg.request.auto_extend_deadline_for_ladder = true;
        cfg.server.request_timeout_secs = 1; // absurdly low operator setting
        // 300s map ceiling + 5s buffer floors the result regardless.
        assert!(cfg.effective_request_timeout_secs() >= 305);
    }

    // ==================================================================
    // non_empty_trimmed_env / user_config_path
    // ==================================================================

    #[test]
    fn non_empty_trimmed_env_trims_and_filters_blank() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("CRW_TEST_TRIM_VAR", "  hello world  ") };
        assert_eq!(
            non_empty_trimmed_env("CRW_TEST_TRIM_VAR").as_deref(),
            Some("hello world")
        );
        unsafe { std::env::set_var("CRW_TEST_TRIM_VAR", "   ") };
        assert_eq!(non_empty_trimmed_env("CRW_TEST_TRIM_VAR"), None);
        unsafe { std::env::remove_var("CRW_TEST_TRIM_VAR") };
        assert_eq!(non_empty_trimmed_env("CRW_TEST_TRIM_VAR"), None);
    }

    #[test]
    fn user_config_path_falls_back_to_home_when_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let had_override = std::env::var_os("CRW_USER_CONFIG_DIR");
        unsafe { std::env::remove_var("CRW_USER_CONFIG_DIR") };
        let home = std::env::var_os("HOME");
        let result = user_config_path();
        if let Some(over) = had_override {
            unsafe { std::env::set_var("CRW_USER_CONFIG_DIR", over) };
        }
        match home {
            Some(h) => {
                assert_eq!(
                    result,
                    Some(
                        std::path::PathBuf::from(h)
                            .join(".config")
                            .join("crw")
                            .join("config.toml")
                    )
                );
            }
            None => assert_eq!(result, None, "no HOME and no override means None"),
        }
    }

    // ==================================================================
    // deserialize_string_vec / deserialize_opt_nonempty_string — direct
    // coverage via small wrapper structs (both are private helpers).
    // ==================================================================

    #[derive(Deserialize)]
    struct VecWrap {
        #[serde(deserialize_with = "deserialize_string_vec")]
        v: Vec<String>,
    }

    #[test]
    fn deserialize_string_vec_toml_array() {
        let w: VecWrap = toml::from_str(r#"v = ["a", "b", "c"]"#).unwrap();
        assert_eq!(w.v, vec!["a", "b", "c"]);
    }

    #[test]
    fn deserialize_string_vec_empty_toml_array() {
        let w: VecWrap = toml::from_str(r#"v = []"#).unwrap();
        assert!(w.v.is_empty());
    }

    #[test]
    fn deserialize_string_vec_single_value_no_comma() {
        let w: VecWrap = toml::from_str(r#"v = "only-one""#).unwrap();
        assert_eq!(w.v, vec!["only-one"]);
    }

    #[test]
    fn deserialize_string_vec_whitespace_around_commas_trimmed() {
        let w: VecWrap = toml::from_str(r#"v = "  a  ,  b  ,c""#).unwrap();
        assert_eq!(w.v, vec!["a", "b", "c"]);
    }

    #[test]
    fn deserialize_string_vec_unicode_entries() {
        let w: VecWrap = toml::from_str(r#"v = "日本語,emoji-🎉,café""#).unwrap();
        assert_eq!(w.v, vec!["日本語", "emoji-🎉", "café"]);
    }

    #[test]
    fn deserialize_string_vec_malformed_json_array_errors() {
        let result: Result<VecWrap, _> = toml::from_str(r#"v = "[not, valid, json""#);
        assert!(result.is_err());
    }

    #[derive(Deserialize)]
    struct OptStringWrap {
        #[serde(default, deserialize_with = "deserialize_opt_nonempty_string")]
        v: Option<String>,
    }

    #[test]
    fn deserialize_opt_nonempty_string_missing_key_is_none() {
        let w: OptStringWrap = toml::from_str("").unwrap();
        assert_eq!(w.v, None);
    }

    #[test]
    fn deserialize_opt_nonempty_string_present_value_trimmed() {
        let w: OptStringWrap = toml::from_str(r#"v = "  hello  ""#).unwrap();
        assert_eq!(w.v.as_deref(), Some("hello"));
    }

    #[test]
    fn deserialize_opt_nonempty_string_tabs_and_newlines_trimmed_to_none() {
        let w: OptStringWrap = toml::from_str("v = \"\\t\\n  \\t\"").unwrap();
        assert_eq!(w.v, None);
    }

    #[test]
    fn deserialize_opt_nonempty_string_unicode_value_preserved() {
        let w: OptStringWrap = toml::from_str(r#"v = "  café-🎉  ""#).unwrap();
        assert_eq!(w.v.as_deref(), Some("café-🎉"));
    }

    // ==================================================================
    // Boundary / malformed-input coverage: huge numbers, negative numbers
    // into unsigned fields, case-sensitivity of enum tags, deep nesting.
    // ==================================================================

    #[test]
    fn server_config_port_overflow_u16_errors() {
        let result: Result<ServerConfig, _> = toml::from_str("port = 70000");
        assert!(result.is_err(), "70000 exceeds u16::MAX");
    }

    #[test]
    fn server_config_negative_port_errors() {
        let result: Result<ServerConfig, _> = toml::from_str("port = -1");
        assert!(result.is_err());
    }

    #[test]
    fn crawler_config_negative_max_concurrency_errors() {
        let result: Result<CrawlerConfig, _> = toml::from_str("max_concurrency = -1");
        assert!(result.is_err(), "usize cannot hold a negative value");
    }

    #[test]
    fn crawler_config_max_concurrency_zero_is_accepted_at_parse_time() {
        // No lower-bound validation happens at deserialize time; a caller
        // setting 0 gets 0 back (any floor/clamp lives at the call site).
        let c: CrawlerConfig = toml::from_str("max_concurrency = 0").unwrap();
        assert_eq!(c.max_concurrency, 0);
    }

    #[test]
    fn document_config_sandbox_memory_bytes_accepts_u64_max() {
        let d: DocumentConfig =
            toml::from_str(&format!("sandbox_memory_bytes = {}", u64::MAX)).unwrap();
        assert_eq!(d.sandbox_memory_bytes, u64::MAX);
    }

    #[test]
    fn document_config_max_pages_overflow_beyond_usize_on_32bit_still_parses_u64_range() {
        // max_pages is `usize`; a very large but in-range value must parse.
        let d: DocumentConfig = toml::from_str("max_pages = 1000000").unwrap();
        assert_eq!(d.max_pages, 1_000_000);
    }

    #[test]
    fn renderer_mode_uppercase_variant_rejected() {
        #[derive(Deserialize)]
        struct Wrap {
            #[allow(dead_code)]
            mode: RendererMode,
        }
        // rename_all = "lowercase" means the tag is case-sensitive.
        let result: Result<Wrap, _> = toml::from_str(r#"mode = "AUTO""#);
        assert!(result.is_err());
    }

    #[test]
    fn renderer_mode_empty_string_rejected() {
        #[derive(Deserialize)]
        struct Wrap {
            #[allow(dead_code)]
            mode: RendererMode,
        }
        let result: Result<Wrap, _> = toml::from_str(r#"mode = """#);
        assert!(result.is_err());
    }

    #[test]
    fn chrome_backend_uppercase_variant_rejected() {
        #[derive(Deserialize)]
        struct Wrap {
            #[allow(dead_code)]
            chrome_backend: ChromeBackend,
        }
        let result: Result<Wrap, _> = toml::from_str(r#"chrome_backend = "Vanilla""#);
        assert!(result.is_err());
    }

    #[test]
    fn app_config_deeply_nested_full_renderer_tree_parses() {
        let a: AppConfig = toml::from_str(
            r#"
            [renderer]
            mode = "auto"

            [renderer.chrome]
            ws_url = "ws://chrome:9222"

            [renderer.chrome_pool]
            size = 8
            recycle_after_navs = 3

            [renderer.escalation]
            enabled = true
            residential_proxy = true

            [renderer.antibot]
            escalate_on_signal = true

            [renderer.camoufox]
            base_url = "http://cam:9377"
            include_in_auto = true
            "#,
        )
        .unwrap();
        assert_eq!(a.renderer.chrome.unwrap().ws_url, "ws://chrome:9222");
        assert_eq!(a.renderer.chrome_pool.size, Some(8));
        assert_eq!(a.renderer.chrome_pool.recycle_after_navs, 3);
        assert!(a.renderer.escalation.enabled);
        assert!(a.renderer.escalation.residential_proxy);
        assert!(a.renderer.antibot.escalate_on_signal);
        assert!(a.renderer.camoufox.unwrap().include_in_auto);
    }

    #[test]
    fn app_config_nested_extraction_llm_missing_api_key_fails_whole_load() {
        let result: Result<AppConfig, _> = toml::from_str(
            r#"
            [extraction.llm]
            provider = "deepseek"
            "#,
        );
        assert!(
            result.is_err(),
            "a nested required field must fail the whole document, not just that section"
        );
    }

    #[test]
    fn crawler_config_stealth_nested_toml_parse() {
        let c: CrawlerConfig = toml::from_str(
            r#"
            [stealth]
            enabled = true
            user_agents = ["ua-1"]
            jitter_factor = 0.7
            "#,
        )
        .unwrap();
        assert!(c.stealth.enabled);
        assert_eq!(c.stealth.user_agents, vec!["ua-1"]);
        assert_eq!(c.stealth.jitter_factor, 0.7);
        assert!(c.stealth.inject_headers, "unset field keeps its default");
    }

    #[test]
    fn search_config_research_and_github_engines_override() {
        let s: SearchConfig = toml::from_str(
            r#"
            research_engines = ["custom-engine"]
            github_engines = ["custom-github"]
            "#,
        )
        .unwrap();
        assert_eq!(s.research_engines, vec!["custom-engine"]);
        assert_eq!(s.github_engines, vec!["custom-github"]);
    }

    #[test]
    fn extraction_config_reserved_interactive_extracts_some_zero_disables() {
        let e: ExtractionConfig = toml::from_str("reserved_interactive_extracts = 0").unwrap();
        assert_eq!(e.reserved_interactive_extracts, Some(0));
    }

    #[test]
    fn llm_config_max_tokens_zero_accepted_at_parse_time() {
        // No lower-bound validation on this field; document current behavior.
        let l: LlmConfig = toml::from_str(
            r#"
            api_key = "sk-test"
            max_tokens = 0
            "#,
        )
        .unwrap();
        assert_eq!(l.max_tokens, 0);
    }

    #[test]
    fn crawler_config_requests_per_second_negative_accepted_at_parse_time() {
        // f64 has no inherent lower bound at deserialize time.
        let c: CrawlerConfig = toml::from_str("requests_per_second = -5.0").unwrap();
        assert_eq!(c.requests_per_second, -5.0);
    }

    #[test]
    fn map_url_filter_extra_tracking_params_preserves_duplicates() {
        let f: MapUrlFilterConfig =
            toml::from_str(r#"extra_tracking_params = ["ref", "ref", "src"]"#).unwrap();
        assert_eq!(f.extra_tracking_params, vec!["ref", "ref", "src"]);
    }

    #[test]
    fn env_var_bool_invalid_string_errors() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_CRAWLER__RESPECT_ROBOTS_TXT", "maybe") };
        let result = AppConfig::load();
        unsafe { std::env::remove_var("CRW_CRAWLER__RESPECT_ROBOTS_TXT") };
        assert!(result.is_err(), "\"maybe\" is not a valid bool");
    }

    #[test]
    fn env_var_bool_accepts_true_false_case_insensitive() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_CRAWLER__RESPECT_ROBOTS_TXT", "FALSE") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_CRAWLER__RESPECT_ROBOTS_TXT") };
        assert!(!cfg.crawler.respect_robots_txt);
    }

    #[test]
    fn document_config_reserved_interactive_parses_some_zero_disables() {
        let d: DocumentConfig = toml::from_str("reserved_interactive_parses = 0").unwrap();
        assert_eq!(d.reserved_interactive_parses, Some(0));
    }

    #[test]
    fn client_config_env_alias_whitespace_only_value_still_sets_some_empty_trim_filters_it() {
        // non_empty_trimmed_env filters whitespace-only to None, so the alias
        // must NOT overwrite an existing file-provided value with blank noise.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        let tmp = std::env::temp_dir().join(format!("crw-client-blank-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("config.toml"),
            "[client]\napi_url = \"https://from-file.example\"\n",
        )
        .unwrap();
        unsafe {
            std::env::set_var("CRW_USER_CONFIG_DIR", &tmp);
            std::env::set_var("CRW_API_URL", "   ");
        }
        let cfg = AppConfig::load().unwrap();
        unsafe {
            std::env::remove_var("CRW_USER_CONFIG_DIR");
            std::env::remove_var("CRW_API_URL");
        }
        std::fs::remove_dir_all(&tmp).ok();
        assert_eq!(
            cfg.client.api_url.as_deref(),
            Some("https://from-file.example"),
            "whitespace-only CRW_API_URL must not clobber the file value"
        );
    }

    #[test]
    fn escalation_config_toml_partial_keeps_other_defaults() {
        let e: EscalationConfig = toml::from_str("enabled = true").unwrap();
        assert!(e.enabled);
        assert_eq!(e.waterfall_timeout_ms, 8_000, "unset field keeps default");
        assert_eq!(e.proxy_country, "us");
    }

    #[test]
    fn antibot_config_toml_partial_keeps_other_defaults() {
        let a: AntibotConfig = toml::from_str("escalate_on_signal = true").unwrap();
        assert!(a.escalate_on_signal);
        assert!(a.enabled, "unset field keeps default true");
        assert!(a.escalate_in_failover, "unset field keeps default true");
    }

    #[test]
    fn chrome_pool_config_toml_partial_keeps_other_defaults() {
        let p: ChromePoolConfig = toml::from_str("size = 2").unwrap();
        assert_eq!(p.size, Some(2));
        assert_eq!(p.recycle_after_navs, 1, "unset field keeps default");
        assert_eq!(p.idle_timeout_secs, 300);
    }

    #[test]
    fn stealth_config_toml_partial_keeps_other_defaults() {
        let s: StealthConfig = toml::from_str("enabled = true").unwrap();
        assert!(s.enabled);
        assert_eq!(s.jitter_factor, 0.2, "unset field keeps default");
        assert!(s.inject_headers, "unset field keeps default true");
    }

    #[test]
    fn llm_fallback_config_toml_partial_keeps_other_defaults() {
        let l: LlmFallbackConfig = toml::from_str("always_run = true").unwrap();
        assert!(l.always_run);
        assert!(!l.enable, "unset field keeps default false");
        assert_eq!(l.quality_threshold, 0.3);
    }

    #[test]
    fn document_config_deeply_nested_within_app_config() {
        let a: AppConfig = toml::from_str(
            r#"
            [document]
            enabled = false
            sandbox = true
            sandbox_memory_bytes = 268435456
            "#,
        )
        .unwrap();
        assert!(!a.document.enabled);
        assert!(a.document.sandbox);
        assert_eq!(a.document.sandbox_memory_bytes, 268_435_456);
        // Unset field keeps the whole-struct default via #[serde(default)].
        assert_eq!(a.document.max_upload_bytes, 52_428_800);
    }

    #[test]
    fn search_config_toml_partial_keeps_other_defaults() {
        let s: SearchConfig = toml::from_str("timeout_ms = 1000").unwrap();
        assert_eq!(s.timeout_ms, 1000);
        assert!(s.enabled, "unset field keeps default true");
        assert_eq!(s.max_limit, 20, "unset field keeps default");
    }

    #[test]
    fn crawler_config_empty_toml_matches_programmatic_default_for_scalars() {
        let from_toml: CrawlerConfig = toml::from_str("").unwrap();
        let default = CrawlerConfig::default();
        assert_eq!(from_toml.max_concurrency, default.max_concurrency);
        assert_eq!(from_toml.max_batch_urls, default.max_batch_urls);
        assert_eq!(from_toml.user_agent, default.user_agent);
        assert_eq!(from_toml.job_ttl_secs, default.job_ttl_secs);
    }

    #[test]
    fn extraction_config_llm_present_but_empty_table_requires_api_key() {
        let result: Result<ExtractionConfig, _> = toml::from_str("[llm]\n");
        assert!(result.is_err());
    }

    #[test]
    fn extraction_config_llm_with_api_key_only_parses() {
        let e: ExtractionConfig = toml::from_str("[llm]\napi_key = \"sk-x\"\n").unwrap();
        let llm = e.llm.expect("llm section present");
        assert_eq!(llm.api_key, "sk-x");
        assert_eq!(llm.provider, "anthropic");
    }

    // ==================================================================
    // A few more precedence / edge cases to round out coverage.
    // ==================================================================

    #[test]
    fn env_var_search_backend_url_beats_default_toml_value() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        unsafe { std::env::set_var("CRW_SEARCH__SEARCH_BACKEND_URL", "http://env-wins:8080") };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_SEARCH__SEARCH_BACKEND_URL") };
        assert_eq!(
            cfg.search.resolve_backend_url(),
            Some("http://env-wins:8080")
        );
    }

    #[test]
    fn env_var_beats_user_config_for_scalar_int_field() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        let tmp = std::env::temp_dir().join(format!("crw-prec-int-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("config.toml"), "[server]\nport = 1111\n").unwrap();
        unsafe {
            std::env::set_var("CRW_USER_CONFIG_DIR", &tmp);
            std::env::set_var("CRW_SERVER__PORT", "2222");
        }
        let cfg = AppConfig::load().unwrap();
        unsafe {
            std::env::remove_var("CRW_USER_CONFIG_DIR");
            std::env::remove_var("CRW_SERVER__PORT");
        }
        std::fs::remove_dir_all(&tmp).ok();
        assert_eq!(
            cfg.server.port, 2222,
            "env var must beat the user config file"
        );
    }

    #[test]
    fn user_config_file_beats_bundled_defaults_for_unset_env() {
        // With no env var set, a value present only in the user config file
        // must still override the code-level Default.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        let tmp = std::env::temp_dir().join(format!("crw-prec-file-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("config.toml"), "[crawler]\nmax_concurrency = 77\n").unwrap();
        unsafe { std::env::set_var("CRW_USER_CONFIG_DIR", &tmp) };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_USER_CONFIG_DIR") };
        std::fs::remove_dir_all(&tmp).ok();
        assert_eq!(cfg.crawler.max_concurrency, 77);
        assert_ne!(
            cfg.crawler.max_concurrency,
            CrawlerConfig::default().max_concurrency
        );
    }

    #[test]
    fn no_user_config_file_falls_back_to_bundled_defaults() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        let tmp = std::env::temp_dir().join(format!("crw-no-file-{}", std::process::id()));
        // Directory exists but no config.toml inside it.
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe { std::env::set_var("CRW_USER_CONFIG_DIR", &tmp) };
        let cfg = AppConfig::load().unwrap();
        unsafe { std::env::remove_var("CRW_USER_CONFIG_DIR") };
        std::fs::remove_dir_all(&tmp).ok();
        assert_eq!(cfg.crawler.max_concurrency, 10);
        assert_eq!(cfg.server.port, 3000);
    }

    #[test]
    fn document_config_max_concurrent_parses_toml_and_env_precedence() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_renderer_env();
        let tmp = std::env::temp_dir().join(format!("crw-doc-prec-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("config.toml"),
            "[document]\nmax_concurrent_parses = 9\n",
        )
        .unwrap();
        unsafe {
            std::env::set_var("CRW_USER_CONFIG_DIR", &tmp);
            std::env::set_var("CRW_DOCUMENT__MAX_CONCURRENT_PARSES", "3");
        }
        let cfg = AppConfig::load().unwrap();
        unsafe {
            std::env::remove_var("CRW_USER_CONFIG_DIR");
            std::env::remove_var("CRW_DOCUMENT__MAX_CONCURRENT_PARSES");
        }
        std::fs::remove_dir_all(&tmp).ok();
        assert_eq!(cfg.document.max_concurrent_parses, 3, "env must win");
    }

    #[test]
    fn renderer_config_toml_parse_camoufox_and_cloak_endpoints_together() {
        let r: RendererConfig = toml::from_str(
            r#"
            [camoufox]
            base_url = "http://cam:9377"
            api_key = "cam-key"

            [cloak]
            base_url = "http://cloak:8000"
            api_key = "cloak-key"
            "#,
        )
        .unwrap();
        let cam = r.camoufox.unwrap();
        assert_eq!(cam.base_url, "http://cam:9377");
        assert_eq!(cam.api_key, "cam-key");
        assert!(!cam.include_in_auto);
        let cloak = r.cloak.unwrap();
        assert_eq!(cloak.base_url, "http://cloak:8000");
        assert_eq!(cloak.api_key, "cloak-key");
    }

    #[test]
    fn renderer_config_toml_parse_chrome_proxy_endpoint_and_timeout() {
        let r: RendererConfig = toml::from_str(
            r#"
            [chrome_proxy]
            ws_url = "ws://chrome-proxy:9222"
            chrome_proxy_timeout_ms = 50000
            "#,
        )
        .unwrap();
        assert_eq!(r.chrome_proxy.unwrap().ws_url, "ws://chrome-proxy:9222");
    }

    #[test]
    fn extraction_config_max_concurrent_extracts_is_deterministic_for_fixed_input() {
        // The formula itself (not the machine-dependent CPU count) is pure:
        // calling it twice must be identical.
        assert_eq!(
            crate::config::ExtractionConfig::default().max_concurrent_extracts,
            crate::config::ExtractionConfig::default().max_concurrent_extracts
        );
    }

    #[test]
    fn map_config_toml_parse_nested_url_filter() {
        let m: MapConfig = toml::from_str(
            r#"
            [url_filter]
            strip_tracking_params = false
            extra_preserve_params = ["utm_campaign"]
            "#,
        )
        .unwrap();
        assert!(!m.url_filter.strip_tracking_params);
        assert_eq!(m.url_filter.extra_preserve_params, vec!["utm_campaign"]);
        assert!(m.url_filter.drop_action_urls, "unset field keeps default");
    }
}
