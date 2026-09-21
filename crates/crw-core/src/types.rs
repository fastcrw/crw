use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use uuid::Uuid;

/// Supported output formats.
///
/// `"extract"` and `"llm-extract"` are accepted as aliases for `Json`
/// during deserialization (Firecrawl compatibility).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputFormat {
    Markdown,
    Html,
    RawHtml,
    PlainText,
    Links,
    Images,
    Json,
    Summary,
    ChangeTracking,
    Screenshot,
}

impl OutputFormat {
    /// Parse a single format token, accepting the Firecrawl-compatible aliases
    /// (`extract`/`llm-extract` → `json`, `change-tracking` → `changeTracking`).
    ///
    /// Shared by the v1 string deserializer below and the v2 `FormatSpec`
    /// parser (`routes/v2/formats.rs`) so the accepted token set and the error
    /// wording stay byte-identical across API versions.
    pub fn parse_loose(s: &str) -> Result<Self, String> {
        match s {
            "markdown" => Ok(OutputFormat::Markdown),
            "html" => Ok(OutputFormat::Html),
            "rawHtml" => Ok(OutputFormat::RawHtml),
            "plainText" => Ok(OutputFormat::PlainText),
            "links" => Ok(OutputFormat::Links),
            "images" => Ok(OutputFormat::Images),
            "json" | "extract" | "llm-extract" => Ok(OutputFormat::Json),
            "summary" => Ok(OutputFormat::Summary),
            "changeTracking" | "change-tracking" => Ok(OutputFormat::ChangeTracking),
            // `screenshot@fullPage` parses to the same fieldless variant; the
            // `fullPage` bit is carried out-of-band via
            // `ScrapeRequest.screenshot_full_page`. v2 extracts it from the
            // format spec (`routes/v2/formats.rs`); a v1 caller sets the body
            // field `screenshotFullPage`, so on v1 the `@fullPage` suffix alone
            // is accepted but does NOT widen the capture.
            "screenshot" | "screenshot@fullPage" => Ok(OutputFormat::Screenshot),
            other => Err(format!(
                "Unknown format '{other}'. Valid formats: markdown, html, rawHtml, plainText, links, images, json, summary, changeTracking, screenshot \
                 (aliases: extract, llm-extract, change-tracking, screenshot@fullPage). Use formats: [\"json\"] with jsonSchema for structured extraction."
            )),
        }
    }
}

impl<'de> Deserialize<'de> for OutputFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        OutputFormat::parse_loose(&s).map_err(serde::de::Error::custom)
    }
}

/// Strategy for chunking text content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ChunkStrategy {
    /// Split on sentence boundaries (.!?). Merges short chunks up to max_chars.
    #[serde(rename = "sentence")]
    Sentence {
        #[serde(default, alias = "maxChars")]
        max_chars: Option<usize>,
        #[serde(default, alias = "overlapChars")]
        overlap_chars: Option<usize>,
        #[serde(default)]
        dedupe: Option<bool>,
    },
    /// Split on a regex pattern.
    #[serde(rename = "regex")]
    Regex {
        pattern: String,
        #[serde(default, alias = "maxChars")]
        max_chars: Option<usize>,
        #[serde(default, alias = "overlapChars")]
        overlap_chars: Option<usize>,
        #[serde(default)]
        dedupe: Option<bool>,
    },
    /// Split on markdown headings (h1-h6).
    #[serde(rename = "topic")]
    Topic {
        #[serde(default, alias = "maxChars")]
        max_chars: Option<usize>,
        #[serde(default, alias = "overlapChars")]
        overlap_chars: Option<usize>,
        #[serde(default)]
        dedupe: Option<bool>,
    },
}

/// Filtering mode for ranked chunk retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilterMode {
    Bm25,
    Cosine,
}

/// Per-request renderer override. Sibling to `renderJs` for finer control.
///
/// `Auto` is equivalent to omitting the field — uses the configured fallback chain.
/// Other variants hard-pin to a specific renderer with no fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestedRenderer {
    Auto,
    Lightpanda,
    Chrome,
    /// Residential-proxy Chrome tier — egresses through the DataImpulse
    /// pool. `rename_all = "lowercase"` would yield `"chromeproxy"`, so the
    /// variant is renamed explicitly to match the internal renderer name
    /// (`"chrome_proxy"`) and `RendererKind::ChromeProxy`.
    #[serde(rename = "chrome_proxy")]
    ChromeProxy,
    Playwright,
    /// Opt-in Camoufox stealth tier. `rename_all = "lowercase"` already yields
    /// `"camoufox"`, matching the internal renderer name and
    /// `RendererKind::Camoufox`.
    Camoufox,
    /// Chrome-impersonating HTTP tier (wreq): real Chrome TLS/JA3/HTTP2
    /// fingerprint, no JS execution. `rename_all = "lowercase"` would yield
    /// `"impersonatedhttp"`, so renamed explicitly like `chrome_proxy`.
    #[serde(rename = "impersonated-http")]
    ImpersonatedHttp,
    // NOTE: there is deliberately NO `Cloak` variant here. The cloak tier is an
    // internal CF-challenge recovery arm (RendererKind::Cloak), fired
    // automatically — never a user-pinnable per-request `renderer`.
}

impl RequestedRenderer {
    /// Returns `Some(name)` for renderers that should be hard-pinned in dispatch.
    /// `Auto` returns `None` — equivalent to omitting the field.
    pub fn pinned_name(self) -> Option<&'static str> {
        match self {
            RequestedRenderer::Auto => None,
            RequestedRenderer::Lightpanda => Some("lightpanda"),
            RequestedRenderer::Chrome => Some("chrome"),
            RequestedRenderer::ChromeProxy => Some("chrome_proxy"),
            RequestedRenderer::Playwright => Some("playwright"),
            RequestedRenderer::Camoufox => Some("camoufox"),
            RequestedRenderer::ImpersonatedHttp => Some("impersonated-http"),
        }
    }

    /// Whether hard-pinning this tier implies a JS render when the request
    /// omits `renderJs`. True for every browser tier; false for wire-level
    /// tiers that never execute JS, and false for `Auto` because `Auto` is not
    /// a pin at all (`pinned_name()` returns `None`; an explicit
    /// `renderer: "auto"` must stay equivalent to omitting the field). The
    /// single rule behind the pin choke points (single.rs/crawl.rs implies-JS
    /// coercion, state.rs/single.rs pin validation, fetch_inner dispatch), so
    /// they cannot drift.
    pub fn implies_js(self) -> bool {
        !matches!(
            self,
            RequestedRenderer::ImpersonatedHttp | RequestedRenderer::Auto
        )
    }
}

/// Firecrawl-compatible extraction options (used via `extract: { schema: {...} }`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractOptions {
    #[serde(default)]
    pub schema: Option<serde_json::Value>,
    /// Natural-language extraction instruction (Firecrawl-compatible
    /// `extract.prompt`). Used alone — the LLM infers the output shape — or
    /// alongside `schema` to steer which fields are filled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// POST /v1/scrape request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrapeRequest {
    pub url: String,
    #[serde(default = "default_formats")]
    pub formats: Vec<OutputFormat>,
    #[serde(default = "default_true", alias = "only_main_content")]
    pub only_main_content: bool,
    /// null = auto-detect, true = force JS, false = skip JS
    #[serde(alias = "render_js")]
    pub render_js: Option<bool>,
    /// Milliseconds to wait after JS rendering.
    #[serde(alias = "wait_for")]
    pub wait_for: Option<u64>,
    #[serde(default, alias = "include_tags")]
    pub include_tags: Vec<String>,
    #[serde(default, alias = "exclude_tags")]
    pub exclude_tags: Vec<String>,
    #[serde(alias = "json_schema")]
    pub json_schema: Option<serde_json::Value>,
    /// Ask structured extraction (the `json` format) for per-field evidence.
    ///
    /// Each top-level **scalar** property of `json_schema` comes back with a
    /// [`crate::evidence::Basis`]: the value, the citation supporting it, and an
    /// honest [`crate::evidence::FieldStatus`]. Requires `json_schema`; the
    /// model's claims are verified server-side, so a field whose attribution
    /// does not hold up is marked `unverified`/`unsupported` rather than given
    /// a fabricated citation.
    ///
    /// Off by default, and when off the request sent to the model is unchanged.
    /// Costs extra output tokens and is materially slower on large schemas.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub basis: bool,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// CSS selector to narrow content before extraction.
    #[serde(default, alias = "css_selector")]
    pub css_selector: Option<String>,
    /// XPath expression to narrow content before extraction.
    #[serde(default)]
    pub xpath: Option<String>,
    /// Strategy for chunking the extracted markdown.
    #[serde(default, alias = "chunk_strategy")]
    pub chunk_strategy: Option<ChunkStrategy>,
    /// Query string for BM25/cosine chunk filtering.
    #[serde(default)]
    pub query: Option<String>,
    /// Filtering algorithm to rank chunks against query.
    #[serde(default, alias = "filter_mode")]
    pub filter_mode: Option<FilterMode>,
    /// Number of top chunks to return (default: 5).
    #[serde(default)]
    pub top_k: Option<usize>,
    /// Per-request proxy URL (overrides global config).
    /// Supports HTTP, HTTPS, and SOCKS5
    /// (e.g. "http://proxy:8080" or "socks5://user:pass@proxy:1080").
    #[serde(default)]
    pub proxy: Option<String>,
    /// Per-request proxy pool to rotate among (BYOP). Takes precedence over
    /// `proxy` and over the server's configured pool. Empty = use server config.
    /// Accepts the snake_case `proxy_list` alias (the managed layer injects it)
    /// in addition to the camelCase `proxyList`.
    #[serde(default, alias = "proxy_list")]
    pub proxy_list: Vec<String>,
    /// Rotation strategy for `proxy_list` (`round_robin`, `random`,
    /// `sticky_per_host`). `None` = server default (`sticky_per_host`).
    #[serde(default, alias = "proxy_rotation")]
    pub proxy_rotation: Option<crate::proxy::ProxyRotation>,
    /// 2-letter ISO 3166-1 alpha-2 country code (e.g. "us", "gb") for the
    /// residential-proxy chrome tier's egress. When the server has
    /// DataImpulse credentials configured, the engine composes
    /// `<base>__cr.<country>` per request and supplies it via CDP
    /// `Fetch.authRequired`. Unset / empty = server default country (or
    /// global pool when no default configured). Validated server-side;
    /// invalid values fall back to default.
    #[serde(default)]
    pub country: Option<String>,
    /// Override stealth mode for this request (None = use global config).
    #[serde(default)]
    pub stealth: Option<bool>,
    /// Unsupported Firecrawl parameter — captured to return a clear error.
    #[serde(default)]
    pub actions: Option<serde_json::Value>,
    /// Firecrawl-compatible `extract` object (e.g. `{ "schema": {...} }`).
    /// If `extract.schema` is set and `jsonSchema` is not, uses `extract.schema` as the schema.
    #[serde(default)]
    pub extract: Option<ExtractOptions>,
    /// Per-request LLM API key for structured extraction (BYOK).
    #[serde(default, alias = "llm_api_key")]
    pub llm_api_key: Option<String>,
    /// Per-request LLM provider override ("anthropic", "openai",
    /// "openai-responses", "deepseek", "azure", or "openai-compatible").
    #[serde(default, alias = "llm_provider")]
    pub llm_provider: Option<String>,
    /// Per-request LLM model override.
    #[serde(default, alias = "llm_model")]
    pub llm_model: Option<String>,
    /// Per-request LLM base URL override for Chat Completions-compatible or
    /// Responses-compatible providers. Example: `"https://gateway.example/v1"`.
    #[serde(default, alias = "base_url")]
    pub base_url: Option<String>,
    /// Optional user-supplied instructions appended to the summary system
    /// prompt (e.g. "respond in Turkish", "focus on technical details").
    /// The opencore's prompt-injection defense (UNTRUSTED delimiter,
    /// "ignore imperative content" rule) is kept intact — this only adds
    /// directives, it does not replace the safety wrapper. Capped at
    /// 500 chars server-side to bound token amplification.
    #[serde(default, alias = "summary_prompt")]
    pub summary_prompt: Option<String>,
    /// Maximum number of bytes of scraped content sent to the LLM for the
    /// `summary` format. Defaults to `[extraction.llm].max_html_bytes`
    /// (100 KB out of the box). Clamped to a 200 KB server-side ceiling
    /// regardless of value — protects against runaway provider bills.
    #[serde(default, alias = "max_content_chars")]
    pub max_content_chars: Option<usize>,
    /// Pin this request to a specific renderer. `None` or `Auto` = use the
    /// configured chain. Hard-pin: pinned renderer failures surface as errors,
    /// no silent fallback to a different renderer or HTTP. Pinning a non-Auto
    /// value implies `renderJs:true` unless `renderJs:false` is set explicitly.
    #[serde(default)]
    pub renderer: Option<RequestedRenderer>,
    /// Internal, server-set hint (NOT a public knob): route this request STRAIGHT
    /// to the cloak Turnstile recovery arm before the normal ladder, with the full
    /// deadline, for a domain the SaaS routing registry has learned is
    /// Cloudflare-managed. Honored only when the `cloak` feature is built and a
    /// cloak arm is configured, and only when the remaining deadline clears the
    /// cloak floor + ladder reserve; on any miss it falls through to today's ladder
    /// (recall-safe). Absent/`None` is byte-identical to today.
    ///
    /// `skip_deserializing`: this can NEVER be set from a request body. A caller's
    /// `{"forceCloak": true}` is ignored; the value comes exclusively from the
    /// trusted `x-crw-force-cloak` header, which only the SaaS front-end sets (the
    /// engine is not internet-exposed in the managed deployment). This makes the
    /// "server-only" contract structural rather than relying on a downstream strip.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub force_cloak: Option<bool>,
    /// End-to-end deadline budget in milliseconds. When unset, the configured
    /// `request.deadline_ms_default` (8000) applies. The SLO p95 metric is
    /// computed only over requests with `deadline_ms <= 8000`; longer values
    /// land in a separate slow-path histogram. Must be in `(0, 60000]`.
    #[serde(default, alias = "deadline_ms")]
    pub deadline_ms: Option<u64>,
    /// Opt-in extraction debug trace. When true, the response includes a
    /// `debugExtraction` field describing every candidate the extractor
    /// considered and why one was selected.
    #[serde(default)]
    pub debug: Option<bool>,
    /// Change-tracking options. Activated when `formats` contains
    /// `"changeTracking"`. Carries the diff modes, an optional extraction
    /// schema/prompt for json mode, and the caller-supplied `previous`
    /// snapshot to diff the current scrape against. Sibling field — mirrors
    /// the precedented `extract` / `jsonSchema` pattern (the `formats` entry
    /// is the plain string `"changeTracking"`, options ride here).
    #[serde(default, alias = "change_tracking")]
    pub change_tracking: Option<ChangeTrackingOptions>,
    /// Plain-language monitor goal used by the meaningful-change judge.
    /// Capped server-side at 2 KB. The judge only runs when both `goal` is
    /// present and `judgeEnabled` is true (and the page actually changed).
    #[serde(default)]
    pub goal: Option<String>,
    /// Whether to run the LLM meaningful-change judge on a changed page.
    /// `None` is treated as "off" at the opencore layer — the SaaS
    /// orchestration decides auto-enable semantics.
    #[serde(default, alias = "judge_enabled")]
    pub judge_enabled: Option<bool>,
    /// Firecrawl-compatible document parsers. Controls how non-HTML documents
    /// (currently only PDF) are handled when a URL returns one.
    /// - `None` (field omitted) → PDFs are auto-parsed to markdown (default,
    ///   matches Firecrawl).
    /// - `Some([])` → parsing disabled; the raw document is left unconverted.
    /// - `Some([{type:"pdf"}])` → explicitly enable PDF parsing (optionally
    ///   capped via `maxPages`).
    #[serde(default)]
    pub parsers: Option<Vec<ParserSpec>>,
    /// Whether a requested `screenshot` format should capture the full page
    /// (`screenshot@fullPage` / `{type:"screenshot", fullPage:true}`) instead of
    /// just the viewport. Carried out-of-band rather than on the (`Copy`/`Hash`)
    /// `OutputFormat` enum so `formats.contains(&Screenshot)` stays cheap. v2
    /// sets it from the format spec (`routes/v2/formats.rs`); a v1 caller sets
    /// it directly (`screenshotFullPage`), defaulting to viewport-only.
    #[serde(default, alias = "screenshot_full_page")]
    pub screenshot_full_page: bool,
    /// How old a cached copy of this fetch may be, in milliseconds. `None` is
    /// the published default (one hour), `0` always fetches, and anything above
    /// the 24 hour ceiling is clamped. Documented at `docs/docs/scraping.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age: Option<u64>,
    /// Firecrawl's write switch: `false` reads the cache but never writes to
    /// it. Reading stays allowed because the field is about what we keep, not
    /// about what we may reuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_in_cache: Option<bool>,
    /// Opaque tenant scope for the page cache. `skip_deserializing`, so a
    /// caller can never set it: the only source is the `x-crw-cache-scope`
    /// header our own front end sets, the same channel as `force_cloak`.
    ///
    /// Entries are separated by this value, which is how the hosted API keeps
    /// its published promise that a cache is never shared between customers.
    /// Unset (self-host, CLI) means one shared scope, which is the right
    /// behaviour when there is only one tenant.
    #[serde(skip_deserializing, skip_serializing)]
    pub cache_scope: Option<String>,
}

/// A document parser directive (Firecrawl `parsers` entry). Accepts either the
/// bare string form (`"pdf"`) or the object form (`{ "type": "pdf",
/// "mode": "auto", "maxPages": 10 }`) on the wire; always serializes to the
/// object form. Matches Firecrawl v2's `parsers` shape exactly.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ParserSpec {
    /// Parser type. Only `"pdf"` is supported today.
    #[serde(rename = "type")]
    pub parser_type: String,
    /// Parsing strategy (Firecrawl: `auto` | `fast` | `ocr`). fastCRW has no
    /// OCR, so `ocr` degrades to text extraction with a warning, and `auto`
    /// (text-first + OCR fallback in Firecrawl) is text-only here. Accepted for
    /// wire-compatibility regardless. `None` ≈ `auto`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Optional cap on the number of pages to parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pages: Option<usize>,
}

impl ParserSpec {
    /// Convenience constructor for the common PDF directive.
    pub fn pdf() -> Self {
        Self {
            parser_type: "pdf".to_string(),
            mode: None,
            max_pages: None,
        }
    }
}

impl<'de> serde::Deserialize<'de> for ParserSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Str(String),
            Obj {
                #[serde(rename = "type")]
                parser_type: String,
                #[serde(default)]
                mode: Option<String>,
                #[serde(default, rename = "maxPages", alias = "max_pages")]
                max_pages: Option<usize>,
            },
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Str(parser_type) => ParserSpec {
                parser_type,
                mode: None,
                max_pages: None,
            },
            Raw::Obj {
                parser_type,
                mode,
                max_pages,
            } => ParserSpec {
                parser_type,
                mode,
                max_pages,
            },
        })
    }
}

fn default_formats() -> Vec<OutputFormat> {
    vec![OutputFormat::Markdown]
}

impl Default for ScrapeRequest {
    /// Matches the serde defaults exactly (`formats: ["markdown"]`,
    /// `only_main_content: true`, everything else empty/None). Hand-written
    /// rather than derived because `#[derive(Default)]` would give
    /// `formats: vec![]` / `only_main_content: false`, contradicting the wire
    /// defaults — the v2 adapters build `ScrapeRequest { .., ..Default::default() }`
    /// and rely on these matching.
    fn default() -> Self {
        Self {
            url: String::new(),
            formats: default_formats(),
            only_main_content: true,
            render_js: None,
            wait_for: None,
            include_tags: Vec::new(),
            exclude_tags: Vec::new(),
            json_schema: None,
            basis: false,
            headers: HashMap::new(),
            css_selector: None,
            xpath: None,
            chunk_strategy: None,
            query: None,
            filter_mode: None,
            top_k: None,
            proxy: None,
            proxy_list: Vec::new(),
            proxy_rotation: None,
            country: None,
            stealth: None,
            actions: None,
            extract: None,
            llm_api_key: None,
            llm_provider: None,
            llm_model: None,
            base_url: None,
            summary_prompt: None,
            max_content_chars: None,
            renderer: None,
            force_cloak: None,
            deadline_ms: None,
            debug: None,
            change_tracking: None,
            goal: None,
            judge_enabled: None,
            parsers: None,
            screenshot_full_page: false,
            max_age: None,
            store_in_cache: None,
            cache_scope: None,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Metadata about a scraped page.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PageMetadata {
    pub title: Option<String>,
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub og_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub og_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub og_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    #[serde(rename = "sourceURL")]
    pub source_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub status_code: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rendered_with: Option<String>,
    pub elapsed_ms: u64,
    /// Number of pages, for paginated documents (PDF). `None` for web pages.
    /// Drives per-page billing on document scrapes / uploads. Serialized as
    /// `numPages` to match Firecrawl's metadata field name.
    #[serde(default, rename = "numPages", skip_serializing_if = "Option::is_none")]
    pub page_count: Option<usize>,
    /// Original filename for documents uploaded via `/v2/parse`. `None` for
    /// URL-sourced pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_filename: Option<String>,
    /// All raw `<meta>` tags (name/property → content) not already surfaced as
    /// a named field above, flattened onto the metadata object to match
    /// Firecrawl (e.g. `twitter:creator`, `author`, `keywords`, `og:type`).
    /// A tag repeated on the page becomes a JSON array; otherwise a string.
    /// Empty for sources without HTML meta (PDFs, uploads).
    #[serde(flatten, default)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Token-usage and best-effort cost for one LLM call.
///
/// `estimated_cost_usd` is informational only — provider pricing drifts
/// and this value MUST NOT be used for customer billing.
///
/// `cache_hit_input_tokens` / `cache_miss_input_tokens` surface the
/// provider's prompt-cache breakdown (Anthropic `cache_read_input_tokens`,
/// OpenAI `prompt_tokens_details.cached_tokens`, DeepSeek
/// `prompt_cache_hit_tokens`). `None` means the provider did not report a
/// breakdown for this call. `truncated` flags requests whose markdown
/// input was clipped before the LLM call. `calls` aggregates the number
/// of underlying provider calls when usage is summed across multiple
/// invocations (default 1 for a single call).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
    pub model: String,
    pub provider: String,

    // ── Wave 2 additions (additive, backward-compatible via serde defaults) ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_hit_input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_miss_input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(default = "one_u32", skip_serializing_if = "is_one_u32")]
    pub calls: u32,

    // ── Wave 4 (R1) additions: SaaS billing correlation across legs ──
    //
    // The SaaS-side managed pricing path needs to know exactly how many
    // summary calls executed AND whether the answer leg ran. The 5-branch
    // fail-closed dispatch keys off these counters:
    //   - executedSummaries > 0 OR answerExecuted ⇒ engine did work
    //   - inputTokens == 0 AND outputTokens == 0 ⇒ no upstream cost
    // Without the counters the SaaS cannot disambiguate "no work" from
    // "work but missing telemetry" and would refund or charge wrong.
    //
    // Always serialized (no skip_serializing_if) so the always-present
    // R1 invariant holds: when /v1/search returns llmUsage, both fields
    // are explicitly visible.
    #[serde(default)]
    pub executed_summaries: u32,
    #[serde(default)]
    pub answer_executed: bool,
}

impl LlmUsage {
    /// Fold another leg's usage into this one.
    ///
    /// A single scrape can make MORE THAN ONE model call: structured extraction,
    /// the summary, and the change-tracking judge are three separate legs. The old
    /// code kept whichever landed first (`if usage.is_none()`), so the second and
    /// third calls were invisible: the provider billed us for them and the SaaS,
    /// which prices off this field, billed the customer for one.
    ///
    /// Tokens add up. `calls` counts the legs. The model/provider labels are kept
    /// from the first leg, which is the managed model on every path today.
    pub fn merge(&mut self, other: LlmUsage) {
        let self_input_before = self.input_tokens;
        let self_cache_hit_before = self.cache_hit_input_tokens.unwrap_or(0);
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
        self.calls = self.calls.saturating_add(other.calls.max(1));
        self.executed_summaries = self
            .executed_summaries
            .saturating_add(other.executed_summaries);
        self.answer_executed |= other.answer_executed;
        self.truncated |= other.truncated;
        // Cache counts are merged BEFORE `input_tokens` is consulted below, so
        // capture each side's own input first.
        let (self_input, other_input) = (self_input_before, other.input_tokens);
        self.cache_hit_input_tokens =
            sum_opt(self.cache_hit_input_tokens, other.cache_hit_input_tokens);
        // A leg that reports no cache counts still consumed its input, and that
        // input is a cache MISS by definition: nothing was served from cache.
        // Carrying the other leg's `Some(x)` forward unchanged would price this
        // leg's tokens at zero downstream, because the biller prefers an
        // explicit miss count over deriving `input - hit`. Dormant while the
        // provider reports no cache fields at all; live as soon as one leg does.
        self.cache_miss_input_tokens =
            match (self.cache_miss_input_tokens, other.cache_miss_input_tokens) {
                (Some(a), Some(b)) => Some(a.saturating_add(b)),
                (Some(a), None) => Some(a.saturating_add(
                    other_input.saturating_sub(other.cache_hit_input_tokens.unwrap_or(0)),
                )),
                (None, Some(b)) => Some(
                    self_input
                        .saturating_sub(self_cache_hit_before)
                        .saturating_add(b),
                ),
                (None, None) => None,
            };
        self.estimated_cost_usd = match (self.estimated_cost_usd, other.estimated_cost_usd) {
            (Some(a), Some(b)) => Some(a + b),
            (Some(a), None) => Some(a),
            (None, b) => b,
        };
    }

    /// Merge into an optional slot, seeding it when empty.
    pub fn accumulate(slot: &mut Option<LlmUsage>, other: Option<LlmUsage>) {
        let Some(other) = other else { return };
        match slot {
            Some(existing) => existing.merge(other),
            None => *slot = Some(other),
        }
    }
}

fn sum_opt(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.saturating_add(y)),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

fn one_u32() -> u32 {
    1
}
fn is_one_u32(n: &u32) -> bool {
    *n == 1
}

/// A single chunk with optional relevance score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkResult {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    pub index: usize,
}

/// A single image discovered on a scraped page, returned when `formats`
/// includes `images`. `url` is resolved to an absolute URL (or kept verbatim for
/// `data:`/`blob:`); `alt` is the `<img alt>` text when available (most non-img
/// sources — meta, icons, poster, background — carry no alt).
///
/// The native `/v1` surface serializes these objects. The Firecrawl-compat
/// `/v2` surface flattens them to a plain `Vec<String>` of URLs in
/// `routes/v2/adapters.rs::to_v2_document` (Firecrawl's `images` is `string[]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScrapedImage {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
}

/// Data returned for a single scraped page.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScrapeData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    /// Content fingerprint of the canonical markdown: hex SHA-256 of the
    /// normalized markdown (`crw_diff::snapshot::hash_markdown`). Stable across
    /// CRLF/whitespace noise, so clients can dedup/cache and evidence offsets
    /// (highlights, citations) can be tied to an exact source revision. `None`
    /// when no markdown was produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plain_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<String>>,
    /// Images discovered on the page; populated when `formats` includes
    /// `images`. Native `/v1` shape; v2 flattens to `Vec<String>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ScrapedImage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<serde_json::Value>,
    /// Per-field evidence for `json`; populated only when the request set
    /// `basis: true`. One entry per top-level scalar schema property, each with
    /// an honest [`crate::evidence::FieldStatus`] — a field whose attribution
    /// could not be verified says so rather than carrying a fabricated citation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<Vec<crate::evidence::Basis>>,
    /// Coded reasons for every basis downgrade.
    ///
    /// Deliberately NOT merged into `warnings`: that field carries free-form
    /// upstream text and internal renderer detail, whereas these are a closed,
    /// crw-owned code set safe for a consumer to persist and show a customer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub basis_warnings: Vec<crate::evidence::BasisWarning>,
    /// `"sha256:"`-prefixed hash of the canonical text sent to the extraction
    /// LLM (the cleaned markdown, after truncation). Populated on the `basis`
    /// path only.
    ///
    /// This is the per-document hash a consumer checks `EvidenceCitation
    /// .sourceHash` against. It is recorded even when no citation survived, so
    /// that check has an independent record to compare with rather than
    /// validating a citation against itself. Distinct from `source_hash`, which
    /// covers the **full** markdown; `sourceTextKind` names which is which.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_input_hash: Option<String>,
    /// LLM-generated summary; populated when `formats` includes `summary`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Token usage + best-effort cost for any LLM call this request triggered
    /// (summary, structured JSON, etc).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_usage: Option<LlmUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunks: Option<Vec<ChunkResult>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// Soft-failure / informational warnings collected through the render
    /// chain. Empty vec serializes as missing for backward compatibility.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
    /// Routing decision metadata (renderer chosen, failover chain).
    /// Surfaced for debug + UI; `None` for legacy paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_decision: Option<RenderDecision>,
    /// Credit cost attributed to this page (0 = not yet priced).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub credit_cost: u32,
    /// True when this page came from the fetch cache instead of the network.
    /// Surfaces as `cacheState` on the Firecrawl-compat surface and as `cached`
    /// on the native one, so a caller can always tell a replay from a fresh
    /// fetch. Omitted when false, which keeps every existing response byte
    /// identical.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cached: bool,
    pub metadata: PageMetadata,
    /// Extraction debug trace; populated only when the request opts in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_extraction: Option<DebugExtraction>,
    /// MIME content type of the fetched resource (from `FetchResult`).
    /// Surfaced so change-tracking can hash binary/non-text content (PDF,
    /// images) by bytes rather than attempting a markdown/json diff.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Change-tracking result; populated only when `formats` includes
    /// `"changeTracking"`. Carries per-page status + diff (+ judgment when
    /// the orchestration layer ran the judge).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_tracking: Option<ChangeTrackingResult>,
    /// Page screenshot as a `data:image/png;base64,<...>` URL; populated only
    /// when `formats` includes `"screenshot"`. The `data:` prefix is wrapped
    /// once in `single.rs` (`FetchResult.screenshot` stays raw base64).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    /// Why this document has no body: an anti-bot verdict stamped at the scrape
    /// choke (`single::scrape_url`), or — on the crawl and batch paths, which
    /// return documents rather than one envelope — `HTTP_ERROR_VENDOR` for an
    /// origin error page. `Some` means the caller did not get the page they
    /// asked for, so v1/v2 turn it into `success:false` and nobody bills it.
    /// `None` (skipped when serializing) = a real page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<BlockOutcome>,
    /// The renderer snapshotted a partial DOM because the navigation budget
    /// elapsed (`FetchResult.truncated`). The content is usable but incomplete,
    /// and a caller cannot otherwise tell it apart from a page that genuinely
    /// has little content — which is what makes a shrinking scrape budget a
    /// silent recall regression.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// Above this many bytes of body, a `>= 400` response is treated as the real page
/// rather than the origin's error page.
///
/// Measured, not guessed. Across one prod day (43 responses graded `success` while
/// carrying `metadata.statusCode >= 400`) the largest error page — a CentOS Apache
/// test page — rendered to 2,356 bytes of markdown, and the next-largest 4xx body
/// was 3,340. So the bar sits in that gap. Above it live real pages served under an
/// error status, which the renderer deliberately keeps (`crw_renderer` accept gate,
/// and `crw_crawl::single`: "the status is a soft signal, not a content gate"):
/// stackoverflow.com under 403 renders 39,499 chars, a YouTube watch page under 401
/// renders 12,256, a Medium article under 403 renders 10,826.
///
/// Deliberately conservative. It leaves large branded 404s (GitHub's is 3,340)
/// passing as successes; raising it is a recall decision that needs its own
/// benchmark run, not a nudge.
const ERROR_PAGE_MAX_TEXT: usize = 2_500;

impl ScrapeData {
    /// Size of the body the caller actually asked for, in bytes.
    ///
    /// Text formats first: the previous gate took `.max()` across all four body
    /// fields, so an error page's raw HTML — always large — kept the gate from
    /// ever firing whenever `formats` included `html` or `rawHtml`.
    ///
    /// But measuring only text would be worse than the bug it fixes. A request
    /// for `formats:["rawHtml"]` populates neither text field, so a text-only
    /// measurement reads 0 and fails **every** `>= 400` response, including the
    /// large real pages that soft-block statuses are known to carry. So when the
    /// caller asked for no text at all, fall back to the markup we do hold. It
    /// discriminates less well (an error page's HTML is bulkier than its text),
    /// which is the right direction to be wrong in.
    /// `None` when the document holds no body at all — `formats:["screenshot"]`,
    /// `["links"]` and a summary-only request all populate none of these fields,
    /// and "nothing to measure" must not be read as "measured nothing".
    /// A present-but-empty field is a real measurement of 0: the caller asked for
    /// that format and the page yielded none of it.
    fn rendered_text_len(&self) -> Option<usize> {
        let text = [self.markdown.as_deref(), self.plain_text.as_deref()]
            .into_iter()
            .flatten()
            .map(str::len)
            .max();
        text.or_else(|| {
            [self.html.as_deref(), self.raw_html.as_deref()]
                .into_iter()
                .flatten()
                .map(str::len)
                .max()
        })
    }

    /// `Some(message)` when the origin answered `>= 400` and what we are holding is
    /// its error page rather than the page that was asked for.
    ///
    /// One helper for every surface: v1, v2, crawl and batch all have to agree, or
    /// the same URL is refunded on one endpoint and billed on another.
    ///
    /// A block verdict outranks the origin's status, and that ordering lives here
    /// rather than at the call sites. Cloudflare answers its challenge with 403,
    /// so without this the wall was classified `http_error` and the "Just a
    /// moment..." shell shipped as the page's markdown — `clear_body()` runs on
    /// the block path, which the status gate returned before ever reaching.
    /// Measured on prod 2026-08-24 and on 109 real customer requests across six
    /// days of traces.
    ///
    /// Three callers already worked around this by hand (`crw-crawl::crawl`,
    /// `crw-server::state`'s batch path, `crw-crawl::single`'s `unusable`), and
    /// the batch one's comment says it clears the shell "exactly as the single
    /// scrape route does" — which was not true, because the single scrape route
    /// asked this helper first. Owning the rule here makes that comment true and
    /// leaves those guards redundant but harmless.
    ///
    /// `structural_failure` is the one verdict that does NOT outrank the status,
    /// and the exclusion is deliberate: `structural_integrity_check` never reads
    /// the HTTP status, so a terse origin error page earns that verdict on its
    /// shape alone. Letting it short-circuit here would turn every small 404 into
    /// `no_usable_content` with its body cleared, when today the caller can read
    /// the error page under `http_error`.
    ///
    /// It is also exactly the vendor `crw-crawl::crawl` filters out before it
    /// asks this question, for the same stated reason — so excluding it here, and
    /// only it, is what makes the two surfaces agree. `parked_domain` is NOT
    /// excluded, precisely because crawl does not exclude it either: a parked
    /// page is a real verdict about the destination on every surface, and
    /// carving it out here would have created the cross-surface split this change
    /// exists to close.
    pub fn http_error(&self) -> Option<String> {
        if self
            .block
            .as_ref()
            .is_some_and(|b| b.vendor != STRUCTURAL_FAILURE_VENDOR)
        {
            return None;
        }
        let status = self.metadata.status_code;
        if status < 400 || self.rendered_text_len()? >= ERROR_PAGE_MAX_TEXT {
            return None;
        }
        Some(
            self.warning
                .clone()
                .unwrap_or_else(|| format!("Target returned HTTP {status}")),
        )
    }

    /// True when none of the formats the caller actually asked for produced
    /// anything.
    ///
    /// This is the last gap that let a scrape charge for a response with no
    /// content in it. Two shapes measured on prod, both `success:true`, both
    /// billed:
    ///
    /// ```text
    /// {"markdown":"", "warning":"pdf_too_large: document decompresses beyond
    ///   the allowed size (possible decompression bomb)",
    ///   "metadata":{"statusCode":200,"renderedWith":"pdf","numPages":0}}
    /// {"markdown":"", "warning":null, "warnings":null,
    ///   "metadata":{"statusCode":200,"renderedWith":"http","elapsedMs":110}}
    /// ```
    ///
    /// The second carries no warning at all, so keying on warning strings — or
    /// on a list of terminal `PdfError` codes — would miss it and would need
    /// extending every time a new extraction failure is added.
    ///
    /// Takes `formats` instead of reading `Option::is_some()` off `self`. For
    /// most fields the two agree, but `summary` and `screenshot` collapse
    /// "requested and failed" into the same `None` as "never asked for":
    /// `crw_crawl::single` turns a `summarize()` error into a warning and leaves
    /// `summary: None`, and a screenshot is only captured on the CDP tier, so a
    /// request served by any other tier leaves `screenshot: None`. Both are
    /// exactly the billed-for-nothing case this exists to catch, and neither is
    /// visible without knowing what was asked.
    ///
    /// `ChangeTracking` is excluded on purpose: "nothing changed since
    /// `previous`" is a real answer, the same way a confirmed zero-result search
    /// is a real search. `chunks` is excluded too — it is driven by
    /// `chunk_strategy` rather than a format, and is a derived view of markdown
    /// rather than an independent ask.
    pub fn has_no_content(&self, formats: &[OutputFormat]) -> bool {
        // An explicitly empty `formats` array asked for nothing, so nothing is
        // missing. `serde`'s default only fills in `[Markdown]` when the field is
        // absent — `"formats": []` reaches here as an empty slice, and `.any()`
        // over it is vacuously false, which would fail every such scrape. `/v2`
        // rejects an empty list up front (`v2/formats.rs`); `/v1` accepts it, so
        // the guard belongs here rather than at one route.
        if formats.is_empty() {
            return false;
        }
        !formats.iter().any(|f| self.format_delivered(*f))
    }

    /// Whether the one field that carries `format` came back with something in it.
    fn format_delivered(&self, format: OutputFormat) -> bool {
        fn filled(s: Option<&str>) -> bool {
            s.is_some_and(|s| !s.trim().is_empty())
        }
        match format {
            OutputFormat::Markdown => filled(self.markdown.as_deref()),
            OutputFormat::Html => filled(self.html.as_deref()),
            OutputFormat::RawHtml => filled(self.raw_html.as_deref()),
            OutputFormat::PlainText => filled(self.plain_text.as_deref()),
            OutputFormat::Summary => filled(self.summary.as_deref()),
            OutputFormat::Screenshot => filled(self.screenshot.as_deref()),
            // A collection that came back present-but-empty is a real answer:
            // "this page has no outbound links" is a complete result, unlike an
            // empty markdown body, which means the page never rendered. Presence
            // is the measurement; emptiness is a legitimate value of it.
            OutputFormat::Links => self.links.is_some(),
            OutputFormat::Images => self.images.is_some(),
            // An extraction that returns `{}` or `[]` found none of the schema's
            // fields; a bare number or bool is still a real answer.
            OutputFormat::Json => self.json.as_ref().is_some_and(|v| match v {
                serde_json::Value::Null => false,
                serde_json::Value::Object(m) => !m.is_empty(),
                serde_json::Value::Array(a) => !a.is_empty(),
                serde_json::Value::String(s) => !s.trim().is_empty(),
                serde_json::Value::Number(_) | serde_json::Value::Bool(_) => true,
            }),
            // "nothing changed since `previous`" is a real answer, so the value
            // is never inspected — but its presence is, so a change-tracking run
            // that produced nothing at all is still caught.
            OutputFormat::ChangeTracking => self.change_tracking.is_some(),
        }
    }

    /// Clear the page-content fields (markdown, HTML, text, links, and any
    /// LLM-derived outputs), keeping `metadata`, `block`, warnings, and the
    /// screenshot. Used on the block-response path so a detected anti-bot
    /// interstitial returns a clean block (success:false + error + metadata)
    /// instead of the challenge shell text as content.
    pub fn clear_body(&mut self) {
        self.markdown = None;
        self.source_hash = None;
        self.html = None;
        self.raw_html = None;
        self.plain_text = None;
        self.links = None;
        self.images = None;
        self.json = None;
        self.basis = None;
        self.summary = None;
        self.chunks = None;
    }

    /// Whether any page-content field survived. A document without a body is a
    /// placeholder for a URL that produced no page (a cleared wall, or a scrape
    /// error), as opposed to an origin error page that was kept readable.
    /// `source_hash` and `basis` are excluded on purpose: both derive from the
    /// fields below and never stand on their own. `screenshot` is not counted
    /// either, so a wall captured with a screenshot still reads as no page
    /// while the caller keeps the image.
    pub fn has_body(&self) -> bool {
        self.markdown.is_some()
            || self.html.is_some()
            || self.raw_html.is_some()
            || self.plain_text.is_some()
            || self.links.is_some()
            || self.images.is_some()
            || self.json.is_some()
            || self.summary.is_some()
            || self.chunks.is_some()
    }
}

/// Typed anti-bot block verdict. `vendor` is the antibot `class_name`
/// (cloudflare|datadome|perimeterx|generic_block|structural_failure|…);
/// `reason` is the detector's human-readable explanation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockOutcome {
    pub vendor: String,
    pub reason: String,
}

/// `AntibotSignal::StructuralFailure`'s `class_name()`. Not a vendor: it is the
/// "we got a page but there is nothing usable in it" verdict, which the classifier
/// keeps inside `AntibotSignal` because `is_blocked()` drives renderer escalation.
/// See `message()` for why the customer-facing wording splits here.
pub const STRUCTURAL_FAILURE_VENDOR: &str = "structural_failure";

/// `BlockOutcome::vendor` for an HTTP-level failure to get the page: the origin
/// answered with an error status and this is its error page, its CDN answered
/// that it could not reach the origin, or, on the crawl path, the request did
/// not complete at all and `metadata.statusCode` is `0`. Not an anti-bot
/// verdict, but the same consequence for the caller and for billing, and the
/// crawl/batch surfaces have nowhere else to say it: they return an array of
/// documents, not an envelope with an error code.
pub const HTTP_ERROR_VENDOR: &str = "http_error";

/// `BlockOutcome::vendor` for a registrar parking page, a domain-marketplace listing
/// or a default web-server vhost. Like `HTTP_ERROR_VENDOR` it is not an anti-bot
/// verdict but carries the same consequence: nothing the caller asked for was
/// delivered. Kept distinct from `STRUCTURAL_FAILURE_VENDOR` because these pages are
/// not thin or broken — they render perfectly well, they are just not the site.
pub const PARKED_DOMAIN_VENDOR: &str = "parked_domain";

impl BlockOutcome {
    /// Standard anti-bot block error string shared by the v1 and v2 handlers so
    /// the two API surfaces label the same block identically.
    ///
    /// `structural_failure` is deliberately worded differently. It is not a
    /// vendor wall — it fires on a thin or empty document (`antibot.rs` structural
    /// arms), which is most often a broken TLS page, an error stub, or a JS shell
    /// we could not hydrate. Reporting that as "Blocked by anti-bot" sent
    /// customers to buy proxies and stealth for what was a certificate problem
    /// (`wrong.host.badssl.com`: 21 visible characters, reported as a block).
    ///
    /// Wording only. The verdict stays inside `AntibotSignal`, `is_blocked()` is
    /// untouched, and the escalation ladder behaves identically — a thin page must
    /// still escalate to the next renderer tier, which is what `is_blocked()`
    /// drives.
    pub fn message(&self) -> String {
        // `parked_domain` rides the same wording for the same reason: telling a
        // customer their target was "blocked by anti-bot" when the domain is simply
        // for sale is what sends them off to buy proxies for a site that does not
        // exist.
        if self.vendor == STRUCTURAL_FAILURE_VENDOR || self.vendor == PARKED_DOMAIN_VENDOR {
            format!("No usable content could be extracted ({})", self.reason)
        } else {
            format!("Blocked by anti-bot ({}): {}", self.vendor, self.reason)
        }
    }
}

/// Per-request extraction debug trace. One entry per extract() call
/// (multi-attempt JS escalation produces multiple attempts).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DebugExtraction {
    pub attempts: Vec<DebugAttempt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugAttempt {
    pub renderer: String,
    pub extracted_via: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_features: Option<serde_json::Value>,
    pub candidates: Vec<DebugCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugCandidate {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cap_chars: Option<usize>,
    pub score: f64,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

/// Generic API response wrapper.
///
/// Serializes camelCase like every other public type; the `error_code` field
/// therefore ships as `errorCode`. A deserialization `alias` keeps the legacy
/// snake_case key readable for any client still sending it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "error_code")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            error_code: None,
            warning: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(msg.into()),
            error_code: None,
            warning: None,
        }
    }

    pub fn err_with_code(msg: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(msg.into()),
            error_code: Some(code.into()),
            warning: None,
        }
    }
}

// ── Crawl types ──

/// POST /v1/crawl request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrawlRequest {
    pub url: String,
    pub max_depth: Option<u32>,
    #[serde(alias = "limit", alias = "max_pages")]
    pub max_pages: Option<u32>,
    #[serde(default = "default_formats")]
    pub formats: Vec<OutputFormat>,
    #[serde(default = "default_true")]
    pub only_main_content: bool,
    #[serde(default, alias = "json_schema")]
    pub json_schema: Option<serde_json::Value>,
    /// null = auto-detect (use global default), true = force JS, false = skip JS.
    /// Applies to every page fetched during the crawl.
    #[serde(default, alias = "render_js")]
    pub render_js: Option<bool>,
    /// Milliseconds to wait after JS rendering on each page.
    #[serde(default, alias = "wait_for")]
    pub wait_for: Option<u64>,
    /// Pin every page in this crawl to a specific renderer. See `ScrapeRequest::renderer`.
    #[serde(default)]
    pub renderer: Option<RequestedRenderer>,
    /// 2-letter ISO 3166-1 alpha-2 country code (e.g. "us", "gb") applied to
    /// every page fetched in this crawl. See `ScrapeRequest::country`.
    #[serde(default)]
    pub country: Option<String>,
    /// Per-crawl proxy pool to rotate among (BYOP). Takes precedence over the
    /// server's configured pool. Empty = use server config. Rotation is applied
    /// per page (see `proxy_rotation`). Accepts the snake_case `proxy_list` alias.
    #[serde(default, alias = "proxy_list")]
    pub proxy_list: Vec<String>,
    /// Rotation strategy for `proxy_list` (`round_robin`, `random`,
    /// `sticky_per_host`). `None` = server default (`sticky_per_host`).
    #[serde(default, alias = "proxy_rotation")]
    pub proxy_rotation: Option<crate::proxy::ProxyRotation>,
    /// Extra request headers applied to every page this crawl fetches, with the
    /// same semantics as [`ScrapeRequest::headers`], including its warning: on
    /// a browser render `Network.setExtraHTTPHeaders` decorates every request
    /// the page makes, subresources included, so cross-origin-sensitive
    /// credentials do not belong here.
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

/// Resolve the effective `render_js` decision from a per-request value and the
/// global default. Per-request always wins when set; otherwise fall back to the
/// default. `None` at both ends means "auto-detect".
///
/// Precedence table:
///
/// | request       | default       | effective    |
/// |---------------|---------------|--------------|
/// | `Some(true)`  | any           | `Some(true)` |
/// | `Some(false)` | any           | `Some(false)`|
/// | `None`        | `Some(true)`  | `Some(true)` |
/// | `None`        | `Some(false)` | `Some(false)`|
/// | `None`        | `None`        | `None`       |
pub fn resolve_render_js(request: Option<bool>, default: Option<bool>) -> Option<bool> {
    request.or(default)
}

/// Resolve the effective pinned renderer name from a per-request value.
///
/// Returns the renderer name (e.g. `"chrome"`) when a non-`Auto` renderer is pinned.
/// `None` and `Some(Auto)` both return `None` — meaning "use the configured chain".
pub fn resolve_pinned_renderer(req: Option<RequestedRenderer>) -> Option<&'static str> {
    req.and_then(|r| r.pinned_name())
}

/// Is this declared content type actually HTML (or HTML-like markup)?
///
/// Shared by the renderer (deciding whether a browser render can add
/// anything to a body) and the extractor (deciding whether it is safe to run
/// an HTML parser / HTML-to-markdown converter over the body at all). Absent
/// or unrecognised types stay eligible: a server that omits `Content-Type`
/// is still overwhelmingly likely to be serving HTML.
pub fn is_html_like_content_type(content_type: Option<&str>) -> bool {
    match content_type {
        None => true,
        Some(ct) => {
            let ct = ct.trim().to_ascii_lowercase();
            ct.is_empty()
                || ct == "text/html"
                || ct == "application/xhtml+xml"
                || ct == "application/xml"
                || ct == "text/xml"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fully populated document, so each test can vary just the field it is about.
    fn blocked_fixture() -> ScrapeData {
        ScrapeData {
            markdown: Some("Just a moment... Humans only".into()),
            source_hash: Some("sha256:abc".into()),
            html: Some("<html>challenge</html>".into()),
            raw_html: Some("<html>challenge</html>".into()),
            plain_text: Some("challenge text".into()),
            links: Some(vec!["https://help.example.com".into()]),
            images: Some(vec![ScrapedImage {
                url: "https://help.example.com/logo.png".into(),
                alt: Some("logo".into()),
            }]),
            json: Some(serde_json::json!({"junk": true})),
            summary: Some("a summary of junk".into()),
            llm_usage: None,
            chunks: None,
            warning: Some("blocked".into()),
            warnings: vec!["blocked".into()],
            render_decision: None,
            credit_cost: 0,
            basis: None,
            basis_warnings: Vec::new(),
            llm_input_hash: None,
            metadata: PageMetadata {
                title: None,
                description: None,
                og_title: None,
                og_description: None,
                og_image: None,
                canonical_url: None,
                source_url: "https://www.glassdoor.com/Reviews/x.htm".into(),
                language: None,
                status_code: 200,
                rendered_with: None,
                elapsed_ms: 0,
                page_count: None,
                source_filename: None,
                extra: Default::default(),
            },
            debug_extraction: None,
            content_type: Some("text/html".into()),
            change_tracking: None,
            screenshot: Some("data:image/png;base64,AA".into()),
            block: Some(BlockOutcome {
                vendor: "cloudflare".into(),
                reason: "cloudflare challenge interstitial".into(),
            }),
            truncated: false,
            cached: false,
        }
    }

    /// An ordinary origin error page: the status is the caller's, and there is no
    /// anti-bot verdict. `block` must be cleared explicitly — the fixture this
    /// builds on is a *walled* page, and `http_error` now returns `None` for
    /// anything carrying a block, so leaving it set would make every assertion
    /// here measure the block gate instead of the status gate.
    fn page(status: u16, markdown_len: usize) -> ScrapeData {
        let mut d = blocked_fixture();
        d.block = None;
        d.metadata.status_code = status;
        d.markdown = Some("x".repeat(markdown_len));
        d.plain_text = None;
        d.html = None;
        d.raw_html = None;
        d.warning = None;
        d
    }

    #[test]
    fn http_error_fires_on_an_error_page_and_spares_a_real_one() {
        // americastire.com: 403 with 650 bytes of CloudFront prose, billed as a
        // success in prod for months because the old bar was 200 bytes.
        assert!(page(403, 650).http_error().is_some());
        // stackoverflow.com/questions/tagged/rust answers 403 and serves the real
        // page (39,499 chars). The renderer keeps it on purpose; so must this.
        assert!(page(403, 39_499).http_error().is_none());
        // A thin page that the origin says is fine stays a success.
        assert!(page(200, 2).http_error().is_none());
    }

    #[test]
    fn a_block_verdict_outranks_the_origin_status() {
        // Cloudflare answers its challenge with 403. Reporting that as
        // `http_error` returned the "Just a moment..." shell as the page's
        // markdown, because `clear_body()` only runs on the block path.
        let mut walled = page(403, 650);
        walled.block = Some(BlockOutcome {
            vendor: "cloudflare".into(),
            reason: "cloudflare challenge interstitial".into(),
        });
        assert!(walled.http_error().is_none());
        // The same page without a verdict is still an ordinary origin error.
        walled.block = None;
        assert!(walled.http_error().is_some());
    }

    #[test]
    fn has_no_content_catches_the_shapes_that_were_billed_for_nothing() {
        // pdf_too_large: the decompression-bomb guard refused the document, so
        // markdown is empty and only a warning records why.
        let mut d = page(200, 0);
        d.warning = Some("pdf_too_large: document decompresses beyond the allowed size".into());
        assert!(d.has_no_content(&[OutputFormat::Markdown]));
        // aliexpress.com, 2026-08-17: same empty markdown, no warning at all —
        // which is why this keys on the outcome and not on warning text.
        d.warning = None;
        assert!(d.has_no_content(&[OutputFormat::Markdown]));
        // A legitimately thin page delivered what was asked for.
        assert!(!page(200, 1).has_no_content(&[OutputFormat::Markdown]));
    }

    #[test]
    fn has_no_content_judges_only_the_formats_that_were_requested() {
        let mut d = page(200, 0); // markdown present but empty
        d.links = Some(vec!["https://example.com/a".into()]);
        // Asking for links and getting links is a delivered scrape, even though
        // the markdown field happens to be empty.
        assert!(!d.has_no_content(&[OutputFormat::Links]));
        // Partial delivery across a multi-format request still counts.
        assert!(!d.has_no_content(&[OutputFormat::Markdown, OutputFormat::Links]));
    }

    #[test]
    fn has_no_content_accepts_a_page_that_genuinely_has_no_links() {
        // `formats:["links"]` over a page with no outbound links returns
        // `Some([])`. That is the complete, correct answer to what was asked —
        // failing it would bill-refund a scrape that worked, and it is the one
        // shape where "empty" and "missing" must not be conflated.
        let mut d = page(200, 0);
        d.markdown = None;
        d.images = None;
        d.links = Some(Vec::new());
        assert!(!d.has_no_content(&[OutputFormat::Links]));
        // Isolated from `links`, so an arm that read the wrong field would fail
        // here instead of riding on the assertion above.
        d.links = None;
        d.images = Some(Vec::new());
        assert!(!d.has_no_content(&[OutputFormat::Images]));
        assert!(d.has_no_content(&[OutputFormat::Links]));
        // Not requested at all is still nothing delivered.
        d.images = None;
        assert!(d.has_no_content(&[OutputFormat::Images]));
    }

    #[test]
    fn a_thin_error_page_keeps_its_status_classification() {
        // `structural_integrity_check` never reads the HTTP status, so a terse
        // 404 earns a `structural_failure` verdict on shape alone. If that
        // short-circuited `http_error`, every small error page would come back
        // `no_usable_content` with its body cleared instead of readable under
        // `http_error` — and would disagree with `crawl.rs`, which filters this
        // vendor out for the same reason.
        let mut d = page(404, 300);
        d.block = Some(BlockOutcome {
            vendor: STRUCTURAL_FAILURE_VENDOR.into(),
            reason: "Structural: minimal_text, no_content_elements".into(),
        });
        assert!(d.http_error().is_some());
        // `parked_domain` is NOT carved out: crawl.rs does not filter it either,
        // and a parked page is a real verdict about the destination on every
        // surface. Carving it out here is what would split them.
        d.block = Some(BlockOutcome {
            vendor: PARKED_DOMAIN_VENDOR.into(),
            reason: "parked domain".into(),
        });
        assert!(d.http_error().is_none());
        // A real vendor wall still outranks the status.
        d.block = Some(BlockOutcome {
            vendor: "datadome".into(),
            reason: "datadome interstitial".into(),
        });
        assert!(d.http_error().is_none());
    }

    #[test]
    fn has_no_content_catches_a_format_that_silently_failed() {
        // A screenshot is only captured on the CDP tier, and `summarize()`
        // failures become a warning. Both leave the field `None`, which is
        // indistinguishable from "not requested" without the formats list — this
        // is the whole reason `has_no_content` takes one.
        let mut d = page(200, 400);
        d.screenshot = None;
        assert!(d.has_no_content(&[OutputFormat::Screenshot]));
        d.summary = None;
        assert!(d.has_no_content(&[OutputFormat::Summary]));
        // Delivered, so not empty.
        d.screenshot = Some("data:image/png;base64,AA".into());
        assert!(!d.has_no_content(&[OutputFormat::Screenshot]));
    }

    #[test]
    fn has_no_content_treats_an_unchanged_page_as_an_answer() {
        // "nothing changed since `previous`" is a real result, the same
        // precedent as a confirmed zero-result search being a real search.
        let mut d = page(200, 0);
        d.markdown = None;
        d.change_tracking = Some(ChangeTrackingResult {
            status: ChangeStatus::Same,
            first_observation: false,
            content_hash: "sha256:same".into(),
            snapshot: None,
            diff: None,
            judgment: None,
            tag: None,
            truncated: false,
        });
        assert!(!d.has_no_content(&[OutputFormat::ChangeTracking]));
        // Requested but never produced is still nothing delivered.
        d.change_tracking = None;
        assert!(d.has_no_content(&[OutputFormat::ChangeTracking]));
        // An empty extraction found none of the schema's fields.
        d.json = Some(serde_json::json!({}));
        assert!(d.has_no_content(&[OutputFormat::Json]));
        d.json = Some(serde_json::json!({"title": "x"}));
        assert!(!d.has_no_content(&[OutputFormat::Json]));
    }

    #[test]
    fn has_no_content_does_not_fail_a_request_that_asked_for_nothing() {
        // `"formats": []` survives serde (the default only fills in when the
        // field is ABSENT), and `.any()` over an empty slice is vacuously false —
        // without the guard, every such scrape would hard-fail as
        // `no_usable_content` no matter what was actually fetched.
        let d = page(200, 400);
        assert!(!d.has_no_content(&[]));
        let empty = page(200, 0);
        assert!(!empty.has_no_content(&[]));
    }

    #[test]
    fn http_error_is_silent_when_there_is_no_body_to_judge() {
        // `formats:["screenshot"]` / `["links"]` populate none of the four body
        // fields. Nothing to measure is not a measurement of nothing.
        let mut d = page(403, 0);
        d.markdown = None;
        d.plain_text = None;
        d.html = None;
        d.raw_html = None;
        assert!(d.http_error().is_none());
    }

    #[test]
    fn http_error_measures_markup_when_no_text_was_requested() {
        // `formats:["rawHtml"]` populates neither text field. Measuring text only
        // would read 0 and fail every >= 400 response, including the large real
        // pages that soft-block statuses are known to carry.
        let mut d = page(403, 0);
        d.markdown = None;
        d.raw_html = Some("<html>".repeat(2_000));
        assert!(d.http_error().is_none());
        d.raw_html = Some("<html>403</html>".into());
        assert!(d.http_error().is_some());
    }

    #[test]
    fn http_error_ignores_raw_html_length() {
        // The old gate took `.max()` across markdown/text/html/rawHtml, so asking
        // for `rawHtml` made the bar unreachable and the gate never fired.
        let mut d = page(404, 250);
        d.raw_html = Some("<html>".repeat(10_000));
        assert!(d.http_error().is_some());
    }

    #[test]
    fn has_body_separates_a_placeholder_from_a_readable_error_page() {
        let mut d = ScrapeData::default();
        assert!(!d.has_body(), "a failed_page or cleared wall has no body");
        d.markdown = Some("404 Not Found".into());
        assert!(
            d.has_body(),
            "an origin error page kept readable has a body"
        );
        d.markdown = None;
        d.links = Some(vec!["https://example.com/a".into()]);
        assert!(d.has_body(), "a links-only format still delivered a page");
    }

    #[test]
    fn clear_body_drops_content_keeps_metadata_and_block() {
        let mut data = ScrapeData {
            markdown: Some("Just a moment... Humans only".into()),
            source_hash: Some("sha256:abc".into()),
            html: Some("<html>challenge</html>".into()),
            raw_html: Some("<html>challenge</html>".into()),
            plain_text: Some("challenge text".into()),
            links: Some(vec!["https://help.example.com".into()]),
            images: Some(vec![ScrapedImage {
                url: "https://help.example.com/logo.png".into(),
                alt: Some("logo".into()),
            }]),
            json: Some(serde_json::json!({"junk": true})),
            summary: Some("a summary of junk".into()),
            llm_usage: None,
            chunks: None,
            warning: Some("blocked".into()),
            warnings: vec!["blocked".into()],
            render_decision: None,
            credit_cost: 0,
            basis: None,
            basis_warnings: Vec::new(),
            llm_input_hash: None,
            metadata: PageMetadata {
                title: None,
                description: None,
                og_title: None,
                og_description: None,
                og_image: None,
                canonical_url: None,
                source_url: "https://www.glassdoor.com/Reviews/x.htm".into(),
                language: None,
                status_code: 200,
                rendered_with: None,
                elapsed_ms: 0,
                page_count: None,
                source_filename: None,
                extra: Default::default(),
            },
            debug_extraction: None,
            content_type: Some("text/html".into()),
            change_tracking: None,
            screenshot: Some("data:image/png;base64,AA".into()),
            block: Some(BlockOutcome {
                vendor: "cloudflare".into(),
                reason: "cloudflare challenge interstitial".into(),
            }),
            truncated: false,
            cached: false,
        };
        data.clear_body();
        // content-shell + LLM outputs cleared
        assert!(data.markdown.is_none());
        assert!(data.source_hash.is_none());
        assert!(data.html.is_none());
        assert!(data.raw_html.is_none());
        assert!(data.plain_text.is_none());
        assert!(data.links.is_none());
        assert!(data.images.is_none());
        assert!(data.json.is_none());
        assert!(data.summary.is_none());
        // block verdict, metadata, warnings, screenshot kept for the caller
        assert!(data.block.is_some());
        assert_eq!(data.metadata.status_code, 200);
        assert_eq!(data.warnings, vec!["blocked".to_string()]);
        assert!(data.screenshot.is_some());
    }

    fn usage(input: u32, output: u32) -> LlmUsage {
        LlmUsage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input + output,
            estimated_cost_usd: None,
            model: "crw-managed-pro".into(),
            provider: "azure".into(),
            cache_hit_input_tokens: None,
            cache_miss_input_tokens: None,
            truncated: false,
            calls: 1,
            executed_summaries: 0,
            answer_executed: false,
        }
    }

    #[test]
    fn llm_usage_accumulates_every_leg() {
        // A scrape can call the model three times: structured extraction, the
        // summary, and the change-tracking judge. Keeping only the first one is
        // how we ended up paying the provider for calls the SaaS never billed.
        let mut slot: Option<LlmUsage> = None;
        LlmUsage::accumulate(&mut slot, Some(usage(1000, 100))); // json
        LlmUsage::accumulate(&mut slot, Some(usage(500, 50))); // summary
        LlmUsage::accumulate(&mut slot, Some(usage(200, 20))); // judge

        let merged = slot.expect("usage");
        assert_eq!(merged.input_tokens, 1700);
        assert_eq!(merged.output_tokens, 170);
        assert_eq!(merged.total_tokens, 1870);
        assert_eq!(merged.calls, 3);
    }

    #[test]
    fn llm_usage_accumulate_seeds_an_empty_slot_and_ignores_none() {
        let mut slot: Option<LlmUsage> = None;
        LlmUsage::accumulate(&mut slot, None);
        assert!(slot.is_none(), "no call, no usage");

        LlmUsage::accumulate(&mut slot, Some(usage(10, 1)));
        LlmUsage::accumulate(&mut slot, None);
        let merged = slot.expect("usage");
        assert_eq!(merged.input_tokens, 10);
        assert_eq!(merged.calls, 1);
    }

    #[test]
    fn resolve_render_js_request_wins_true() {
        assert_eq!(resolve_render_js(Some(true), Some(false)), Some(true));
    }

    #[test]
    fn resolve_render_js_request_wins_false() {
        assert_eq!(resolve_render_js(Some(false), Some(true)), Some(false));
    }

    #[test]
    fn resolve_render_js_falls_back_to_default() {
        assert_eq!(resolve_render_js(None, Some(true)), Some(true));
        assert_eq!(resolve_render_js(None, Some(false)), Some(false));
    }

    #[test]
    fn resolve_render_js_both_none() {
        assert_eq!(resolve_render_js(None, None), None);
    }

    #[test]
    fn crawl_request_accepts_render_js_camel_case() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "renderJs": true,
            "waitFor": 2000
        });
        let req: CrawlRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.render_js, Some(true));
        assert_eq!(req.wait_for, Some(2000));
    }

    #[test]
    fn crawl_request_accepts_render_js_snake_case() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "render_js": false,
            "wait_for": 1500
        });
        let req: CrawlRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.render_js, Some(false));
        assert_eq!(req.wait_for, Some(1500));
    }

    #[test]
    fn crawl_request_render_fields_optional() {
        let json = serde_json::json!({ "url": "https://example.com" });
        let req: CrawlRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.render_js, None);
        assert_eq!(req.wait_for, None);
    }

    #[test]
    fn requested_renderer_deserializes_lowercase() {
        for (s, expected) in [
            ("\"auto\"", RequestedRenderer::Auto),
            ("\"lightpanda\"", RequestedRenderer::Lightpanda),
            ("\"chrome\"", RequestedRenderer::Chrome),
            ("\"playwright\"", RequestedRenderer::Playwright),
            ("\"camoufox\"", RequestedRenderer::Camoufox),
        ] {
            let parsed: RequestedRenderer = serde_json::from_str(s).unwrap();
            assert_eq!(parsed, expected, "input {s} should parse to {expected:?}");
        }
    }

    #[test]
    fn requested_renderer_camoufox_round_trip() {
        let parsed: RequestedRenderer = serde_json::from_str("\"camoufox\"").unwrap();
        assert_eq!(parsed, RequestedRenderer::Camoufox);
        let json = serde_json::to_string(&RequestedRenderer::Camoufox).unwrap();
        assert_eq!(json, "\"camoufox\"");
        assert_eq!(
            resolve_pinned_renderer(Some(RequestedRenderer::Camoufox)),
            Some("camoufox")
        );
        assert_eq!(RendererKind::Camoufox.as_str(), "camoufox");
    }

    #[test]
    fn requested_renderer_chrome_proxy_round_trip() {
        let parsed: RequestedRenderer = serde_json::from_str("\"chrome_proxy\"").unwrap();
        assert_eq!(parsed, RequestedRenderer::ChromeProxy);
        let json = serde_json::to_string(&RequestedRenderer::ChromeProxy).unwrap();
        assert_eq!(json, "\"chrome_proxy\"");
        assert_eq!(
            resolve_pinned_renderer(Some(RequestedRenderer::ChromeProxy)),
            Some("chrome_proxy")
        );
    }

    #[test]
    fn requested_renderer_impersonated_http_round_trip() {
        let parsed: RequestedRenderer = serde_json::from_str("\"impersonated-http\"").unwrap();
        assert_eq!(parsed, RequestedRenderer::ImpersonatedHttp);
        let json = serde_json::to_string(&RequestedRenderer::ImpersonatedHttp).unwrap();
        assert_eq!(json, "\"impersonated-http\"");
        assert_eq!(
            resolve_pinned_renderer(Some(RequestedRenderer::ImpersonatedHttp)),
            Some("impersonated-http")
        );
        assert_eq!(RendererKind::ImpersonatedHttp.as_str(), "impersonated-http");
        let k: RendererKind = serde_json::from_str("\"impersonated-http\"").unwrap();
        assert_eq!(k, RendererKind::ImpersonatedHttp);
        let back = serde_json::to_string(&RendererKind::ImpersonatedHttp).unwrap();
        assert_eq!(back, "\"impersonated-http\"");
    }

    #[test]
    fn requested_renderer_impersonated_http_does_not_imply_js() {
        // Every browser tier implies a JS render when the request omits
        // renderJs; the wire-level impersonated tier never executes JS, and
        // Auto is not a pin so it implies nothing (an explicit "auto" stays
        // equivalent to omitting the field).
        assert!(!RequestedRenderer::ImpersonatedHttp.implies_js());
        assert!(!RequestedRenderer::Auto.implies_js());
        assert!(RequestedRenderer::Lightpanda.implies_js());
        assert!(RequestedRenderer::Chrome.implies_js());
        assert!(RequestedRenderer::ChromeProxy.implies_js());
        assert!(RequestedRenderer::Playwright.implies_js());
        assert!(RequestedRenderer::Camoufox.implies_js());
    }

    #[test]
    fn requested_renderer_rejects_unknown() {
        let result: Result<RequestedRenderer, _> = serde_json::from_str("\"firefox\"");
        assert!(
            result.is_err(),
            "unknown renderer should fail to deserialize"
        );
    }

    #[test]
    fn scrape_request_accepts_renderer_field() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "renderer": "chrome"
        });
        let req: ScrapeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.renderer, Some(RequestedRenderer::Chrome));
    }

    #[test]
    fn scrape_request_renderer_explicit_null() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "renderer": null
        });
        let req: ScrapeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.renderer, None);
    }

    #[test]
    fn scrape_request_renderer_omitted() {
        let json = serde_json::json!({ "url": "https://example.com" });
        let req: ScrapeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.renderer, None);
    }

    #[test]
    fn crawl_request_accepts_renderer_field() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "renderer": "lightpanda"
        });
        let req: CrawlRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.renderer, Some(RequestedRenderer::Lightpanda));
    }

    #[test]
    fn resolve_pinned_renderer_auto_returns_none() {
        assert_eq!(resolve_pinned_renderer(Some(RequestedRenderer::Auto)), None);
        assert_eq!(resolve_pinned_renderer(None), None);
    }

    #[test]
    fn resolve_pinned_renderer_chrome_returns_name() {
        assert_eq!(
            resolve_pinned_renderer(Some(RequestedRenderer::Chrome)),
            Some("chrome")
        );
        assert_eq!(
            resolve_pinned_renderer(Some(RequestedRenderer::Lightpanda)),
            Some("lightpanda")
        );
        assert_eq!(
            resolve_pinned_renderer(Some(RequestedRenderer::Playwright)),
            Some("playwright")
        );
    }

    #[test]
    fn chrome_proxy_serializes_with_underscore() {
        let json = serde_json::to_string(&RendererKind::ChromeProxy).unwrap();
        assert_eq!(json, "\"chrome_proxy\"");
    }

    #[test]
    fn chrome_proxy_deserializes_from_underscore() {
        let k: RendererKind = serde_json::from_str("\"chrome_proxy\"").unwrap();
        assert_eq!(k, RendererKind::ChromeProxy);
    }

    #[test]
    fn chrome_proxy_as_str() {
        assert_eq!(RendererKind::ChromeProxy.as_str(), "chrome_proxy");
    }

    #[test]
    fn block_message_keeps_anti_bot_wording_for_a_real_vendor() {
        let b = BlockOutcome {
            vendor: "cloudflare".into(),
            reason: "CF challenge".into(),
        };
        assert_eq!(
            b.message(),
            "Blocked by anti-bot (cloudflare): CF challenge"
        );
    }

    /// A thin document is not a vendor wall. Reporting it as one sent customers
    /// to buy proxies for what was a broken certificate — `wrong.host.badssl.com`
    /// renders 21 visible characters and was labelled "Blocked by anti-bot".
    #[test]
    fn block_message_does_not_call_a_thin_page_a_block() {
        let b = BlockOutcome {
            // The LITERAL, not the constant. Building the fixture from
            // `STRUCTURAL_FAILURE_VENDOR` would make this test pass even if the
            // constant were a typo, since both sides would then be wrong
            // together and the branch would silently never fire in production.
            // The constant is pinned to its real producer separately, by
            // `structural_failure_vendor_matches_classifier` in crw-extract.
            vendor: "structural_failure".into(),
            reason: "Structural: minimal_text on small page (500 bytes, 21 chars visible)".into(),
        };
        let m = b.message();
        assert!(
            !m.contains("Blocked by anti-bot"),
            "structural failure must not read as a vendor block: {m}"
        );
        assert!(m.starts_with("No usable content could be extracted"), "{m}");
        // The diagnostic detail still reaches the caller.
        assert!(m.contains("21 chars visible"), "{m}");
    }

    /// Every other vendor string must keep the historical wording verbatim: it is
    /// documented customer contract and is shared byte-for-byte with the
    /// Firecrawl-compat surface.
    #[test]
    fn block_message_split_is_scoped_to_structural_failure_only() {
        for vendor in [
            "cloudflare",
            "datadome",
            "perimeterx",
            "akamai",
            "imperva",
            "sucuri",
            "kasada",
            "vercel",
            "network_security",
            "rate_limited",
            "generic_block",
        ] {
            let b = BlockOutcome {
                vendor: vendor.into(),
                reason: "r".into(),
            };
            assert_eq!(b.message(), format!("Blocked by anti-bot ({vendor}): r"));
        }
    }

    // ── shared fixtures for the expanded suite below ──

    fn min_metadata(source_url: &str) -> PageMetadata {
        PageMetadata {
            title: None,
            description: None,
            og_title: None,
            og_description: None,
            og_image: None,
            canonical_url: None,
            source_url: source_url.into(),
            language: None,
            status_code: 200,
            rendered_with: None,
            elapsed_ms: 0,
            page_count: None,
            source_filename: None,
            extra: Default::default(),
        }
    }

    fn min_scrape_data() -> ScrapeData {
        ScrapeData {
            markdown: None,
            source_hash: None,
            html: None,
            raw_html: None,
            plain_text: None,
            links: None,
            images: None,
            json: None,
            basis: None,
            basis_warnings: Vec::new(),
            llm_input_hash: None,
            summary: None,
            llm_usage: None,
            chunks: None,
            warning: None,
            warnings: Vec::new(),
            render_decision: None,
            credit_cost: 0,
            metadata: min_metadata("https://example.com"),
            debug_extraction: None,
            content_type: None,
            change_tracking: None,
            screenshot: None,
            block: None,
            truncated: false,
            cached: false,
        }
    }

    // ── OutputFormat ──

    #[test]
    fn output_format_parse_loose_all_canonical_names() {
        for (s, expected) in [
            ("markdown", OutputFormat::Markdown),
            ("html", OutputFormat::Html),
            ("rawHtml", OutputFormat::RawHtml),
            ("plainText", OutputFormat::PlainText),
            ("links", OutputFormat::Links),
            ("images", OutputFormat::Images),
            ("json", OutputFormat::Json),
            ("summary", OutputFormat::Summary),
            ("changeTracking", OutputFormat::ChangeTracking),
            ("screenshot", OutputFormat::Screenshot),
        ] {
            assert_eq!(OutputFormat::parse_loose(s).unwrap(), expected, "input {s}");
        }
    }

    #[test]
    fn output_format_parse_loose_aliases() {
        assert_eq!(
            OutputFormat::parse_loose("extract").unwrap(),
            OutputFormat::Json
        );
        assert_eq!(
            OutputFormat::parse_loose("llm-extract").unwrap(),
            OutputFormat::Json
        );
        assert_eq!(
            OutputFormat::parse_loose("change-tracking").unwrap(),
            OutputFormat::ChangeTracking
        );
        assert_eq!(
            OutputFormat::parse_loose("screenshot@fullPage").unwrap(),
            OutputFormat::Screenshot
        );
    }

    #[test]
    fn output_format_parse_loose_unknown_error_message() {
        let err = OutputFormat::parse_loose("pdf").unwrap_err();
        assert!(err.contains("Unknown format 'pdf'"), "{err}");
        assert!(err.contains("jsonSchema"), "{err}");
    }

    #[test]
    fn output_format_parse_loose_empty_string_errors() {
        assert!(OutputFormat::parse_loose("").is_err());
    }

    #[test]
    fn output_format_parse_loose_is_case_sensitive() {
        assert!(OutputFormat::parse_loose("Markdown").is_err());
        assert!(OutputFormat::parse_loose("JSON").is_err());
    }

    #[test]
    fn output_format_serializes_camel_case() {
        for (v, expected) in [
            (OutputFormat::Markdown, "\"markdown\""),
            (OutputFormat::RawHtml, "\"rawHtml\""),
            (OutputFormat::PlainText, "\"plainText\""),
            (OutputFormat::ChangeTracking, "\"changeTracking\""),
            (OutputFormat::Screenshot, "\"screenshot\""),
        ] {
            assert_eq!(serde_json::to_string(&v).unwrap(), expected);
        }
    }

    #[test]
    fn output_format_deserialize_rejects_non_string() {
        let result: Result<OutputFormat, _> = serde_json::from_value(serde_json::json!(1));
        assert!(result.is_err());
    }

    #[test]
    fn output_format_round_trip_vec_in_scrape_request() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "formats": ["markdown", "rawHtml", "extract", "screenshot@fullPage"]
        });
        let req: ScrapeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(
            req.formats,
            vec![
                OutputFormat::Markdown,
                OutputFormat::RawHtml,
                OutputFormat::Json,
                OutputFormat::Screenshot,
            ]
        );
    }

    // ── ChunkStrategy / FilterMode ──

    #[test]
    fn chunk_strategy_sentence_accepts_camel_case_on_deserialize() {
        let json = serde_json::json!({
            "type": "sentence",
            "maxChars": 500,
            "overlapChars": 50,
            "dedupe": true
        });
        let parsed: ChunkStrategy = serde_json::from_value(json).unwrap();
        match &parsed {
            ChunkStrategy::Sentence {
                max_chars,
                overlap_chars,
                dedupe,
            } => {
                assert_eq!(*max_chars, Some(500));
                assert_eq!(*overlap_chars, Some(50));
                assert_eq!(*dedupe, Some(true));
            }
            other => panic!("expected Sentence, got {other:?}"),
        }
        // BUG: the enum's `#[serde(rename_all = "camelCase")]` only renames the
        // `type` tag values (overridden per-variant anyway by explicit
        // `#[serde(rename = "sentence"|"regex"|"topic")]`); it does NOT cascade
        // into the struct-variant fields the way it would for a plain struct.
        // So `max_chars`/`overlap_chars` serialize snake_case even though the
        // aliases accept camelCase on the way in — a one-way asymmetry against
        // this API's "camelCase everywhere" contract. Harmless today because
        // `ScrapeRequest.chunk_strategy` is inbound-only and never echoed back
        // on `ScrapeData`, but would surface the moment something serializes a
        // `ChunkStrategy` into a response body.
        let out = serde_json::to_value(&parsed).unwrap();
        assert_eq!(out["type"], "sentence");
        assert_eq!(
            out["max_chars"], 500,
            "actual wire key is snake_case, not camelCase"
        );
        assert!(out.get("maxChars").is_none());
    }

    #[test]
    fn chunk_strategy_regex_requires_pattern_field() {
        let json = serde_json::json!({ "type": "regex" });
        let result: Result<ChunkStrategy, _> = serde_json::from_value(json);
        assert!(result.is_err(), "regex without pattern should fail");
    }

    #[test]
    fn chunk_strategy_regex_round_trip() {
        let json = serde_json::json!({ "type": "regex", "pattern": "\\n\\n+" });
        let parsed: ChunkStrategy = serde_json::from_value(json).unwrap();
        match parsed {
            ChunkStrategy::Regex {
                pattern,
                max_chars,
                overlap_chars,
                dedupe,
            } => {
                assert_eq!(pattern, "\\n\\n+");
                assert_eq!(max_chars, None);
                assert_eq!(overlap_chars, None);
                assert_eq!(dedupe, None);
            }
            other => panic!("expected Regex, got {other:?}"),
        }
    }

    #[test]
    fn chunk_strategy_topic_accepts_snake_case_aliases() {
        let json = serde_json::json!({
            "type": "topic",
            "max_chars": 300,
            "overlap_chars": 20
        });
        let parsed: ChunkStrategy = serde_json::from_value(json).unwrap();
        match parsed {
            ChunkStrategy::Topic {
                max_chars,
                overlap_chars,
                ..
            } => {
                assert_eq!(max_chars, Some(300));
                assert_eq!(overlap_chars, Some(20));
            }
            other => panic!("expected Topic, got {other:?}"),
        }
    }

    #[test]
    fn chunk_strategy_defaults_when_only_type_given() {
        for tag in ["sentence", "topic"] {
            let json = serde_json::json!({ "type": tag });
            let parsed: ChunkStrategy = serde_json::from_value(json).unwrap();
            let (max_chars, overlap_chars, dedupe) = match parsed {
                ChunkStrategy::Sentence {
                    max_chars,
                    overlap_chars,
                    dedupe,
                } => (max_chars, overlap_chars, dedupe),
                ChunkStrategy::Topic {
                    max_chars,
                    overlap_chars,
                    dedupe,
                } => (max_chars, overlap_chars, dedupe),
                other => panic!("unexpected {other:?}"),
            };
            assert_eq!(max_chars, None, "tag {tag}");
            assert_eq!(overlap_chars, None, "tag {tag}");
            assert_eq!(dedupe, None, "tag {tag}");
        }
    }

    #[test]
    fn chunk_strategy_unknown_tag_errors() {
        let json = serde_json::json!({ "type": "paragraph" });
        let result: Result<ChunkStrategy, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn filter_mode_serializes_camel_case() {
        assert_eq!(
            serde_json::to_string(&FilterMode::Bm25).unwrap(),
            "\"bm25\""
        );
        assert_eq!(
            serde_json::to_string(&FilterMode::Cosine).unwrap(),
            "\"cosine\""
        );
    }

    #[test]
    fn filter_mode_round_trip() {
        for s in ["\"bm25\"", "\"cosine\""] {
            let parsed: FilterMode = serde_json::from_str(s).unwrap();
            assert_eq!(serde_json::to_string(&parsed).unwrap(), s);
        }
    }

    #[test]
    fn filter_mode_rejects_unknown() {
        let result: Result<FilterMode, _> = serde_json::from_str("\"tfidf\"");
        assert!(result.is_err());
    }

    // ── RequestedRenderer (additional) ──

    #[test]
    fn requested_renderer_auto_round_trip() {
        let json = serde_json::to_string(&RequestedRenderer::Auto).unwrap();
        assert_eq!(json, "\"auto\"");
        let parsed: RequestedRenderer = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, RequestedRenderer::Auto);
    }

    #[test]
    fn requested_renderer_pinned_name_table() {
        for (v, expected) in [
            (RequestedRenderer::Auto, None),
            (RequestedRenderer::Lightpanda, Some("lightpanda")),
            (RequestedRenderer::Chrome, Some("chrome")),
            (RequestedRenderer::ChromeProxy, Some("chrome_proxy")),
            (RequestedRenderer::Playwright, Some("playwright")),
            (RequestedRenderer::Camoufox, Some("camoufox")),
        ] {
            assert_eq!(v.pinned_name(), expected, "{v:?}");
        }
    }

    #[test]
    fn requested_renderer_deserialize_rejects_non_string() {
        let result: Result<RequestedRenderer, _> = serde_json::from_value(serde_json::json!(1));
        assert!(result.is_err());
    }

    // ── ExtractOptions ──

    #[test]
    fn extract_options_round_trip_with_schema_and_prompt() {
        let opts = ExtractOptions {
            schema: Some(serde_json::json!({"type": "object"})),
            prompt: Some("extract the price".into()),
        };
        let json = serde_json::to_value(&opts).unwrap();
        assert_eq!(json["schema"]["type"], "object");
        assert_eq!(json["prompt"], "extract the price");
        let back: ExtractOptions = serde_json::from_value(json).unwrap();
        assert_eq!(back.prompt, opts.prompt);
    }

    #[test]
    fn extract_options_defaults_from_empty_object() {
        let opts: ExtractOptions = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(opts.schema.is_none());
        assert!(opts.prompt.is_none());
    }

    #[test]
    fn extract_options_prompt_omitted_when_none() {
        let opts = ExtractOptions {
            schema: None,
            prompt: None,
        };
        let json = serde_json::to_value(&opts).unwrap();
        assert!(json.get("prompt").is_none());
        // `schema` has no skip_serializing_if, so it stays present as null.
        assert!(json.get("schema").unwrap().is_null());
    }

    // ── ScrapeRequest ──

    #[test]
    fn scrape_request_default_matches_field_by_field() {
        let d = ScrapeRequest::default();
        assert_eq!(d.url, "");
        assert_eq!(d.formats, vec![OutputFormat::Markdown]);
        assert!(d.only_main_content);
        assert_eq!(d.render_js, None);
        assert_eq!(d.wait_for, None);
        assert!(d.include_tags.is_empty());
        assert!(d.exclude_tags.is_empty());
        assert_eq!(d.json_schema, None);
        assert!(!d.basis);
        assert!(d.headers.is_empty());
        assert_eq!(d.css_selector, None);
        assert_eq!(d.xpath, None);
        assert!(d.chunk_strategy.is_none());
        assert_eq!(d.query, None);
        assert!(d.filter_mode.is_none());
        assert_eq!(d.top_k, None);
        assert_eq!(d.proxy, None);
        assert!(d.proxy_list.is_empty());
        assert!(d.proxy_rotation.is_none());
        assert_eq!(d.country, None);
        assert_eq!(d.stealth, None);
        assert!(d.actions.is_none());
        assert!(d.extract.is_none());
        assert_eq!(d.llm_api_key, None);
        assert_eq!(d.llm_provider, None);
        assert_eq!(d.llm_model, None);
        assert_eq!(d.base_url, None);
        assert_eq!(d.summary_prompt, None);
        assert_eq!(d.max_content_chars, None);
        assert!(d.renderer.is_none());
        assert!(d.force_cloak.is_none());
        assert_eq!(d.deadline_ms, None);
        assert_eq!(d.debug, None);
        assert!(d.change_tracking.is_none());
        assert_eq!(d.goal, None);
        assert_eq!(d.judge_enabled, None);
        assert!(d.parsers.is_none());
        assert!(!d.screenshot_full_page);
    }

    #[test]
    fn scrape_request_default_matches_serde_defaults() {
        // The hand-written `Default` impl exists specifically to match what
        // deserializing `{"url": ""}` would produce; keep the two in sync.
        let from_json: ScrapeRequest =
            serde_json::from_value(serde_json::json!({ "url": "" })).unwrap();
        let hand_written = ScrapeRequest::default();
        assert_eq!(from_json.formats, hand_written.formats);
        assert_eq!(from_json.only_main_content, hand_written.only_main_content);
        assert_eq!(
            from_json.screenshot_full_page,
            hand_written.screenshot_full_page
        );
    }

    #[test]
    fn scrape_request_camel_case_field_names() {
        let mut req = ScrapeRequest {
            url: "https://example.com".into(),
            ..Default::default()
        };
        req.only_main_content = false;
        req.css_selector = Some(".main".into());
        req.chunk_strategy = Some(ChunkStrategy::Topic {
            max_chars: None,
            overlap_chars: None,
            dedupe: None,
        });
        req.filter_mode = Some(FilterMode::Bm25);
        req.top_k = Some(3);
        req.proxy_list = vec!["http://p:1".into()];
        req.proxy_rotation = Some(crate::proxy::ProxyRotation::Random);
        req.llm_api_key = Some("k".into());
        req.llm_provider = Some("openai".into());
        req.llm_model = Some("gpt".into());
        req.base_url = Some("https://api.example.com".into());
        req.summary_prompt = Some("be terse".into());
        req.max_content_chars = Some(1000);
        req.deadline_ms = Some(5000);
        req.change_tracking = Some(ChangeTrackingOptions::default());
        req.judge_enabled = Some(true);
        req.screenshot_full_page = true;

        let json = serde_json::to_value(&req).unwrap();
        for key in [
            "onlyMainContent",
            "cssSelector",
            "chunkStrategy",
            "filterMode",
            "topK",
            "proxyList",
            "proxyRotation",
            "llmApiKey",
            "llmProvider",
            "llmModel",
            "baseUrl",
            "summaryPrompt",
            "maxContentChars",
            "deadlineMs",
            "changeTracking",
            "judgeEnabled",
            "screenshotFullPage",
        ] {
            assert!(json.get(key).is_some(), "missing camelCase key {key}");
        }
        for key in [
            "only_main_content",
            "css_selector",
            "chunk_strategy",
            "filter_mode",
            "top_k",
            "proxy_list",
            "proxy_rotation",
            "llm_api_key",
            "base_url",
            "max_content_chars",
        ] {
            assert!(json.get(key).is_none(), "unexpected snake_case key {key}");
        }
    }

    #[test]
    fn scrape_request_snake_case_aliases_deserialize() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "only_main_content": false,
            "render_js": true,
            "wait_for": 1500,
            "include_tags": ["main"],
            "exclude_tags": ["nav"],
            "json_schema": {"type": "object"},
            "css_selector": "#a",
            "chunk_strategy": {"type": "topic"},
            "filter_mode": "cosine",
            "proxy_list": ["http://p:1"],
            "proxy_rotation": "round_robin",
            "llm_api_key": "k",
            "llm_provider": "anthropic",
            "llm_model": "claude",
            "base_url": "https://x.example.com",
            "summary_prompt": "hi",
            "max_content_chars": 200,
            "deadline_ms": 9000,
            "change_tracking": {},
            "judge_enabled": false,
            "screenshot_full_page": true
        });
        let req: ScrapeRequest = serde_json::from_value(json).unwrap();
        assert!(!req.only_main_content);
        assert_eq!(req.render_js, Some(true));
        assert_eq!(req.wait_for, Some(1500));
        assert_eq!(req.include_tags, vec!["main".to_string()]);
        assert_eq!(req.exclude_tags, vec!["nav".to_string()]);
        assert!(req.json_schema.is_some());
        assert_eq!(req.css_selector, Some("#a".into()));
        assert!(req.chunk_strategy.is_some());
        assert!(matches!(req.filter_mode, Some(FilterMode::Cosine)));
        assert_eq!(req.proxy_list, vec!["http://p:1".to_string()]);
        assert!(matches!(
            req.proxy_rotation,
            Some(crate::proxy::ProxyRotation::RoundRobin)
        ));
        assert_eq!(req.llm_api_key, Some("k".into()));
        assert_eq!(req.llm_provider, Some("anthropic".into()));
        assert_eq!(req.llm_model, Some("claude".into()));
        assert_eq!(req.base_url, Some("https://x.example.com".into()));
        assert_eq!(req.summary_prompt, Some("hi".into()));
        assert_eq!(req.max_content_chars, Some(200));
        assert_eq!(req.deadline_ms, Some(9000));
        assert!(req.change_tracking.is_some());
        assert_eq!(req.judge_enabled, Some(false));
        assert!(req.screenshot_full_page);
    }

    #[test]
    fn scrape_request_unknown_field_is_ignored_not_rejected() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "someFutureField": "whatever"
        });
        let req: ScrapeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.url, "https://example.com");
    }

    #[test]
    fn scrape_request_requires_url_field() {
        let result: Result<ScrapeRequest, _> = serde_json::from_value(serde_json::json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn scrape_request_only_main_content_defaults_true_when_omitted() {
        let req: ScrapeRequest =
            serde_json::from_value(serde_json::json!({ "url": "https://example.com" })).unwrap();
        assert!(req.only_main_content);
    }

    #[test]
    fn scrape_request_force_cloak_cannot_be_set_from_a_request_body() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "forceCloak": true
        });
        let req: ScrapeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(
            req.force_cloak, None,
            "forceCloak must never be settable from a caller-supplied body"
        );
    }

    #[test]
    fn scrape_request_force_cloak_omitted_when_none() {
        let req = ScrapeRequest {
            url: "https://example.com".into(),
            ..Default::default()
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("forceCloak").is_none());
    }

    #[test]
    fn scrape_request_basis_omitted_when_false() {
        let req = ScrapeRequest {
            url: "https://example.com".into(),
            ..Default::default()
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("basis").is_none());
    }

    #[test]
    fn scrape_request_basis_present_when_true() {
        let mut req = ScrapeRequest {
            url: "https://example.com".into(),
            ..Default::default()
        };
        req.basis = true;
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["basis"], true);
    }

    #[test]
    fn scrape_request_proxy_rotation_accepts_all_wire_variants() {
        for (s, expected) in [
            ("round_robin", crate::proxy::ProxyRotation::RoundRobin),
            ("random", crate::proxy::ProxyRotation::Random),
            (
                "sticky_per_host",
                crate::proxy::ProxyRotation::StickyPerHost,
            ),
        ] {
            let json = serde_json::json!({
                "url": "https://example.com",
                "proxyRotation": s
            });
            let req: ScrapeRequest = serde_json::from_value(json).unwrap();
            assert_eq!(req.proxy_rotation, Some(expected), "variant {s}");
        }
    }

    #[test]
    fn scrape_request_huge_deadline_ms_does_not_panic() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "deadlineMs": u64::MAX
        });
        let req: ScrapeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.deadline_ms, Some(u64::MAX));
    }

    #[test]
    fn scrape_request_negative_number_for_unsigned_field_errors() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "waitFor": -1
        });
        let result: Result<ScrapeRequest, _> = serde_json::from_value(json);
        assert!(result.is_err(), "u64 field must reject a negative number");
    }

    #[test]
    fn scrape_request_deeply_nested_json_schema_does_not_panic() {
        let mut nested = serde_json::json!("leaf");
        for _ in 0..200 {
            nested = serde_json::json!({ "properties": { "child": nested } });
        }
        let json = serde_json::json!({
            "url": "https://example.com",
            "jsonSchema": nested
        });
        let req: ScrapeRequest = serde_json::from_value(json).unwrap();
        assert!(req.json_schema.is_some());
    }

    #[test]
    fn scrape_request_truncated_json_str_errors_without_panic() {
        let truncated = r#"{"url": "https://example.com", "formats": ["markdown"#;
        let result: Result<ScrapeRequest, _> = serde_json::from_str(truncated);
        assert!(result.is_err());
    }

    #[test]
    fn scrape_request_unicode_and_emoji_url_round_trips() {
        let url = "https://例え.jp/ページ?q=🚀emoji";
        let json = serde_json::json!({ "url": url });
        let req: ScrapeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.url, url);
        let back = serde_json::to_value(&req).unwrap();
        assert_eq!(back["url"], url);
    }

    #[test]
    fn scrape_request_very_long_xpath_does_not_panic() {
        let long = "/".repeat(50_000);
        let json = serde_json::json!({
            "url": "https://example.com",
            "xpath": long
        });
        let req: ScrapeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.xpath.unwrap().len(), 50_000);
    }

    #[test]
    fn scrape_request_top_k_boundary_values() {
        for v in [0usize, 1, usize::MAX] {
            let json = serde_json::json!({ "url": "https://example.com", "topK": v });
            let req: ScrapeRequest = serde_json::from_value(json).unwrap();
            assert_eq!(req.top_k, Some(v));
        }
    }

    #[test]
    fn scrape_request_headers_map_round_trips() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "headers": { "X-Custom": "1", "Accept-Language": "tr" }
        });
        let req: ScrapeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.headers.get("X-Custom"), Some(&"1".to_string()));
        assert_eq!(req.headers.len(), 2);
    }

    #[test]
    fn scrape_request_extract_options_round_trip() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "extract": { "schema": { "type": "object" }, "prompt": "get price" }
        });
        let req: ScrapeRequest = serde_json::from_value(json).unwrap();
        let extract = req.extract.unwrap();
        assert_eq!(extract.prompt, Some("get price".into()));
        assert!(extract.schema.is_some());
    }

    // ── ParserSpec ──

    #[test]
    fn parser_spec_string_form_deserializes_bare_type() {
        let spec: ParserSpec = serde_json::from_value(serde_json::json!("pdf")).unwrap();
        assert_eq!(spec.parser_type, "pdf");
        assert_eq!(spec.mode, None);
        assert_eq!(spec.max_pages, None);
    }

    #[test]
    fn parser_spec_object_form_deserializes() {
        let json = serde_json::json!({ "type": "pdf", "mode": "ocr", "maxPages": 10 });
        let spec: ParserSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec.parser_type, "pdf");
        assert_eq!(spec.mode, Some("ocr".into()));
        assert_eq!(spec.max_pages, Some(10));
    }

    #[test]
    fn parser_spec_max_pages_accepts_snake_case_alias() {
        let json = serde_json::json!({ "type": "pdf", "max_pages": 4 });
        let spec: ParserSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec.max_pages, Some(4));
    }

    #[test]
    fn parser_spec_pdf_constructor() {
        let spec = ParserSpec::pdf();
        assert_eq!(spec.parser_type, "pdf");
        assert_eq!(spec.mode, None);
        assert_eq!(spec.max_pages, None);
    }

    #[test]
    fn parser_spec_always_serializes_object_form() {
        let spec: ParserSpec = serde_json::from_value(serde_json::json!("pdf")).unwrap();
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["type"], "pdf");
        assert!(json.get("mode").is_none());
        assert!(json.get("maxPages").is_none());
    }

    #[test]
    fn parser_spec_missing_type_in_object_form_errors() {
        let json = serde_json::json!({ "mode": "auto" });
        let result: Result<ParserSpec, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn parser_spec_equality() {
        assert_eq!(ParserSpec::pdf(), ParserSpec::pdf());
        let mut other = ParserSpec::pdf();
        other.max_pages = Some(1);
        assert_ne!(ParserSpec::pdf(), other);
    }

    // ── small helper functions ──

    #[test]
    fn default_formats_helper_is_markdown_only() {
        assert_eq!(default_formats(), vec![OutputFormat::Markdown]);
    }

    #[test]
    fn default_true_helper_returns_true() {
        assert!(default_true());
    }

    #[test]
    fn is_zero_u32_helper() {
        assert!(is_zero_u32(&0));
        assert!(!is_zero_u32(&1));
    }

    #[test]
    fn one_u32_and_is_one_u32_helpers() {
        assert_eq!(one_u32(), 1);
        assert!(is_one_u32(&1));
        assert!(!is_one_u32(&0));
        assert!(!is_one_u32(&2));
    }

    #[test]
    fn sum_opt_all_combinations() {
        assert_eq!(sum_opt(Some(2), Some(3)), Some(5));
        assert_eq!(sum_opt(Some(2), None), Some(2));
        assert_eq!(sum_opt(None, Some(3)), Some(3));
        assert_eq!(sum_opt(None, None), None);
    }

    #[test]
    fn sum_opt_saturates_at_u32_max() {
        assert_eq!(sum_opt(Some(u32::MAX), Some(1)), Some(u32::MAX));
    }

    // ── PageMetadata ──

    #[test]
    fn page_metadata_round_trip_minimal() {
        let meta = min_metadata("https://example.com/a");
        let json = serde_json::to_value(&meta).unwrap();
        let back: PageMetadata = serde_json::from_value(json).unwrap();
        assert_eq!(back.source_url, "https://example.com/a");
        assert_eq!(back.status_code, 200);
    }

    #[test]
    fn page_metadata_camel_case_field_names() {
        let mut meta = min_metadata("https://example.com/a");
        meta.og_title = Some("t".into());
        meta.og_description = Some("d".into());
        meta.og_image = Some("i".into());
        meta.canonical_url = Some("c".into());
        meta.rendered_with = Some("chrome".into());
        meta.page_count = Some(3);
        meta.source_filename = Some("f.pdf".into());
        let json = serde_json::to_value(&meta).unwrap();
        assert!(json.get("ogTitle").is_some());
        assert!(json.get("ogDescription").is_some());
        assert!(json.get("ogImage").is_some());
        assert!(json.get("canonicalUrl").is_some());
        assert!(json.get("renderedWith").is_some());
        assert!(
            json.get("numPages").is_some(),
            "page_count renames to numPages"
        );
        assert!(json.get("sourceFilename").is_some());
        // `source_url` is `#[serde(rename = "sourceURL")]`, not camelCase.
        assert!(json.get("sourceURL").is_some());
        assert!(json.get("source_url").is_none());
        assert!(json.get("pageCount").is_none());
    }

    #[test]
    fn page_metadata_skip_serializing_if_none_fields_omitted() {
        let meta = min_metadata("https://example.com/a");
        let json = serde_json::to_value(&meta).unwrap();
        for key in [
            "ogTitle",
            "ogDescription",
            "ogImage",
            "canonicalUrl",
            "language",
            "renderedWith",
            "numPages",
            "sourceFilename",
        ] {
            assert!(json.get(key).is_none(), "expected {key} to be omitted");
        }
        // title/description have no skip_serializing_if — always present as null.
        assert!(json.get("title").unwrap().is_null());
        assert!(json.get("description").unwrap().is_null());
    }

    #[test]
    fn page_metadata_extra_flatten_round_trips() {
        let json = serde_json::json!({
            "title": null,
            "description": null,
            "sourceURL": "https://example.com",
            "statusCode": 200,
            "elapsedMs": 5,
            "author": "us",
            "keywords": ["a", "b"]
        });
        let meta: PageMetadata = serde_json::from_value(json).unwrap();
        assert_eq!(meta.extra.get("author").unwrap(), "us");
        assert_eq!(
            meta.extra.get("keywords").unwrap(),
            &serde_json::json!(["a", "b"])
        );
        let back = serde_json::to_value(&meta).unwrap();
        assert_eq!(back["author"], "us");
    }

    #[test]
    fn page_metadata_deserialize_missing_extra_defaults_empty() {
        let json = serde_json::json!({
            "title": null,
            "description": null,
            "sourceURL": "https://example.com",
            "statusCode": 200,
            "elapsedMs": 0
        });
        let meta: PageMetadata = serde_json::from_value(json).unwrap();
        assert!(meta.extra.is_empty());
    }

    #[test]
    fn page_metadata_unicode_title_round_trips() {
        let mut meta = min_metadata("https://example.com");
        meta.title = Some("İstanbul'da hava çok güzel 🌤️".into());
        let json = serde_json::to_value(&meta).unwrap();
        let back: PageMetadata = serde_json::from_value(json).unwrap();
        assert_eq!(back.title, meta.title);
    }

    // ── LlmUsage ──

    #[test]
    fn llm_usage_round_trip_full() {
        let mut u = usage(100, 20);
        u.cache_hit_input_tokens = Some(10);
        u.cache_miss_input_tokens = Some(90);
        u.truncated = true;
        u.calls = 2;
        u.executed_summaries = 1;
        u.answer_executed = true;
        u.estimated_cost_usd = Some(0.0012);
        let json = serde_json::to_value(&u).unwrap();
        let back: LlmUsage = serde_json::from_value(json).unwrap();
        assert_eq!(back.input_tokens, 100);
        assert_eq!(back.cache_hit_input_tokens, Some(10));
        assert!(back.truncated);
        assert_eq!(back.calls, 2);
        assert_eq!(back.executed_summaries, 1);
        assert!(back.answer_executed);
    }

    #[test]
    fn llm_usage_calls_one_is_omitted_from_wire() {
        let u = usage(1, 1);
        let json = serde_json::to_value(&u).unwrap();
        assert!(
            json.get("calls").is_none(),
            "calls:1 is the default, should be skipped"
        );
    }

    #[test]
    fn llm_usage_calls_non_one_is_present_on_wire() {
        let mut u = usage(1, 1);
        u.calls = 3;
        let json = serde_json::to_value(&u).unwrap();
        assert_eq!(json["calls"], 3);
    }

    #[test]
    fn llm_usage_missing_calls_defaults_to_one_on_deserialize() {
        let json = serde_json::json!({
            "inputTokens": 5,
            "outputTokens": 5,
            "totalTokens": 10,
            "model": "m",
            "provider": "p"
        });
        let u: LlmUsage = serde_json::from_value(json).unwrap();
        assert_eq!(u.calls, 1);
        assert_eq!(u.executed_summaries, 0);
        assert!(!u.answer_executed);
    }

    #[test]
    fn llm_usage_wave2_optional_fields_omitted_when_none() {
        let u = usage(1, 1);
        let json = serde_json::to_value(&u).unwrap();
        assert!(json.get("cacheHitInputTokens").is_none());
        assert!(json.get("cacheMissInputTokens").is_none());
        assert!(
            json.get("truncated").is_none(),
            "truncated:false is skipped"
        );
    }

    #[test]
    fn llm_usage_executed_summaries_and_answer_executed_always_serialize() {
        let u = usage(1, 1);
        let json = serde_json::to_value(&u).unwrap();
        assert_eq!(json["executedSummaries"], 0);
        assert_eq!(json["answerExecuted"], false);
    }

    #[test]
    fn llm_usage_merge_sums_cache_tokens() {
        let mut a = usage(100, 10);
        a.cache_hit_input_tokens = Some(5);
        let mut b = usage(50, 5);
        b.cache_hit_input_tokens = Some(3);
        b.cache_miss_input_tokens = Some(47);
        a.merge(b);
        assert_eq!(a.cache_hit_input_tokens, Some(8));
        // `a` reported no miss count of its own, so its 95 un-cached tokens are
        // derived rather than dropped. This used to carry `b`'s Some(47)
        // forward unchanged, which priced `a`'s input at zero downstream: the
        // biller prefers an explicit miss count over deriving `input - hit`.
        assert_eq!(a.cache_miss_input_tokens, Some(142));
        assert_eq!(a.input_tokens, 150);
        // The parts account for the whole.
        assert_eq!(
            a.cache_hit_input_tokens.unwrap() + a.cache_miss_input_tokens.unwrap(),
            a.input_tokens
        );
    }

    #[test]
    fn llm_usage_merge_cost_both_present_sums() {
        let mut a = usage(1, 1);
        a.estimated_cost_usd = Some(0.5);
        let mut b = usage(1, 1);
        b.estimated_cost_usd = Some(0.25);
        a.merge(b);
        assert_eq!(a.estimated_cost_usd, Some(0.75));
    }

    #[test]
    fn llm_usage_merge_cost_one_side_none_keeps_the_other() {
        let mut a = usage(1, 1);
        a.estimated_cost_usd = Some(0.5);
        let b = usage(1, 1); // cost None
        a.merge(b);
        assert_eq!(a.estimated_cost_usd, Some(0.5));

        let mut a2 = usage(1, 1); // cost None
        let mut b2 = usage(1, 1);
        b2.estimated_cost_usd = Some(0.25);
        a2.merge(b2);
        assert_eq!(a2.estimated_cost_usd, Some(0.25));
    }

    #[test]
    fn llm_usage_merge_ors_truncated_flag() {
        let mut a = usage(1, 1);
        let mut b = usage(1, 1);
        b.truncated = true;
        a.merge(b);
        assert!(a.truncated);
    }

    #[test]
    fn llm_usage_merge_treats_a_leg_with_calls_zero_as_one_leg() {
        let mut a = usage(1, 1);
        let mut b = usage(1, 1);
        b.calls = 0;
        a.merge(b);
        // `other.calls.max(1)` — a leg always counts for at least 1 call even if
        // it reported 0.
        assert_eq!(a.calls, 2);
    }

    // ── ChunkResult / ScrapedImage ──

    #[test]
    fn chunk_result_round_trip_with_score() {
        let c = ChunkResult {
            content: "hello".into(),
            score: Some(0.87),
            index: 0,
        };
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["score"], 0.87);
        let back: ChunkResult = serde_json::from_value(json).unwrap();
        assert_eq!(back.content, "hello");
    }

    #[test]
    fn chunk_result_score_omitted_when_none() {
        let c = ChunkResult {
            content: "hi".into(),
            score: None,
            index: 1,
        };
        let json = serde_json::to_value(&c).unwrap();
        assert!(json.get("score").is_none());
    }

    #[test]
    fn scraped_image_equality_and_round_trip() {
        let a = ScrapedImage {
            url: "https://example.com/x.png".into(),
            alt: Some("logo".into()),
        };
        let b = a.clone();
        assert_eq!(a, b);
        let json = serde_json::to_value(&a).unwrap();
        assert_eq!(json["url"], "https://example.com/x.png");
        assert_eq!(json["alt"], "logo");
    }

    #[test]
    fn scraped_image_alt_omitted_when_none() {
        let img = ScrapedImage {
            url: "https://example.com/x.png".into(),
            alt: None,
        };
        let json = serde_json::to_value(&img).unwrap();
        assert!(json.get("alt").is_none());
    }

    // ── ScrapeData (additional) ──

    #[test]
    fn scrape_data_camel_case_field_names() {
        let mut d = min_scrape_data();
        d.source_hash = Some("sha256:x".into());
        d.llm_input_hash = Some("sha256:y".into());
        d.basis_warnings = vec![];
        d.render_decision = None;
        d.credit_cost = 1;
        d.debug_extraction = None;
        d.content_type = Some("text/html".into());
        d.plain_text = Some("text".into());
        d.raw_html = Some("<html></html>".into());
        let json = serde_json::to_value(&d).unwrap();
        for key in [
            "sourceHash",
            "llmInputHash",
            "creditCost",
            "contentType",
            "plainText",
            "rawHtml",
        ] {
            assert!(json.get(key).is_some(), "missing {key}");
        }
        for key in [
            "source_hash",
            "llm_input_hash",
            "credit_cost",
            "content_type",
        ] {
            assert!(json.get(key).is_none(), "unexpected snake_case {key}");
        }
    }

    #[test]
    fn scrape_data_source_hash_omitted_when_none() {
        let d = min_scrape_data();
        let json = serde_json::to_value(&d).unwrap();
        assert!(json.get("sourceHash").is_none());
    }

    #[test]
    fn scrape_data_credit_cost_omitted_when_zero() {
        let d = min_scrape_data();
        let json = serde_json::to_value(&d).unwrap();
        assert!(json.get("creditCost").is_none());
    }

    #[test]
    fn scrape_data_credit_cost_present_when_nonzero() {
        let mut d = min_scrape_data();
        d.credit_cost = 2;
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["creditCost"], 2);
    }

    #[test]
    fn scrape_data_truncated_omitted_when_false() {
        let d = min_scrape_data();
        let json = serde_json::to_value(&d).unwrap();
        assert!(json.get("truncated").is_none());
    }

    #[test]
    fn scrape_data_truncated_present_when_true() {
        let mut d = min_scrape_data();
        d.truncated = true;
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["truncated"], true);
    }

    #[test]
    fn scrape_data_warnings_omitted_when_empty() {
        let d = min_scrape_data();
        let json = serde_json::to_value(&d).unwrap();
        assert!(json.get("warnings").is_none());
    }

    #[test]
    fn scrape_data_basis_round_trip_supported_field() {
        let mut d = min_scrape_data();
        d.basis = Some(vec![crate::evidence::Basis {
            basis_version: 1,
            field: "price".into(),
            value: Some(serde_json::json!(9.99)),
            status: crate::evidence::FieldStatus::Supported,
            confidence: None,
            reasoning: None,
            citations: Vec::new(),
        }]);
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["basis"][0]["field"], "price");
        assert_eq!(json["basis"][0]["status"], "supported");
        let back: ScrapeData = serde_json::from_value(json).unwrap();
        assert_eq!(back.basis.unwrap()[0].value, Some(serde_json::json!(9.99)));
    }

    #[test]
    fn scrape_data_basis_warnings_round_trip() {
        let mut d = min_scrape_data();
        d.basis_warnings = vec![crate::evidence::BasisWarning {
            field: "price".into(),
            code: "unverified_citation".into(),
        }];
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["basisWarnings"][0]["code"], "unverified_citation");
    }

    #[test]
    fn scrape_data_block_omitted_when_none() {
        let d = min_scrape_data();
        let json = serde_json::to_value(&d).unwrap();
        assert!(json.get("block").is_none());
    }

    #[test]
    fn scrape_data_http_error_uses_custom_warning_text_when_present() {
        let mut d = min_scrape_data();
        d.metadata.status_code = 403;
        d.warning = Some("proxy blocked".into());
        assert_eq!(d.http_error(), None, "no body means nothing to judge");
        d.markdown = Some("x".into());
        assert_eq!(d.http_error(), Some("proxy blocked".into()));
    }

    #[test]
    fn scrape_data_http_error_default_message_when_no_warning() {
        let mut d = min_scrape_data();
        d.metadata.status_code = 500;
        d.markdown = Some("x".into());
        assert_eq!(d.http_error(), Some("Target returned HTTP 500".into()));
    }

    #[test]
    fn scrape_data_http_error_boundary_exactly_at_threshold_is_real_page() {
        let mut d = min_scrape_data();
        d.metadata.status_code = 404;
        d.markdown = Some("x".repeat(2_500));
        assert_eq!(
            d.http_error(),
            None,
            "exactly at ERROR_PAGE_MAX_TEXT counts as real"
        );
    }

    #[test]
    fn scrape_data_http_error_one_byte_under_threshold_fires() {
        let mut d = min_scrape_data();
        d.metadata.status_code = 404;
        d.markdown = Some("x".repeat(2_499));
        assert!(d.http_error().is_some());
    }

    #[test]
    fn scrape_data_http_error_status_399_never_fires() {
        let mut d = min_scrape_data();
        d.metadata.status_code = 399;
        d.markdown = Some("x".into());
        assert_eq!(d.http_error(), None);
    }

    #[test]
    fn scrape_data_http_error_status_exactly_400_can_fire() {
        let mut d = min_scrape_data();
        d.metadata.status_code = 400;
        d.markdown = Some("x".into());
        assert!(d.http_error().is_some());
    }

    #[test]
    fn scrape_data_deserialize_missing_optional_fields_defaults_cleanly() {
        let json = serde_json::json!({
            "metadata": {
                "title": null,
                "description": null,
                "sourceURL": "https://example.com",
                "statusCode": 200,
                "elapsedMs": 0
            }
        });
        let d: ScrapeData = serde_json::from_value(json).unwrap();
        assert!(d.markdown.is_none());
        assert!(d.warnings.is_empty());
        assert_eq!(d.credit_cost, 0);
        assert!(!d.truncated);
        assert!(d.block.is_none());
    }

    // ── BlockOutcome / vendor constants ──

    #[test]
    fn block_outcome_camel_case_round_trip() {
        let b = BlockOutcome {
            vendor: "datadome".into(),
            reason: "js challenge".into(),
        };
        let json = serde_json::to_value(&b).unwrap();
        assert_eq!(json["vendor"], "datadome");
        assert_eq!(json["reason"], "js challenge");
        let back: BlockOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(back.vendor, "datadome");
    }

    #[test]
    fn vendor_constants_have_expected_values() {
        assert_eq!(STRUCTURAL_FAILURE_VENDOR, "structural_failure");
        assert_eq!(HTTP_ERROR_VENDOR, "http_error");
        assert_eq!(PARKED_DOMAIN_VENDOR, "parked_domain");
    }

    #[test]
    fn block_message_parked_domain_uses_soft_wording() {
        let b = BlockOutcome {
            vendor: PARKED_DOMAIN_VENDOR.into(),
            reason: "GoDaddy parking page".into(),
        };
        let m = b.message();
        assert!(m.starts_with("No usable content could be extracted"));
        assert!(!m.contains("Blocked by anti-bot"));
    }

    #[test]
    fn block_message_http_error_vendor_uses_anti_bot_wording() {
        // http_error is deliberately NOT special-cased in `message()` — only
        // structural_failure and parked_domain get the softer phrasing.
        let b = BlockOutcome {
            vendor: HTTP_ERROR_VENDOR.into(),
            reason: "origin returned 500".into(),
        };
        assert_eq!(
            b.message(),
            "Blocked by anti-bot (http_error): origin returned 500"
        );
    }

    // ── DebugExtraction / DebugAttempt / DebugCandidate ──

    #[test]
    fn debug_extraction_default_is_empty_attempts() {
        let d = DebugExtraction::default();
        assert!(d.attempts.is_empty());
    }

    #[test]
    fn debug_candidate_optional_fields_omitted_when_none() {
        let c = DebugCandidate {
            kind: "readability".into(),
            text: None,
            text_excerpt: None,
            cap_chars: None,
            score: 0.5,
        };
        let json = serde_json::to_value(&c).unwrap();
        assert!(json.get("text").is_none());
        assert!(json.get("textExcerpt").is_none());
        assert!(json.get("capChars").is_none());
        assert_eq!(json["score"], 0.5);
    }

    #[test]
    fn debug_extraction_full_nested_round_trip() {
        let d = DebugExtraction {
            attempts: vec![DebugAttempt {
                renderer: "chrome".into(),
                extracted_via: "readability".into(),
                candidate_features: Some(serde_json::json!({"len": 100})),
                candidates: vec![DebugCandidate {
                    kind: "readability".into(),
                    text: Some("body text".into()),
                    text_excerpt: Some("body...".into()),
                    cap_chars: Some(500),
                    score: 0.9,
                }],
            }],
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["attempts"][0]["renderer"], "chrome");
        assert_eq!(json["attempts"][0]["extractedVia"], "readability");
        assert_eq!(json["attempts"][0]["candidates"][0]["capChars"], 500);
        let back: DebugExtraction = serde_json::from_value(json).unwrap();
        assert_eq!(back.attempts.len(), 1);
    }

    // ── ApiResponse ──

    #[test]
    fn api_response_ok_round_trip() {
        let r = ApiResponse::ok(serde_json::json!({"a": 1}));
        assert!(r.success);
        assert!(r.error.is_none());
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["data"]["a"], 1);
        assert!(json.get("error").is_none());
        assert!(json.get("errorCode").is_none());
        assert!(json.get("warning").is_none());
    }

    #[test]
    fn api_response_err_round_trip() {
        let r: ApiResponse<serde_json::Value> = ApiResponse::err("bad url");
        assert!(!r.success);
        assert!(r.data.is_none());
        assert_eq!(r.error, Some("bad url".into()));
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.get("data").is_none());
        assert_eq!(json["error"], "bad url");
    }

    #[test]
    fn api_response_err_with_code_round_trip() {
        let r: ApiResponse<serde_json::Value> =
            ApiResponse::err_with_code("bad url", "invalid_request");
        assert_eq!(r.error_code, Some("invalid_request".into()));
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["errorCode"], "invalid_request");
    }

    #[test]
    fn api_response_deserialize_accepts_snake_case_error_code_alias() {
        let json = serde_json::json!({
            "success": false,
            "error": "bad",
            "error_code": "invalid_request"
        });
        let r: ApiResponse<serde_json::Value> = serde_json::from_value(json).unwrap();
        assert_eq!(r.error_code, Some("invalid_request".into()));
    }

    #[test]
    fn api_response_warning_omitted_when_none() {
        let r = ApiResponse::ok(1u32);
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.get("warning").is_none());
    }

    // ── CrawlRequest (additional) ──

    #[test]
    fn crawl_request_max_pages_accepts_limit_alias() {
        let json = serde_json::json!({ "url": "https://example.com", "limit": 25 });
        let req: CrawlRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.max_pages, Some(25));
    }

    #[test]
    fn crawl_request_max_pages_accepts_snake_case_alias() {
        let json = serde_json::json!({ "url": "https://example.com", "max_pages": 30 });
        let req: CrawlRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.max_pages, Some(30));
    }

    #[test]
    fn crawl_request_camel_case_max_pages_primary_name() {
        let json = serde_json::json!({ "url": "https://example.com", "maxPages": 40 });
        let req: CrawlRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.max_pages, Some(40));
    }

    #[test]
    fn crawl_request_proxy_rotation_and_country_round_trip() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "country": "de",
            "proxyList": ["http://p:1"],
            "proxyRotation": "sticky_per_host"
        });
        let req: CrawlRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.country, Some("de".into()));
        assert_eq!(req.proxy_list, vec!["http://p:1".to_string()]);
        assert!(matches!(
            req.proxy_rotation,
            Some(crate::proxy::ProxyRotation::StickyPerHost)
        ));
    }

    #[test]
    fn crawl_request_formats_and_only_main_content_defaults() {
        let json = serde_json::json!({ "url": "https://example.com" });
        let req: CrawlRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.formats, vec![OutputFormat::Markdown]);
        assert!(req.only_main_content);
        assert!(req.max_depth.is_none());
        assert!(req.max_pages.is_none());
    }

    #[test]
    fn crawl_request_headers_round_trip_and_default_empty() {
        let req: CrawlRequest = serde_json::from_value(serde_json::json!({
            "url": "https://example.com",
            "headers": { "X-Custom": "1", "User-Agent": "test" }
        }))
        .unwrap();
        assert_eq!(req.headers.get("X-Custom"), Some(&"1".to_string()));
        assert_eq!(req.headers.get("User-Agent"), Some(&"test".to_string()));

        // Absent `headers` must stay an empty map, not a deserialize error, so
        // every crawl body written before this field existed still parses.
        let bare: CrawlRequest =
            serde_json::from_value(serde_json::json!({ "url": "https://example.com" })).unwrap();
        assert!(bare.headers.is_empty());
    }

    #[test]
    fn crawl_request_requires_url_field() {
        let result: Result<CrawlRequest, _> = serde_json::from_value(serde_json::json!({}));
        assert!(result.is_err());
    }

    // ── CrawlStatus / CrawlState / CrawlStartResponse ──

    #[test]
    fn crawl_status_serde_rename_each_variant() {
        for (v, expected) in [
            (CrawlStatus::InProgress, "\"scraping\""),
            (CrawlStatus::Completed, "\"completed\""),
            (CrawlStatus::Failed, "\"failed\""),
            (CrawlStatus::Cancelled, "\"cancelled\""),
        ] {
            assert_eq!(serde_json::to_string(&v).unwrap(), expected);
            let back: CrawlStatus = serde_json::from_str(expected).unwrap();
            assert_eq!(back, v);
        }
    }

    #[test]
    fn crawl_status_terminal_states() {
        assert!(matches!(
            CrawlStatus::Completed,
            CrawlStatus::Completed | CrawlStatus::Failed | CrawlStatus::Cancelled
        ));
        assert!(matches!(
            CrawlStatus::Cancelled,
            CrawlStatus::Completed | CrawlStatus::Failed | CrawlStatus::Cancelled
        ));
        assert!(!matches!(
            CrawlStatus::InProgress,
            CrawlStatus::Completed | CrawlStatus::Failed | CrawlStatus::Cancelled
        ));
    }

    #[test]
    fn crawl_state_id_is_never_serialized() {
        let state = CrawlState {
            id: Uuid::nil(),
            success: true,
            status: CrawlStatus::Completed,
            total: 1,
            completed: 1,
            blocked: 0,
            data: Vec::new(),
            error: None,
        };
        let json = serde_json::to_value(&state).unwrap();
        assert!(json.get("id").is_none());
        assert!(json.get("error").is_none());
        assert_eq!(json["status"], "completed");
    }

    #[test]
    fn crawl_state_blocked_defaults_to_zero_for_older_payloads() {
        let json = serde_json::json!({
            "id": Uuid::nil(),
            "success": true,
            "status": "completed",
            "total": 1,
            "completed": 1,
            "data": []
        });
        let state: CrawlState = serde_json::from_value(json).unwrap();
        assert_eq!(state.blocked, 0);
    }

    #[test]
    fn crawl_start_response_round_trip() {
        let r = CrawlStartResponse {
            success: true,
            id: "job-123".into(),
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["id"], "job-123");
        let back: CrawlStartResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back.id, "job-123");
    }

    // ── MapRequest / MapData / MapResponse ──

    #[test]
    fn map_request_defaults_use_sitemap_and_crawl_fallback_true() {
        let req: MapRequest =
            serde_json::from_value(serde_json::json!({ "url": "https://example.com" })).unwrap();
        assert!(req.use_sitemap);
        assert!(req.crawl_fallback);
        assert!(req.max_depth.is_none());
        assert!(req.timeout.is_none());
        assert!(req.limit.is_none());
    }

    #[test]
    fn map_request_camel_case_field_names() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "maxDepth": 2,
            "useSitemap": false,
            "crawlFallback": false,
            "stripTrackingParams": true,
            "dropActionUrls": true,
            "ignoreQueryParameters": true,
            "extraTrackingParams": ["ref"],
            "extraActionParams": ["logout"],
            "preserveParams": ["page"],
            "limit": 5000
        });
        let req: MapRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.max_depth, Some(2));
        assert!(!req.use_sitemap);
        assert!(!req.crawl_fallback);
        assert_eq!(req.strip_tracking_params, Some(true));
        assert_eq!(req.drop_action_urls, Some(true));
        assert_eq!(req.ignore_query_parameters, Some(true));
        assert_eq!(req.extra_tracking_params, Some(vec!["ref".to_string()]));
        assert_eq!(req.extra_action_params, Some(vec!["logout".to_string()]));
        assert_eq!(req.preserve_params, Some(vec!["page".to_string()]));
        assert_eq!(req.limit, Some(5000));
    }

    #[test]
    fn map_data_defaults_zero_counts_and_empty_sitemaps() {
        let data: MapData = serde_json::from_value(serde_json::json!({ "links": [] })).unwrap();
        assert_eq!(data.dropped_action_count, 0);
        assert_eq!(data.stripped_tracking_count, 0);
        assert!(data.sitemaps.is_empty());
    }

    #[test]
    fn map_data_camel_case_field_names() {
        let data = MapData {
            links: vec!["https://example.com/a".into()],
            dropped_action_count: 2,
            stripped_tracking_count: 3,
            sitemaps: vec!["https://example.com/sitemap.xml".into()],
        };
        let json = serde_json::to_value(&data).unwrap();
        assert_eq!(json["droppedActionCount"], 2);
        assert_eq!(json["strippedTrackingCount"], 3);
        assert_eq!(json["sitemaps"][0], "https://example.com/sitemap.xml");
    }

    #[test]
    fn map_response_type_alias_ok_and_err() {
        let ok: MapResponse = ApiResponse::ok(MapData {
            links: vec!["https://example.com".into()],
            dropped_action_count: 0,
            stripped_tracking_count: 0,
            sitemaps: Vec::new(),
        });
        assert!(ok.success);
        let err: MapResponse = ApiResponse::err("bad url");
        assert!(!err.success);
        assert!(err.data.is_none());
    }

    // ── Search types ──

    #[test]
    fn search_source_searxng_category_mapping() {
        assert_eq!(SearchSource::Web.searxng_category(), "general");
        assert_eq!(SearchSource::News.searxng_category(), "news");
        assert_eq!(SearchSource::Images.searxng_category(), "images");
    }

    #[test]
    fn search_source_serde_lowercase() {
        for (v, s) in [
            (SearchSource::Web, "\"web\""),
            (SearchSource::News, "\"news\""),
            (SearchSource::Images, "\"images\""),
        ] {
            assert_eq!(serde_json::to_string(&v).unwrap(), s);
            let back: SearchSource = serde_json::from_str(s).unwrap();
            assert_eq!(back, v);
        }
    }

    #[test]
    fn search_category_from_string_curated_variants() {
        assert_eq!(
            SearchCategory::from("github".to_string()),
            SearchCategory::Github
        );
        assert_eq!(
            SearchCategory::from("research".to_string()),
            SearchCategory::Research
        );
        assert_eq!(SearchCategory::from("pdf".to_string()), SearchCategory::Pdf);
    }

    #[test]
    fn search_category_from_string_other_passthrough() {
        let cat = SearchCategory::from("science".to_string());
        assert_eq!(cat, SearchCategory::Other("science".into()));
        assert_eq!(cat.as_str(), "science");
    }

    #[test]
    fn search_category_serialize_deserialize_round_trip() {
        for s in ["\"github\"", "\"research\"", "\"pdf\"", "\"news\""] {
            let parsed: SearchCategory = serde_json::from_str(s).unwrap();
            assert_eq!(serde_json::to_string(&parsed).unwrap(), s);
        }
    }

    #[test]
    fn search_time_filter_serde_rename_each_variant() {
        for (v, s) in [
            (SearchTimeFilter::Hour, "\"qdr:h\""),
            (SearchTimeFilter::Day, "\"qdr:d\""),
            (SearchTimeFilter::Week, "\"qdr:w\""),
            (SearchTimeFilter::Month, "\"qdr:m\""),
            (SearchTimeFilter::Year, "\"qdr:y\""),
        ] {
            assert_eq!(serde_json::to_string(&v).unwrap(), s);
            let back: SearchTimeFilter = serde_json::from_str(s).unwrap();
            assert_eq!(back, v);
        }
    }

    #[test]
    fn search_time_filter_searxng_time_range_mapping() {
        assert_eq!(SearchTimeFilter::Hour.searxng_time_range(), "day");
        assert_eq!(SearchTimeFilter::Day.searxng_time_range(), "day");
        assert_eq!(SearchTimeFilter::Week.searxng_time_range(), "week");
        assert_eq!(SearchTimeFilter::Month.searxng_time_range(), "month");
        assert_eq!(SearchTimeFilter::Year.searxng_time_range(), "year");
    }

    #[test]
    fn search_scrape_options_defaults_from_empty_object() {
        let opts: SearchScrapeOptions = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(opts.formats, vec![OutputFormat::Markdown]);
        assert!(opts.only_main_content);
        assert!(opts.country.is_none());
        assert!(opts.timeout.is_none());
    }

    #[test]
    fn search_scrape_options_camel_case_field_names() {
        let opts = SearchScrapeOptions {
            formats: vec![OutputFormat::Markdown],
            only_main_content: false,
            country: Some("gb".into()),
            timeout: Some(5000),
        };
        let json = serde_json::to_value(&opts).unwrap();
        assert!(json.get("onlyMainContent").is_some());
        assert_eq!(json["country"], "gb");
        assert_eq!(json["timeout"], 5000);
    }

    #[test]
    fn search_request_paid_rescue_cannot_be_set_from_a_request_body() {
        let json = serde_json::json!({
            "query": "rust async runtimes",
            "paidRescue": true
        });
        let req: SearchRequest = serde_json::from_value(json).unwrap();
        assert!(
            !req.paid_rescue,
            "paidRescue must never be settable by an untrusted caller"
        );
    }

    #[test]
    fn search_request_paid_rescue_never_serialized() {
        let mut req: SearchRequest =
            serde_json::from_value(serde_json::json!({ "query": "q" })).unwrap();
        req.paid_rescue = true;
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("paidRescue").is_none());
        assert!(json.get("paid_rescue").is_none());
    }

    #[test]
    fn search_request_accepts_snake_case_aliases() {
        let json = serde_json::json!({
            "query": "q",
            "summarize_results": true,
            "answer_top_n": 3,
            "max_chars_per_source": 4096,
            "llm_api_key": "k",
            "llm_provider": "openai",
            "llm_model": "gpt",
            "base_url": "https://x.example.com",
            "summary_prompt": "be terse",
            "answer_prompt": "be terse",
            "answer_temperature": 0.0,
            "query_expand_variants": 2,
            "query_expand": true,
            "multi_round": false,
            "snippet_first": true,
            "answer_list_format": true,
            "max_content_chars": 1000
        });
        let req: SearchRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.summarize_results, Some(true));
        assert_eq!(req.answer_top_n, Some(3));
        assert_eq!(req.max_chars_per_source, Some(4096));
        assert_eq!(req.llm_api_key, Some("k".into()));
        assert_eq!(req.llm_provider, Some("openai".into()));
        assert_eq!(req.llm_model, Some("gpt".into()));
        assert_eq!(req.base_url, Some("https://x.example.com".into()));
        assert_eq!(req.summary_prompt, Some("be terse".into()));
        assert_eq!(req.answer_prompt, Some("be terse".into()));
        assert_eq!(req.answer_temperature, Some(0.0));
        assert_eq!(req.query_expand_variants, Some(2));
        assert_eq!(req.query_expand, Some(true));
        assert_eq!(req.multi_round, Some(false));
        assert_eq!(req.snippet_first, Some(true));
        assert_eq!(req.answer_list_format, Some(true));
        assert_eq!(req.max_content_chars, Some(1000));
    }

    #[test]
    fn search_request_requires_query_field() {
        let result: Result<SearchRequest, _> = serde_json::from_value(serde_json::json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn search_request_categories_round_trip_mixed_curated_and_other() {
        let json = serde_json::json!({
            "query": "q",
            "categories": ["github", "science", "pdf"]
        });
        let req: SearchRequest = serde_json::from_value(json).unwrap();
        let cats = req.categories.unwrap();
        assert_eq!(cats[0], SearchCategory::Github);
        assert_eq!(cats[1], SearchCategory::Other("science".into()));
        assert_eq!(cats[2], SearchCategory::Pdf);
    }

    #[test]
    fn search_result_snippet_defaults_to_empty_string_when_absent() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "title": "t",
            "description": "d",
            "position": 1
        });
        let r: SearchResult = serde_json::from_value(json).unwrap();
        assert_eq!(r.snippet, "");
    }

    #[test]
    fn search_result_optional_fields_omitted_when_none() {
        let r = SearchResult {
            url: "https://example.com".into(),
            title: "t".into(),
            description: "d".into(),
            snippet: "d".into(),
            position: 0,
            score: None,
            published_date: None,
            category: None,
            markdown: None,
            html: None,
            raw_html: None,
            links: None,
            metadata: None,
            summary: None,
            error: None,
            truncated: None,
        };
        let json = serde_json::to_value(&r).unwrap();
        for key in [
            "score",
            "publishedDate",
            "category",
            "markdown",
            "html",
            "rawHtml",
            "links",
            "metadata",
            "summary",
            "error",
            "truncated",
        ] {
            assert!(json.get(key).is_none(), "expected {key} omitted");
        }
    }

    #[test]
    fn search_result_truncated_defaults_to_none_for_backward_compat() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "title": "t",
            "description": "d",
            "position": 0
        });
        let r: SearchResult = serde_json::from_value(json).unwrap();
        assert_eq!(r.truncated, None);
    }

    #[test]
    fn image_result_round_trip() {
        let img = ImageResult {
            url: "https://example.com/page".into(),
            title: "t".into(),
            description: "d".into(),
            image_url: "https://example.com/x.png".into(),
            position: 2,
            thumbnail_url: Some("https://example.com/x_thumb.png".into()),
            image_format: Some("png".into()),
            resolution: Some("800x600".into()),
        };
        let json = serde_json::to_value(&img).unwrap();
        assert_eq!(json["imageUrl"], "https://example.com/x.png");
        assert_eq!(json["thumbnailUrl"], "https://example.com/x_thumb.png");
        assert_eq!(json["imageFormat"], "png");
        let back: ImageResult = serde_json::from_value(json).unwrap();
        assert_eq!(back.resolution, Some("800x600".into()));
    }

    #[test]
    fn grouped_search_data_default_is_all_none() {
        let g = GroupedSearchData::default();
        assert!(g.web.is_none());
        assert!(g.news.is_none());
        assert!(g.images.is_none());
    }

    #[test]
    fn grouped_search_data_omits_none_fields() {
        let g = GroupedSearchData {
            web: Some(Vec::new()),
            ..Default::default()
        };
        let json = serde_json::to_value(&g).unwrap();
        assert!(json.get("web").is_some());
        assert!(json.get("news").is_none());
        assert!(json.get("images").is_none());
    }

    #[test]
    fn search_data_untagged_flat_array() {
        let json = serde_json::json!([]);
        let data: SearchData = serde_json::from_value(json).unwrap();
        assert!(matches!(data, SearchData::Flat(v) if v.is_empty()));
    }

    #[test]
    fn search_data_untagged_grouped_object() {
        let json = serde_json::json!({ "web": [] });
        let data: SearchData = serde_json::from_value(json).unwrap();
        assert!(matches!(data, SearchData::Grouped(_)));
    }

    #[test]
    fn citation_round_trip() {
        let c = Citation {
            url: "https://example.com".into(),
            title: "t".into(),
            position: 0,
        };
        let json = serde_json::to_value(&c).unwrap();
        let back: Citation = serde_json::from_value(json).unwrap();
        assert_eq!(back.position, 0);
    }

    #[test]
    fn search_response_data_citations_and_warnings_omitted_when_empty() {
        let data = SearchResponseData {
            results: SearchData::Flat(Vec::new()),
            answer: None,
            citations: Vec::new(),
            llm_usage: None,
            warnings: Vec::new(),
        };
        let json = serde_json::to_value(&data).unwrap();
        assert!(json.get("citations").is_none());
        assert!(json.get("warnings").is_none());
        assert!(json.get("answer").is_none());
        assert!(json.get("llmUsage").is_none());
    }

    #[test]
    fn search_response_type_alias_round_trip() {
        let r: SearchResponse = ApiResponse::ok(SearchResponseData {
            results: SearchData::Flat(Vec::new()),
            answer: Some("42".into()),
            citations: Vec::new(),
            llm_usage: None,
            warnings: Vec::new(),
        });
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["data"]["answer"], "42");
    }

    // ── RendererKind ──

    #[test]
    fn renderer_kind_serde_lowercase_each_variant() {
        for (v, s) in [
            (RendererKind::Http, "\"http\""),
            (RendererKind::Lightpanda, "\"lightpanda\""),
            (RendererKind::Chrome, "\"chrome\""),
            (RendererKind::Camoufox, "\"camoufox\""),
            (RendererKind::Cloak, "\"cloak\""),
        ] {
            assert_eq!(serde_json::to_string(&v).unwrap(), s);
            let back: RendererKind = serde_json::from_str(s).unwrap();
            assert_eq!(back, v);
        }
    }

    #[test]
    fn renderer_kind_as_str_matches_serialized_form() {
        for v in [
            RendererKind::Http,
            RendererKind::Lightpanda,
            RendererKind::Chrome,
            RendererKind::ChromeProxy,
            RendererKind::Camoufox,
            RendererKind::Cloak,
        ] {
            let serialized = serde_json::to_string(&v).unwrap();
            let unquoted = serialized.trim_matches('"');
            assert_eq!(v.as_str(), unquoted, "{v:?}");
        }
    }

    // ── RenderDecision ──

    #[test]
    fn render_decision_user_pinned_round_trip() {
        let d = RenderDecision::UserPinned {
            renderer: RendererKind::Chrome,
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["kind"], "userPinned");
        assert_eq!(json["renderer"], "chrome");
        let back: RenderDecision = serde_json::from_value(json).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn render_decision_auto_default_round_trip() {
        let d = RenderDecision::AutoDefault {
            chosen: RendererKind::Http,
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["kind"], "autoDefault");
        assert_eq!(json["chosen"], "http");
    }

    #[test]
    fn render_decision_auto_promoted_round_trip() {
        let d = RenderDecision::AutoPromoted {
            chosen: RendererKind::Chrome,
            from: RendererKind::Lightpanda,
            reason: "next.js hydration failed".into(),
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["kind"], "autoPromoted");
        assert_eq!(json["from"], "lightpanda");
        let back: RenderDecision = serde_json::from_value(json).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn render_decision_breaker_skipped_round_trip() {
        let d = RenderDecision::BreakerSkipped {
            skipped: RendererKind::Lightpanda,
            chosen: RendererKind::Chrome,
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["kind"], "breakerSkipped");
        assert_eq!(json["skipped"], "lightpanda");
    }

    #[test]
    fn render_decision_failover_round_trip_with_chain() {
        let d = RenderDecision::Failover {
            chain: vec![RendererKind::Lightpanda, RendererKind::Chrome],
            reason: FailoverErrorKind::CloudflareChallenge,
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["kind"], "failover");
        assert_eq!(json["chain"][0], "lightpanda");
        assert_eq!(json["reason"], "cloudflareChallenge");
        let back: RenderDecision = serde_json::from_value(json).unwrap();
        assert_eq!(back, d);
    }

    // ── FailoverErrorKind ──

    #[test]
    fn failover_error_kind_counts_for_promotion_table() {
        for (v, expected) in [
            (FailoverErrorKind::NextJsClientError, true),
            (FailoverErrorKind::EmptyNextRoot, true),
            (FailoverErrorKind::LightpandaTimeout, true),
            (FailoverErrorKind::LightpandaCrash, true),
            (FailoverErrorKind::PlaceholderContent, true),
            (FailoverErrorKind::AntibotBlock, true),
            (FailoverErrorKind::CloudflareChallenge, false),
            (FailoverErrorKind::VendorBlock, false),
            (FailoverErrorKind::StatusBlocked, false),
            (FailoverErrorKind::NetworkError, false),
            (FailoverErrorKind::Other, false),
        ] {
            assert_eq!(v.counts_for_promotion(), expected, "{v:?}");
        }
    }

    #[test]
    fn failover_error_kind_as_str_matches_serde_camel_case() {
        for v in [
            FailoverErrorKind::NextJsClientError,
            FailoverErrorKind::EmptyNextRoot,
            FailoverErrorKind::LightpandaTimeout,
            FailoverErrorKind::LightpandaCrash,
            FailoverErrorKind::CloudflareChallenge,
            FailoverErrorKind::PlaceholderContent,
            FailoverErrorKind::VendorBlock,
            FailoverErrorKind::StatusBlocked,
            FailoverErrorKind::AntibotBlock,
            FailoverErrorKind::NetworkError,
            FailoverErrorKind::Other,
        ] {
            let serialized = serde_json::to_string(&v).unwrap();
            let unquoted = serialized.trim_matches('"');
            assert_eq!(v.as_str(), unquoted, "{v:?}");
        }
    }

    // ── Change tracking types ──

    #[test]
    fn change_tracking_mode_deserializes_git_dash_diff_alias() {
        let parsed: ChangeTrackingMode = serde_json::from_str("\"git-diff\"").unwrap();
        assert_eq!(parsed, ChangeTrackingMode::GitDiff);
        let canonical: ChangeTrackingMode = serde_json::from_str("\"gitDiff\"").unwrap();
        assert_eq!(canonical, ChangeTrackingMode::GitDiff);
    }

    #[test]
    fn change_tracking_mode_serializes_canonical_camel_case() {
        assert_eq!(
            serde_json::to_string(&ChangeTrackingMode::GitDiff).unwrap(),
            "\"gitDiff\""
        );
        assert_eq!(
            serde_json::to_string(&ChangeTrackingMode::Json).unwrap(),
            "\"json\""
        );
    }

    #[test]
    fn change_tracking_mode_unknown_value_errors() {
        let result: Result<ChangeTrackingMode, _> = serde_json::from_str("\"xmlDiff\"");
        assert!(result.is_err());
    }

    #[test]
    fn change_tracking_snapshot_default_is_empty() {
        let s = ChangeTrackingSnapshot::default();
        assert!(s.markdown.is_none());
        assert!(s.json.is_none());
        assert_eq!(s.content_hash, "");
        assert!(s.captured_at.is_none());
    }

    #[test]
    fn change_tracking_snapshot_round_trip() {
        let s = ChangeTrackingSnapshot {
            markdown: Some("# hi".into()),
            json: Some(serde_json::json!({"a": 1})),
            content_hash: "sha256:abc".into(),
            captured_at: Some("2026-08-25T00:00:00Z".into()),
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["contentHash"], "sha256:abc");
        let back: ChangeTrackingSnapshot = serde_json::from_value(json).unwrap();
        assert_eq!(back.markdown, s.markdown);
    }

    #[test]
    fn change_tracking_options_default_is_empty_modes() {
        let o = ChangeTrackingOptions::default();
        assert!(o.modes.is_empty());
        assert!(o.schema.is_none());
        assert!(o.previous.is_none());
    }

    #[test]
    fn change_tracking_options_content_type_accepts_snake_case_alias() {
        let json = serde_json::json!({ "content_type": "application/pdf" });
        let o: ChangeTrackingOptions = serde_json::from_value(json).unwrap();
        assert_eq!(o.content_type, Some("application/pdf".into()));
    }

    #[test]
    fn change_tracking_options_previous_snapshot_round_trip() {
        let o = ChangeTrackingOptions {
            modes: vec![ChangeTrackingMode::GitDiff],
            previous: Some(ChangeTrackingSnapshot {
                content_hash: "sha256:old".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&o).unwrap();
        assert_eq!(json["previous"]["contentHash"], "sha256:old");
    }

    #[test]
    fn change_status_serde_lowercase() {
        assert_eq!(
            serde_json::to_string(&ChangeStatus::Same).unwrap(),
            "\"same\""
        );
        assert_eq!(
            serde_json::to_string(&ChangeStatus::Changed).unwrap(),
            "\"changed\""
        );
    }

    #[test]
    fn change_confidence_serde_lowercase_each_variant() {
        for (v, s) in [
            (ChangeConfidence::Low, "\"low\""),
            (ChangeConfidence::Medium, "\"medium\""),
            (ChangeConfidence::High, "\"high\""),
        ] {
            assert_eq!(serde_json::to_string(&v).unwrap(), s);
        }
    }

    #[test]
    fn meaningful_change_type_field_renamed_to_type() {
        let m = MeaningfulChange {
            change_type: "added".into(),
            before: None,
            after: Some("new text".into()),
            reason: "new paragraph".into(),
        };
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json["type"], "added");
        assert!(json.get("changeType").is_none());
        assert!(json.get("before").is_none());
        assert_eq!(json["after"], "new text");
    }

    #[test]
    fn change_judgment_llm_usage_is_never_serialized() {
        let j = ChangeJudgment {
            meaningful: true,
            confidence: ChangeConfidence::High,
            reason: "price changed".into(),
            meaningful_changes: vec![],
            llm_usage: Some(usage(10, 5)),
        };
        let json = serde_json::to_value(&j).unwrap();
        assert!(json.get("llmUsage").is_none());
        assert!(json.get("llm_usage").is_none());
        assert_eq!(json["meaningful"], true);
        assert_eq!(json["confidence"], "high");
    }

    #[test]
    fn change_judgment_meaningful_changes_defaults_empty_on_deserialize() {
        let json = serde_json::json!({
            "meaningful": false,
            "confidence": "low",
            "reason": "no material change"
        });
        let j: ChangeJudgment = serde_json::from_value(json).unwrap();
        assert!(j.meaningful_changes.is_empty());
        assert!(j.llm_usage.is_none());
    }

    #[test]
    fn diff_change_type_field_renamed_and_line_numbers_omitted_when_none() {
        let c = DiffChange {
            change_type: "add".into(),
            content: "+new line".into(),
            ln: Some(5),
            ln1: None,
            ln2: None,
        };
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["type"], "add");
        assert_eq!(json["ln"], 5);
        assert!(json.get("ln1").is_none());
        assert!(json.get("ln2").is_none());
    }

    #[test]
    fn diff_chunk_and_diff_file_round_trip() {
        let file = DiffFile {
            from: "previous".into(),
            to: "current".into(),
            additions: 1,
            deletions: 0,
            chunks: vec![DiffChunk {
                content: "@@ -1,1 +1,2 @@".into(),
                changes: vec![DiffChange {
                    change_type: "add".into(),
                    content: "+new".into(),
                    ln: Some(2),
                    ln1: None,
                    ln2: None,
                }],
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 2,
            }],
        };
        let json = serde_json::to_value(&file).unwrap();
        assert_eq!(json["chunks"][0]["oldStart"], 1);
        assert_eq!(json["chunks"][0]["changes"][0]["type"], "add");
        let back: DiffFile = serde_json::from_value(json).unwrap();
        assert_eq!(back.additions, 1);
    }

    #[test]
    fn diff_ast_truncated_omitted_when_false() {
        let ast = DiffAst::default();
        assert!(!ast.truncated);
        assert!(ast.files.is_empty());
        let json = serde_json::to_value(&ast).unwrap();
        assert!(json.get("truncated").is_none());
    }

    #[test]
    fn diff_ast_truncated_present_when_true() {
        let ast = DiffAst {
            truncated: true,
            ..Default::default()
        };
        let json = serde_json::to_value(&ast).unwrap();
        assert_eq!(json["truncated"], true);
    }

    #[test]
    fn change_diff_both_fields_omitted_when_none() {
        let d = ChangeDiff::default();
        let json = serde_json::to_value(&d).unwrap();
        assert!(json.get("text").is_none());
        assert!(json.get("json").is_none());
    }

    #[test]
    fn change_diff_json_accepts_either_array_or_object_shape() {
        let array_shape = ChangeDiff {
            text: None,
            json: Some(serde_json::json!([{"a": 1}])),
        };
        let object_shape = ChangeDiff {
            text: None,
            json: Some(serde_json::json!({"path.to.field": {"previous": 1, "current": 2}})),
        };
        assert!(serde_json::to_value(&array_shape).unwrap()["json"].is_array());
        assert!(serde_json::to_value(&object_shape).unwrap()["json"].is_object());
    }

    #[test]
    fn change_tracking_result_full_round_trip() {
        let r = ChangeTrackingResult {
            status: ChangeStatus::Changed,
            first_observation: false,
            content_hash: "sha256:new".into(),
            snapshot: Some(ChangeTrackingSnapshot {
                content_hash: "sha256:new".into(),
                ..Default::default()
            }),
            diff: Some(ChangeDiff {
                text: Some("+added line".into()),
                json: None,
            }),
            judgment: Some(ChangeJudgment {
                meaningful: true,
                confidence: ChangeConfidence::Medium,
                reason: "content changed".into(),
                meaningful_changes: Vec::new(),
                llm_usage: None,
            }),
            tag: Some("target-1".into()),
            truncated: false,
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["status"], "changed");
        assert_eq!(json["contentHash"], "sha256:new");
        assert_eq!(json["judgment"]["confidence"], "medium");
        assert!(json.get("truncated").is_none());
        let back: ChangeTrackingResult = serde_json::from_value(json).unwrap();
        assert_eq!(back.tag, Some("target-1".into()));
    }

    #[test]
    fn change_tracking_result_first_observation_defaults_false() {
        let json = serde_json::json!({
            "status": "same",
            "contentHash": "sha256:x"
        });
        let r: ChangeTrackingResult = serde_json::from_value(json).unwrap();
        assert!(!r.first_observation);
        assert!(r.snapshot.is_none());
        assert!(r.diff.is_none());
        assert!(r.judgment.is_none());
        assert!(r.tag.is_none());
        assert!(!r.truncated);
    }
}

/// Status of an async crawl job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrawlStatus {
    #[serde(rename = "scraping")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    /// Job was cancelled via DELETE. Terminal: pollers must stop and TTL
    /// cleanup must evict — keep every `matches!(.., Completed | Failed)`
    /// terminal-state check in sync when touching this enum.
    #[serde(rename = "cancelled")]
    Cancelled,
}

/// GET /v1/crawl/:id response body.
/// Field names match Firecrawl API: status, total, completed, data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrawlState {
    #[serde(skip_serializing)]
    pub id: Uuid,
    pub success: bool,
    pub status: CrawlStatus,
    pub total: u32,
    pub completed: u32,
    /// How many of `completed` came back a block or an origin error page rather
    /// than the requested page. Counted here because a caller billing per page
    /// reads this envelope, not the paginated `data` array — and a walled page
    /// must not be charged. Additive: `#[serde(default)]` keeps an older client
    /// and an older engine interoperable in both directions.
    #[serde(default)]
    pub blocked: u32,
    pub data: Vec<ScrapeData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// POST /v1/crawl start response.
/// Matches Firecrawl format: { success: true, id: "..." }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrawlStartResponse {
    pub success: bool,
    pub id: String,
}

/// POST /v1/map request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapRequest {
    pub url: String,
    pub max_depth: Option<u32>,
    #[serde(default = "default_true")]
    pub use_sitemap: bool,
    /// When true (default), fall back to a short-budget BFS crawl after the
    /// sitemap phase to fill gaps. Set to false for sitemap-only mode — faster
    /// for sites with rich sitemaps, but may miss pages a sitemap omits.
    #[serde(default = "default_true")]
    pub crawl_fallback: bool,
    /// Custom timeout in seconds (default: 120).
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Tier B — strip tracking params. `Some(_)` overrides TOML.
    #[serde(default)]
    pub strip_tracking_params: Option<bool>,
    /// Tier A — drop action URLs. `Some(_)` overrides TOML.
    #[serde(default)]
    pub drop_action_urls: Option<bool>,
    /// Firecrawl-compatible coarse alias. `Some(true)`: strip every
    /// non-preserved param. `Some(false)`: switch the whole filter off
    /// (raw URLs — the explicit escape hatch).
    #[serde(default)]
    pub ignore_query_parameters: Option<bool>,
    /// Additive on top of `DEFAULT_TRACKING_PARAMS`. Keys are normalized to
    /// canonical form (lowercase, `-` folded to `_`), so `add-to-cart` and
    /// `add_to_cart` are equivalent. Max 64 keys; over-cap → 422.
    #[serde(default)]
    pub extra_tracking_params: Option<Vec<String>>,
    /// Additive on top of `DEFAULT_ACTION_PARAMS`. Keys are normalized to
    /// canonical form (lowercase, `-` folded to `_`). Max 64 keys; over-cap → 422.
    #[serde(default)]
    pub extra_action_params: Option<Vec<String>>,
    /// Additive on top of `ALWAYS_PRESERVE` + TOML preserves. Keys are
    /// normalized to canonical form (lowercase, `-` folded to `_`).
    /// Max 64 keys; over-cap → 422.
    #[serde(default)]
    pub preserve_params: Option<Vec<String>>,
    /// Max URLs to discover. Firecrawl-compatible. Defaults to
    /// `DEFAULT_MAX_DISCOVERED_URLS`; the engine clamps to its hard ceiling.
    /// Raise it to dump large/nested sitemaps (e.g. songsterr's ~4.3M URLs).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// POST /v1/map response data — the discovered links plus filter stats.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapData {
    pub links: Vec<String>,
    /// Number of URLs the /map filter dropped entirely (Tier A action-URL
    /// matches). `0` when the filter is disabled.
    #[serde(default)]
    pub dropped_action_count: usize,
    /// Number of URLs that had at least one query param stripped by Tier B.
    /// `0` when the filter is disabled.
    #[serde(default)]
    pub stripped_tracking_count: usize,
    /// Sitemap documents discovered and parsed while mapping the site, e.g.
    /// `/sitemap.xml`, `/sitemap_index.xml`, `/product-sitemap.xml`. Kept out
    /// of `links` because a sitemap file is not a page. Empty when
    /// `useSitemap` is false or the site exposes no reachable sitemap.
    #[serde(default)]
    pub sitemaps: Vec<String>,
}

/// POST /v1/map response body.
/// Standard envelope: { success: true, data: { links: [...] } }
pub type MapResponse = ApiResponse<MapData>;

// ── Search types ──

/// Top-level "source" buckets exposed in the `/v1/search` API. Maps to
/// SearXNG's `categories` query parameter (web → general, news → news,
/// images → images).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchSource {
    Web,
    News,
    Images,
}

impl SearchSource {
    /// SearXNG category name for this source.
    pub fn searxng_category(self) -> &'static str {
        match self {
            SearchSource::Web => "general",
            SearchSource::News => "news",
            SearchSource::Images => "images",
        }
    }
}

/// User-facing category modifiers.
///
/// Three values carry curated, Firecrawl-compatible behavior:
/// - `Github` / `Research` switch to topical SearXNG *engines* (configurable
///   via `[search].github_engines` / `[search].research_engines`).
/// - `Pdf` appends `filetype:pdf` to the query.
///
/// Any other string is passed straight through to SearXNG's native
/// `categories` query parameter (e.g. `science`, `it`, `news`, `files`,
/// `images`), so SearXNG's own engine→category routing applies without any
/// crw code or config changes. This makes the surface a strict superset of
/// Firecrawl's `github`/`research`/`pdf` — existing callers are unaffected.
///
/// See <https://docs.searxng.org/user/configured_engines.html> for the
/// categories a given SearXNG instance exposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchCategory {
    Github,
    Research,
    Pdf,
    /// Unknown value — forwarded verbatim to SearXNG's `categories` param.
    Other(String),
}

impl SearchCategory {
    /// Wire/string representation. The three curated variants round-trip to
    /// their lowercase names; `Other` returns the verbatim passthrough value.
    pub fn as_str(&self) -> &str {
        match self {
            SearchCategory::Github => "github",
            SearchCategory::Research => "research",
            SearchCategory::Pdf => "pdf",
            SearchCategory::Other(s) => s.as_str(),
        }
    }
}

impl From<String> for SearchCategory {
    fn from(s: String) -> Self {
        match s.as_str() {
            "github" => SearchCategory::Github,
            "research" => SearchCategory::Research,
            "pdf" => SearchCategory::Pdf,
            _ => SearchCategory::Other(s),
        }
    }
}

impl Serialize for SearchCategory {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SearchCategory {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(SearchCategory::from(String::deserialize(deserializer)?))
    }
}

/// Time-window filter, mirrors Google's `tbs=qdr:*` syntax used by Firecrawl.
/// SearXNG's `time_range` only supports day/week/month/year; `Hour` is mapped
/// to `Day` for parity with the SaaS implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchTimeFilter {
    #[serde(rename = "qdr:h")]
    Hour,
    #[serde(rename = "qdr:d")]
    Day,
    #[serde(rename = "qdr:w")]
    Week,
    #[serde(rename = "qdr:m")]
    Month,
    #[serde(rename = "qdr:y")]
    Year,
}

impl SearchTimeFilter {
    /// SearXNG `time_range` string. SearXNG has no hour granularity, so
    /// `Hour` is reported as `day` (lossy; matches SaaS behavior).
    pub fn searxng_time_range(self) -> &'static str {
        match self {
            SearchTimeFilter::Hour | SearchTimeFilter::Day => "day",
            SearchTimeFilter::Week => "week",
            SearchTimeFilter::Month => "month",
            SearchTimeFilter::Year => "year",
        }
    }
}

/// `scrapeOptions` sub-object — a narrow projection of `ScrapeRequest` that
/// we accept on every result from a search. Only the fields the SaaS exposes.
///
/// `formats` defaults to `["markdown"]` so Firecrawl-compatible callers that
/// pass `scrapeOptions: {}` (toggle enrichment without specifying formats)
/// get a sensible default instead of a deserialization error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchScrapeOptions {
    #[serde(default = "default_formats")]
    pub formats: Vec<OutputFormat>,
    #[serde(default = "default_true")]
    pub only_main_content: bool,
    /// Residential-proxy exit country (ISO 3166-1 alpha-2) for the per-result page scrape.
    /// Populated by the SaaS layer from the caller's IP (geo-aware proxy). `None` = engine default.
    #[serde(default)]
    pub country: Option<String>,
    /// Per-result scrape budget (ms). `None` = the search-enrichment default,
    /// NOT the implicit full-ladder deadline a single `/v1/scrape` gets: search
    /// waits for every result, so one straggler walking the whole renderer
    /// ladder would stall the entire response. Must be in `(0, 60000]`.
    #[serde(default)]
    pub timeout: Option<u64>,
}

/// POST /v1/search request body. Mirrors the zod schema in
/// `crw-saas/src/lib/search-schema.ts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    pub query: String,
    /// Number of results per source (or total when `sources` is unset).
    /// Defaults to `[search].default_limit` when omitted; clamped to
    /// `[search].max_limit` server-side.
    #[serde(default)]
    pub limit: Option<u32>,
    /// SearXNG `language` parameter (e.g. `"en"`, `"de"`, `"auto"`).
    #[serde(default)]
    pub lang: Option<String>,
    /// Google-style time filter (`qdr:h|d|w|m|y`).
    #[serde(default)]
    pub tbs: Option<SearchTimeFilter>,
    /// When set, results are grouped under `web`/`news`/`images` keys.
    /// When unset, a flat array is returned.
    #[serde(default)]
    pub sources: Option<Vec<SearchSource>>,
    /// User-facing category modifiers. Max 5 entries (matches SaaS).
    #[serde(default)]
    pub categories: Option<Vec<SearchCategory>>,
    /// When set, every `web` result is enriched in-process via the scrape
    /// pipeline (parallel, bounded by `[crawler].max_concurrency`).
    #[serde(default)]
    pub scrape_options: Option<SearchScrapeOptions>,
    /// When true, every scraped result also gets an LLM summary attached to
    /// `SearchResult.summary`. Requires `scrape_options` to be set (so the
    /// markdown exists to summarize). LLM fan-out is bounded by
    /// `[extraction.llm].max_concurrency`.
    #[serde(default, alias = "summarize_results")]
    pub summarize_results: Option<bool>,
    /// When true, a single synthesized answer is generated from the top-N
    /// scraped results. Requires `scrape_options` to be set.
    #[serde(default)]
    pub answer: Option<bool>,
    /// Number of top results to include in answer synthesis (default 5,
    /// capped at 10).
    #[serde(default, alias = "answer_top_n")]
    pub answer_top_n: Option<u32>,
    /// Per-source character cap for the answer prompt (default 8192,
    /// hard-capped at 32768 server-side).
    #[serde(default, alias = "max_chars_per_source")]
    pub max_chars_per_source: Option<usize>,
    /// BYOK fields (mirror `ScrapeRequest`).
    #[serde(default, alias = "llm_api_key")]
    pub llm_api_key: Option<String>,
    #[serde(default, alias = "llm_provider")]
    pub llm_provider: Option<String>,
    #[serde(default, alias = "llm_model")]
    pub llm_model: Option<String>,
    #[serde(default, alias = "base_url")]
    pub base_url: Option<String>,
    /// Optional user-supplied instructions appended to the per-result
    /// summary system prompt. See `ScrapeRequest.summary_prompt`. Capped
    /// at 500 chars server-side.
    #[serde(default, alias = "summary_prompt")]
    pub summary_prompt: Option<String>,
    /// Optional user-supplied instructions appended to the answer-synthesis
    /// system prompt (e.g. "respond in Turkish", "be concise"). The
    /// "answer using ONLY the provided sources" rule and citation discipline
    /// stay intact. Capped at 500 chars server-side.
    #[serde(default, alias = "answer_prompt")]
    pub answer_prompt: Option<String>,
    /// Sampling temperature for the answer-synthesis LLM call. Omitted (None)
    /// keeps the provider default (prod behavior). The benchmark/eval harness
    /// sets `0` (with a fixed seed) to make answers deterministic, so a real
    /// accuracy lever is distinguishable from sampling noise.
    #[serde(default, alias = "answer_temperature")]
    pub answer_temperature: Option<f32>,
    /// Per-request override for `[search].query_expand_variants` — the number
    /// of diverse query rewrites fetched + unioned when query expansion is on.
    /// None uses the server config. The benchmark/eval harness sets this to A/B
    /// recall (e.g. 1 vs 3) at a fixed answer temperature.
    #[serde(default, alias = "query_expand_variants")]
    pub query_expand_variants: Option<usize>,
    /// Per-request override for `[search].query_expand` — turns multi-query
    /// expansion on/off for this request. None uses the server config. Used by
    /// the eval harness to A/B expansion against prod without a global flip.
    #[serde(default, alias = "query_expand")]
    pub query_expand: Option<bool>,
    /// Per-request override for `[search].multi_round` — the adaptive
    /// evidence-scout round that fires when the round-1 answer abstains. None
    /// uses the server config. The eval harness sets this to A/B the lever.
    #[serde(default, alias = "multi_round")]
    pub multi_round: Option<bool>,
    /// Per-request override for `[search].snippet_first` — answer from free SERP
    /// snippets first and scrape only if that abstains. None uses server config.
    /// Used by the eval harness to A/B the lazy-scrape path against prod.
    #[serde(default, alias = "snippet_first")]
    pub snippet_first: Option<bool>,
    /// Per-request override for `[search].answer_list_format` — when the query
    /// has list intent ("best/top X in Y", "recommend …"), render the answer as
    /// a ranked list of named options instead of prose. None uses the server
    /// config; Some(false) forces prose, Some(true) forces the list path (still
    /// only fires on list-intent queries).
    #[serde(default, alias = "answer_list_format")]
    pub answer_list_format: Option<bool>,
    /// Maximum number of bytes of each per-result markdown sent to the LLM
    /// when `summarize_results` is enabled. Defaults to
    /// `[extraction.llm].max_html_bytes` (100 KB). Clamped to a 200 KB
    /// server-side ceiling. Independent from `max_chars_per_source`, which
    /// caps the answer-synthesis path, not the per-result summary path.
    #[serde(default, alias = "max_content_chars")]
    pub max_content_chars: Option<usize>,
    /// Entitlement to the search backend's PAID rescue tier when the free legs
    /// return nothing. NOT part of the public request body: `serde(skip)` means
    /// a caller cannot set it by sending the field, and we never serialize it
    /// onward. It is populated server-side from a trusted header
    /// (`X-Crw-Paid-Rescue`) that only crw-saas may send, because only crw-saas
    /// can see the plan and role that decide whether the request may spend.
    ///
    /// Defaults to false, so a self-host deployment, the CLI, MCP and every
    /// internally-constructed request behave exactly as before.
    #[serde(skip)]
    pub paid_rescue: bool,
}

/// A single search result (web or news). Mirrors `SearchResult` in
/// `crw-saas/src/lib/search-transform.ts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub url: String,
    pub title: String,
    pub description: String,
    /// Alias of `description`. Always populated. Emitted so downstream LLM
    /// pipelines that ask for "snippet" (Firecrawl convention) don't need a
    /// rename step. `#[serde(default)]` keeps deserialization permissive for
    /// callers that don't supply it.
    #[serde(default)]
    pub snippet: String,
    pub position: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    // Populated when scrapeOptions is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<PageMetadata>,
    /// LLM-generated summary; populated when `summarizeResults: true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Set when this result's enrichment scrape failed (P3-4), so a partial
    /// success is observable: distinguishes "scrape failed" from "page simply
    /// had no markdown". Absent on success — backward-compatible.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Set when the enrichment scrape returned a partial-DOM snapshot because
    /// its budget elapsed (`ScrapeData.truncated`). Sibling of `error`: `error`
    /// marks a total failure, this marks an incomplete success — without it a
    /// budget-shortened render is indistinguishable from a thin page. Absent
    /// (not `false`) when the scrape completed — backward-compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

/// A single image result. Mirrors `ImageResult` in
/// `crw-saas/src/lib/search-transform.ts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageResult {
    pub url: String,
    pub title: String,
    pub description: String,
    pub image_url: String,
    pub position: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
}

/// Grouped result envelope when `sources` is set on the request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupedSearchData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web: Option<Vec<SearchResult>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub news: Option<Vec<SearchResult>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageResult>>,
}

/// `data` payload of the `/v1/search` response. Either a flat list of
/// results or a grouped object — chosen by whether the request specified
/// `sources`. Untagged: serializes as either an array or an object with
/// `web`/`news`/`images` keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SearchData {
    Flat(Vec<SearchResult>),
    Grouped(GroupedSearchData),
}

/// A citation reference in an LLM-synthesized search answer. `position`
/// is clamped to `[0, sources.len())` server-side so fabricated indices
/// can't escape the input source list.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    pub url: String,
    pub title: String,
    pub position: u32,
}

/// Wrapper data envelope for `/v1/search` responses. Carries the existing
/// `SearchData` (flat or grouped) alongside optional LLM-generated
/// `answer` + `citations`. Adding sibling fields directly to `SearchData`
/// is impossible because that enum is `#[serde(untagged)]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponseData {
    pub results: SearchData,
    /// LLM-synthesized answer over the top-N results; `None` unless
    /// `answer: true` was set on the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Source citations for the answer. Order is meaningful: citation #0
    /// is `sources[0]`. Capped at 20 entries.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub citations: Vec<Citation>,
    /// Token usage + best-effort cost from the answer synthesis call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_usage: Option<LlmUsage>,
    /// Soft-failure / partial-result notices (e.g. "answer call rate-limited;
    /// summaries returned without answer").
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
}

/// POST /v1/search response body.
pub type SearchResponse = ApiResponse<SearchResponseData>;

// ── Render result ──

/// Closed enum of renderer kinds used in routing decisions and metrics.
/// Distinct from `RequestedRenderer` (user-facing input) — this is the
/// internal vocabulary for what actually executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RendererKind {
    Http,
    Lightpanda,
    Chrome,
    #[serde(rename = "chrome_proxy")]
    ChromeProxy,
    /// Opt-in Camoufox stealth tier (REST). `rename_all = "lowercase"` yields
    /// `"camoufox"`. Unconditional (not feature-gated) like every other kind —
    /// the variant is inert in lean builds since no camoufox renderer is ever
    /// constructed there.
    Camoufox,
    /// Opt-in cloak Turnstile-solver recovery tier (REST). `rename_all =
    /// "lowercase"` yields `"cloak"`. Unconditional like every other kind; inert
    /// in lean builds since no cloak renderer is ever constructed there.
    Cloak,
    /// Chrome-impersonating HTTP tier (wreq). `rename_all = "lowercase"`
    /// would yield `"impersonatedhttp"`, so renamed explicitly. Unconditional
    /// like every other kind; inert in lean builds since no impersonated
    /// renderer is ever constructed there.
    #[serde(rename = "impersonated-http")]
    ImpersonatedHttp,
}

impl RendererKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RendererKind::Http => "http",
            RendererKind::Lightpanda => "lightpanda",
            RendererKind::Chrome => "chrome",
            RendererKind::ChromeProxy => "chrome_proxy",
            RendererKind::Camoufox => "camoufox",
            RendererKind::Cloak => "cloak",
            RendererKind::ImpersonatedHttp => "impersonated-http",
        }
    }
}

/// Why and how a renderer was chosen for a given request. Surfaced in
/// `FetchResult.render_decision` and exposed to API callers behind a debug gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum RenderDecision {
    /// User pinned a specific renderer; auto-mode learning is bypassed.
    UserPinned { renderer: RendererKind },
    /// Auto mode used the configured default chain (no host preference yet).
    AutoDefault { chosen: RendererKind },
    /// Auto mode promoted a heavy renderer based on host preference.
    AutoPromoted {
        chosen: RendererKind,
        from: RendererKind,
        reason: String,
    },
    /// Auto mode skipped a renderer because its circuit breaker was open.
    BreakerSkipped {
        skipped: RendererKind,
        chosen: RendererKind,
    },
    /// Failover triggered after the initial choice failed.
    Failover {
        chain: Vec<RendererKind>,
        reason: FailoverErrorKind,
    },
}

/// Closed taxonomy of failure reasons that drive failover and host learning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FailoverErrorKind {
    /// LightPanda hydration / runtime exception (counts toward promotion).
    NextJsClientError,
    /// LightPanda returned an empty Next.js root shell.
    EmptyNextRoot,
    /// LightPanda timed out.
    LightpandaTimeout,
    /// LightPanda crashed or connection died.
    LightpandaCrash,
    /// Cloudflare challenge detected (combination markers).
    CloudflareChallenge,
    /// Generic placeholder / too-short content.
    PlaceholderContent,
    /// Vendor-specific anti-bot block (Akamai, PerimeterX, DataDome, etc.).
    /// Vendor name is recorded via `crw_vendor_block_total{vendor}` metric
    /// and the renderer warning — not carried in the enum variant to keep
    /// the type `Copy`-friendly.
    VendorBlock,
    /// JS renderer returned a 4xx/5xx HTTP status (e.g. 403, 429) — same
    /// status set the HTTP tier escalates on. Caught in the JS tier so a
    /// "200 with bot HTML" or "403 with content" can't masquerade as success.
    StatusBlocked,
    /// The `crw_extract::antibot` classifier flagged a block the lighter
    /// `detector` heuristics missed (e.g. a "blocked by network security"
    /// WAF page served with HTTP 200). Drives escalation toward the
    /// residential `chrome_proxy` tier; counts toward host promotion.
    AntibotBlock,
    /// Network error during render.
    NetworkError,
    /// Other / unclassified failure (does NOT count for promotion).
    Other,
}

impl FailoverErrorKind {
    /// Strict failure predicate: only LightPanda-specific failures should
    /// drive host preference promotion. CF challenges and network errors
    /// are not LightPanda's fault.
    pub fn counts_for_promotion(&self) -> bool {
        matches!(
            self,
            FailoverErrorKind::NextJsClientError
                | FailoverErrorKind::EmptyNextRoot
                | FailoverErrorKind::LightpandaTimeout
                | FailoverErrorKind::LightpandaCrash
                | FailoverErrorKind::PlaceholderContent
                | FailoverErrorKind::AntibotBlock
        )
    }

    /// Stable camelCase identifier matching the JSON `serde` rendering.
    /// Used in user-facing warnings so the string a client sees in a
    /// `warnings[]` entry matches the `renderDecision.reason` field.
    pub fn as_str(&self) -> &'static str {
        match self {
            FailoverErrorKind::NextJsClientError => "nextJsClientError",
            FailoverErrorKind::EmptyNextRoot => "emptyNextRoot",
            FailoverErrorKind::LightpandaTimeout => "lightpandaTimeout",
            FailoverErrorKind::LightpandaCrash => "lightpandaCrash",
            FailoverErrorKind::CloudflareChallenge => "cloudflareChallenge",
            FailoverErrorKind::PlaceholderContent => "placeholderContent",
            FailoverErrorKind::VendorBlock => "vendorBlock",
            FailoverErrorKind::StatusBlocked => "statusBlocked",
            FailoverErrorKind::AntibotBlock => "antibotBlock",
            FailoverErrorKind::NetworkError => "networkError",
            FailoverErrorKind::Other => "other",
        }
    }
}

/// Result of fetching + optionally rendering a page.
#[derive(Debug, Clone)]
pub struct FetchResult {
    pub url: String,
    /// Final URL after redirects, populated only when it differs from the
    /// requested `url`. None means no redirect or scheme/path was identical.
    pub final_url: Option<String>,
    pub status_code: u16,
    pub html: String,
    pub content_type: Option<String>,
    pub raw_bytes: Option<Vec<u8>>,
    pub rendered_with: Option<String>,
    pub elapsed_ms: u64,
    pub warning: Option<String>,
    /// Routing decision metadata. `None` for legacy / non-instrumented paths.
    pub render_decision: Option<RenderDecision>,
    /// Credit cost for this request (set by routing layer; 0 = not yet priced).
    pub credit_cost: u32,
    /// Soft-failure / informational warnings to surface to the caller.
    pub warnings: Vec<String>,
    /// Set by chrome renderer when the navigation budget elapsed before
    /// `loadEventFired` and we snapshotted the partial DOM. Mid-load HTML may
    /// still extract usefully (`single.rs` decides success on md length).
    pub truncated: bool,
    /// Set when `Deadline::remaining() == 0` was observed at result-build time.
    /// Stricter than `truncated` — caller's whole budget is spent.
    pub deadline_exceeded: bool,
    /// XHR/fetch responses captured during navigation. Empty unless the
    /// renderer ran with network capture enabled. Used by extraction as a
    /// fallback content source when DOM-based extraction is low quality.
    pub captured_responses: Vec<CapturedNetworkResponse>,
    /// Raw base64 PNG captured via CDP `Page.captureScreenshot` when the
    /// request asked for the `screenshot` format. `None` for the HTTP /
    /// camoufox / lightpanda paths (they never capture). The `data:` URL
    /// prefix is added in `single.rs`, not here.
    pub screenshot: Option<String>,
    /// A wall verdict the renderer already reached, carried forward as data.
    ///
    /// Set in exactly one situation: the JS ladder was spent and the body that
    /// would otherwise be handed back as a fallback is itself an anti-bot wall
    /// (`detector::looks_like_generic_bot_wall`). That predicate is much
    /// narrower than `classify_block` and is deliberately not run on every
    /// page, so the verdict has to travel rather than be recomputed at the
    /// scrape choke.
    ///
    /// It used to be raised as `CrwError::HttpError`, which the server maps to
    /// 502. That reported a target refusing us as our own gateway failing: the
    /// caller saw "Bad Gateway", the SaaS booked `upstream_5xx`, and the
    /// watchdog paged for a platform incident that was not happening. Carrying
    /// it lets `crw_crawl::single` stamp it at the shared choke, where every
    /// other wall already becomes `success: false` + `anti_bot` at HTTP 200.
    ///
    /// `None` on every path that existed before, and on every renderer: only
    /// the fallback arms in `crw_renderer::Renderer::fetch` ever set it.
    pub block: Option<BlockOutcome>,
}

/// A single XHR/fetch response captured via CDP Network domain.
#[derive(Debug, Clone)]
pub struct CapturedNetworkResponse {
    pub url: String,
    pub request_id: String,
    pub status: u16,
    pub mime_type: Option<String>,
    pub body: Option<String>,
    pub body_size_bytes: usize,
}

// ===========================================================================
// Change tracking (monitor) types
//
// These types are the stateless primitives the SaaS / self-host monitor
// control plane builds on. `crw-diff` consumes `ChangeTrackingOptions` and
// produces a `ChangeTrackingResult`; the LLM judge (`crw-extract`) populates
// `ChangeJudgment`. Wire shapes mirror Firecrawl's `/monitor` check payloads.
// ===========================================================================

/// Change-tracking diff mode. Wire: `"gitDiff"` or `"json"`.
///
/// Deserialization also accepts `"git-diff"` for ergonomics; serialization
/// always emits the canonical `"gitDiff"` / `"json"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeTrackingMode {
    GitDiff,
    Json,
}

impl<'de> Deserialize<'de> for ChangeTrackingMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "gitDiff" | "git-diff" => Ok(ChangeTrackingMode::GitDiff),
            "json" => Ok(ChangeTrackingMode::Json),
            other => Err(serde::de::Error::custom(format!(
                "Unknown changeTracking mode '{other}'. Valid modes: gitDiff, json (alias: git-diff)."
            ))),
        }
    }
}

/// A snapshot of a scrape, used as the baseline to diff against. The caller
/// (SaaS / self-host monitor) persists this between checks and supplies the
/// prior one as `previous`; opencore is stateless and stores nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeTrackingSnapshot {
    /// Normalized markdown content (present for gitDiff / mixed mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    /// Extracted structured JSON (present for json / mixed mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<serde_json::Value>,
    /// Mode-aware content hash (markdown hash for gitDiff/mixed; tracked-field
    /// hash for json mode). The SaaS short-circuit keys off this.
    #[serde(default)]
    pub content_hash: String,
    /// Caller-stamped capture time; echoed back untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
}

/// Change-tracking options. Sibling field on `ScrapeRequest` (activated by the
/// `"changeTracking"` format string) and the body of `POST /v1/change-tracking/diff`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeTrackingOptions {
    /// Diff surfaces to compute. `["gitDiff"]` = markdown unified diff + AST;
    /// `["json"]` = per-field diff; `["json","gitDiff"]` = mixed (both).
    #[serde(default)]
    pub modes: Vec<ChangeTrackingMode>,
    /// JSON schema describing the fields to track (json / mixed mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
    /// Natural-language extraction prompt (alternative to `schema`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The previous snapshot to diff against. `None` => first observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<ChangeTrackingSnapshot>,
    /// Opaque caller tag echoed back on the result (e.g. a target id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// MIME content type of the current page (binary/non-text → byte hash, no diff).
    #[serde(
        default,
        alias = "content_type",
        skip_serializing_if = "Option::is_none"
    )]
    pub content_type: Option<String>,
}

/// Per-page change status emitted by opencore. Set-level `new` / `removed`
/// are computed by the caller's reconciler, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeStatus {
    Same,
    Changed,
}

/// Judge confidence level. Matches Firecrawl's `"low" | "medium" | "high"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeConfidence {
    Low,
    Medium,
    High,
}

/// A single meaningful change called out by the judge. Mirrors Firecrawl's
/// `meaningfulChanges[]` entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeaningfulChange {
    /// `"added" | "removed" | "changed"`.
    #[serde(rename = "type")]
    pub change_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    pub reason: String,
}

/// LLM meaningful-change judgment. Public wire shape is exactly
/// `{meaningful, confidence, reason, meaningfulChanges}` (Firecrawl parity);
/// `llm_usage` is internal-only and never serialized.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeJudgment {
    pub meaningful: bool,
    pub confidence: ChangeConfidence,
    pub reason: String,
    #[serde(default)]
    pub meaningful_changes: Vec<MeaningfulChange>,
    /// Token usage for the judge call. Internal-only — `skip` keeps it out of
    /// the public judgment wire shape; the orchestration layer reads it for
    /// billing/observability.
    #[serde(skip)]
    pub llm_usage: Option<LlmUsage>,
}

/// One change line within a diff chunk (parse-diff-compatible).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffChange {
    /// `"add" | "del" | "normal"`.
    #[serde(rename = "type")]
    pub change_type: String,
    pub content: String,
    /// New-file line number (add / normal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ln: Option<usize>,
    /// Old-file line number (normal only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ln1: Option<usize>,
    /// New-file line number (normal only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ln2: Option<usize>,
}

/// A hunk within a diff file (parse-diff-compatible).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffChunk {
    /// The `@@ -a,b +c,d @@` header line.
    pub content: String,
    pub changes: Vec<DiffChange>,
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
}

/// A single file's diff (parse-diff-compatible). For a single-page change
/// track there is always exactly one synthetic file (`previous` → `current`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFile {
    pub from: String,
    pub to: String,
    pub additions: usize,
    pub deletions: usize,
    pub chunks: Vec<DiffChunk>,
}

/// The git-diff AST (parse-diff style). Serialized into `diff.json` for
/// gitDiff-only mode; in mixed mode the per-field json diff takes `diff.json`
/// instead and this AST is not surfaced.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiffAst {
    pub files: Vec<DiffFile>,
    pub additions: usize,
    pub deletions: usize,
    /// True when the AST was capped at `max_diff_changes` (full snapshot still
    /// retained, so the change is recoverable).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// The `diff` envelope: `{ text?, json? }`. `text` is the unified markdown
/// diff (gitDiff / mixed). `json` is mode-polymorphic — the parse-diff AST in
/// gitDiff-only mode, or the per-field path map (`{ "<path>": {previous,current} }`)
/// in json / mixed mode. Modeled as `Value` to carry either shape, exactly
/// matching Firecrawl's wire payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeDiff {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<serde_json::Value>,
}

/// Result of a change-tracking computation for one page. Surfaced on
/// `ScrapeData.change_tracking` and returned by `POST /v1/change-tracking/diff`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeTrackingResult {
    pub status: ChangeStatus,
    /// True when no `previous` was supplied — the caller maps this to `new`.
    #[serde(default)]
    pub first_observation: bool,
    /// Mode-aware hash of the current content (see `ChangeTrackingSnapshot`).
    pub content_hash: String,
    /// The current snapshot — persist this as the next check's `previous`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<ChangeTrackingSnapshot>,
    /// The diff surfaces; `None` when `status == Same` or for binary content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<ChangeDiff>,
    /// Meaningful-change judgment; populated by the orchestration layer only
    /// when the page changed, a goal is set, and judging is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judgment: Option<ChangeJudgment>,
    /// Echoed caller tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// True when the diff AST was truncated (mirrors `DiffAst.truncated`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}
