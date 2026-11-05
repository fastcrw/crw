//! `GET /v1/capabilities` — surface what this opencore instance supports.
//!
//! SaaS / dashboard frontends call this on boot to decide which provider
//! buttons / formats to surface. Closes the "SaaS UI shipped before
//! opencore rollout" silent-failure mode by giving callers a way to ask
//! "do you actually do this?" before making a real request.
//!
//! CONTRACT — every boolean here is DERIVED from the real build (cargo
//! features) and the effective config. A capability is `true` only when this
//! instance can perform the operation for a well-formed request that supplies
//! NO extra credentials. Nothing is hardcoded to `true`.
//!
//! Credentials are reported separately rather than folded into the booleans:
//!
//! * `llm.serverKeyConfigured` — a server-side LLM key is present.
//! * `formats.llmRequired` — the formats that need an LLM: a server key, or a
//!   per-request `llmApiKey` (BYOK is always accepted and cannot be disabled).
//!
//! So a BYOK-only deploy reports `search.answer: false` (it cannot answer
//! without a caller-supplied key) alongside `search.supported: true` — a caller
//! that sends `llmApiKey` still gets answers.

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::state::AppState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub version: &'static str,
    pub llm: LlmCapabilities,
    pub formats: FormatCapabilities,
    pub search: SearchCapabilities,
    pub screenshot: ScreenshotCapabilities,
    pub renderers: RendererCapabilities,
    pub extract: ExtractCapabilities,
    pub documents: DocumentCapabilities,
    pub limits: Limits,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractCapabilities {
    /// `POST /v1/extract` (async multi-URL structured extraction) works for a
    /// request that carries NO credentials of its own. The route is ALWAYS
    /// mounted, but it needs an LLM: it rejects a keyless request when no
    /// server LLM key is configured, and it rejects one regardless of the
    /// server key when `llm.requireByokHeader` is set. Same gate as
    /// `search.answer`, plus the header guard.
    ///
    /// `false` does not mean "not implemented" — a request carrying `llmApiKey`
    /// still extracts. It means "not usable without your own key".
    pub supported: bool,
    /// Max URLs accepted per request (`crawler.max_extract_urls`).
    pub max_urls: usize,
    /// Per-field `basis` attribution: `basis: true` on `/v1/extract` and on a
    /// `formats:["json"]` scrape returns an evidence record per top-level scalar
    /// schema property. Reports the truth of the running build.
    pub per_field_attribution: bool,
    /// The engine's effective per-leg output-token cap for structured
    /// extraction (`extraction.llm.max_tokens`). A budget estimator pins its
    /// worst-case leg cost to this exact number, so it is reported here rather
    /// than assumed — an ops change to `max_tokens` moves this value with it.
    pub max_output_tokens: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentCapabilities {
    /// Document parser types this instance can apply. `["pdf"]` when PDF
    /// support is compiled in and enabled; empty otherwise. The SaaS gates the
    /// `parsers` option and the upload UI on this.
    pub parsers: Vec<&'static str>,
    /// File-upload availability + limits.
    pub file_upload: FileUploadCapabilities,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUploadCapabilities {
    pub supported: bool,
    /// Upload path, as a value a client can join onto its API base. `/v2/parse`
    /// is the one path served by EVERY surface: the engine mounts it at the
    /// root and under `/firecrawl`, and the hosted API proxies `/v1/*` and
    /// `/v2/*` only — a `/firecrawl/...` value would 404 there.
    pub endpoint: &'static str,
    /// The ENFORCED body cap — the same value the `/v2/parse` body-limit layer
    /// applies (`document.max_upload_bytes`, clamped by the hard ceiling).
    pub max_bytes: usize,
    pub types: Vec<&'static str>,
    /// pdf-inspector has no OCR — scanned/image PDFs yield empty/partial text.
    pub ocr: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmCapabilities {
    /// Provider tags the server's dispatch accepts. Sourced from
    /// `crw_extract::llm::SUPPORTED_PROVIDERS`, the same list the dispatcher
    /// validates against, so the advertisement cannot drift from reality.
    pub providers: Vec<&'static str>,
    /// The dispatcher accepts a custom `baseUrl`, but ONLY as part of a BYOK
    /// request: `baseUrl` is read while building the per-request LLM config,
    /// which is only built when the request also carries `llmApiKey`. Sending
    /// `baseUrl` without a key leaves the server's own endpoint in force. The
    /// server would otherwise be pointing its OWN key at a caller-chosen host,
    /// so this is deliberate.
    ///
    /// `/v1/extract` rejects `baseUrl` outright; configure
    /// `[extraction.llm.base_url]` server-side for that route.
    ///
    /// Build-invariant: the dispatcher always compiles this in, so unlike the
    /// other fields here there is no config that turns it off.
    pub supports_base_url: bool,
    /// True when a server-wide LLM key is configured (self-hosted /
    /// no-SaaS deploys). SaaS-fronted deploys set
    /// `CRW_DISABLE_SERVER_LLM_KEY=1` and rely on per-request BYOK.
    pub server_key_configured: bool,
    /// Configured server-side fan-out cap for LLM calls. 0 when no
    /// server-side LLM config is present.
    pub max_concurrency: usize,
    /// Header name the server will look for on LLM-touching requests
    /// (`None` means no header guard).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_byok_header: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatCapabilities {
    /// Formats this build/config can actually produce. `screenshot` appears
    /// only when a screenshot-capable renderer is compiled AND configured.
    pub supported: Vec<&'static str>,
    /// Formats that additionally need an LLM — a server key
    /// (`llm.serverKeyConfigured`) or a per-request `llmApiKey`. Without one,
    /// requesting them is a hard error, never a silent downgrade.
    ///
    /// When `llm.requireByokHeader` is set, the server key alone is NOT enough:
    /// the scrape and extract paths reject these formats unless the request
    /// carries `llmApiKey`, even on a deploy that has a server key.
    pub llm_required: Vec<&'static str>,
    /// Change-tracking diff modes this instance supports. The SaaS
    /// capability-gate checks `supported` contains `"changeTracking"` before
    /// emitting monitor scrapes.
    pub change_tracking_modes: Vec<&'static str>,
    /// Change-tracking modes that need an LLM, on the same terms as
    /// `llmRequired`. `gitDiff` is deterministic and needs none; `json` mode is
    /// a hard error without a server key or a per-request `llmApiKey`.
    pub change_tracking_modes_llm_required: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchCapabilities {
    /// `/v1/search` is usable: `search.enabled` AND `search.search_backend_url` are
    /// set. Configured, not health-probed — a configured-but-unreachable
    /// backend still reports `true`.
    pub supported: bool,
    /// Answer synthesis works WITHOUT a caller-supplied LLM key (search
    /// configured AND a server LLM key present). When this is `false` but
    /// `supported` is `true`, a request carrying `llmApiKey` still gets an
    /// answer.
    pub answer: bool,
    /// Same gate as `answer`.
    pub summarize_results: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotCapabilities {
    /// A screenshot-capable renderer (chrome / chrome_proxy / playwright) is
    /// compiled in AND configured. LightPanda and Camoufox cannot capture, so
    /// an instance that only has those reports `false` — and the scrape path
    /// fails closed on a screenshot request rather than returning an empty one.
    pub supported: bool,
    /// Full-page capture (`screenshot@fullPage` / `screenshotFullPage`). Same
    /// gate as `supported`: the CDP capture path serves both.
    pub full_page: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RendererCapabilities {
    /// JS renderer tiers this instance actually constructed, in fallback order.
    /// Reflects BOTH the build features (a CDP-less build constructs none) and
    /// the config (a tier whose `ws_url` / `base_url` is unset is never built).
    /// A `renderer` pin naming a tier outside this list is rejected; the pin
    /// also accepts `"auto"`, which is not a tier and so is not listed here.
    ///
    /// "Constructible / pinnable", not "always in the auto ladder": `camoufox`
    /// is built whenever its endpoint is set even when `include_in_auto` is
    /// false, and `chrome_proxy` is held out of the auto ladder as a
    /// hard-block recovery arm when `auto_egress_escalation` is on.
    pub available: Vec<String>,
    /// Effective `renderer.mode`.
    pub mode: crw_core::config::RendererMode,
    /// Effective `renderer.render_js_default`; omitted when unset (auto-detect).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_js_default: Option<bool>,
    /// Effective `renderer.cloak_opt_in_only`: when true, the cloak tier only
    /// runs for requests the front end explicitly allowed. A deployment that
    /// bills the tier separately checks this before turning it on.
    pub cloak_opt_in: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    /// Max URLs per batch-scrape submission (`crawler.max_batch_urls`).
    pub max_batch_urls: usize,
    /// Max URLs per `/v1/extract` request (`crawler.max_extract_urls`).
    pub max_extract_urls: usize,
    /// `/v1/search` `limit` default when the request omits it.
    pub search_default_limit: u32,
    /// Hard cap on `/v1/search` `limit`.
    pub search_max_limit: u32,
    /// Enforced `/v2/parse` upload cap, in bytes.
    pub max_upload_bytes: usize,
}

/// Formats every build produces, independent of features and config.
const BASE_FORMATS: &[&str] = &[
    "markdown",
    "html",
    "rawHtml",
    "plainText",
    "links",
    "json",
    "summary",
    "changeTracking",
];

/// Formats that additionally need an LLM (server key or per-request BYOK key).
const LLM_REQUIRED_FORMATS: &[&str] = &["json", "summary"];

/// Change-tracking modes. `gitDiff` is deterministic; `json` calls an LLM.
const CHANGE_TRACKING_MODES: &[&str] = &["gitDiff", "json"];
const LLM_REQUIRED_CHANGE_TRACKING_MODES: &[&str] = &["json"];

pub async fn capabilities(State(state): State<AppState>) -> Json<Capabilities> {
    let llm_cfg = state.config.extraction.llm.as_ref();
    let server_key_configured = llm_cfg.map(|c| !c.api_key.is_empty()).unwrap_or(false);
    // With the BYOK header guard on, the scrape and extract paths reject
    // LLM-backed work unless the request brings its own key — the server key
    // does not count. `search` has no such guard, hence the split.
    let byok_header_required = llm_cfg.is_some_and(|c| c.require_byok_header.is_some());
    let llm_ready_without_caller_key = server_key_configured && !byok_header_required;

    // Search is usable exactly when the SearXNG client was constructed, which
    // happens only when `search.enabled && search.search_backend_url.is_some()`.
    let search_supported = state.searxng.is_some();
    // Answer / summarize additionally need an LLM. Report what works with NO
    // caller-supplied key; BYOK still enables them per request.
    let search_llm_ready = search_supported && server_key_configured;

    // Screenshot capture needs a renderer that can actually capture. The
    // predicate is shared with the request-time filter in crw-renderer, so this
    // can never advertise a screenshot the scrape path would refuse.
    let screenshot_supported = state.renderer.supports_screenshot();

    let mut formats: Vec<&'static str> = BASE_FORMATS.to_vec();
    if screenshot_supported {
        formats.push("screenshot");
    }

    let pdf_on = crw_extract::pdf::PDF_SUPPORTED && state.config.document.enabled;
    let max_upload_bytes = crate::routes::v2::parse::effective_max_upload_bytes(&state.config);
    // A zero cap means the body-limit layer rejects every upload, so uploads are
    // not supported however the parser is compiled.
    let upload_on = pdf_on && max_upload_bytes > 0;

    Json(Capabilities {
        version: env!("CARGO_PKG_VERSION"),
        llm: LlmCapabilities {
            providers: crw_extract::llm::SUPPORTED_PROVIDERS.to_vec(),
            supports_base_url: true,
            server_key_configured,
            max_concurrency: llm_cfg.map(|c| c.max_concurrency).unwrap_or(0),
            require_byok_header: llm_cfg.and_then(|c| c.require_byok_header.clone()),
        },
        formats: FormatCapabilities {
            supported: formats,
            llm_required: LLM_REQUIRED_FORMATS.to_vec(),
            change_tracking_modes: CHANGE_TRACKING_MODES.to_vec(),
            change_tracking_modes_llm_required: LLM_REQUIRED_CHANGE_TRACKING_MODES.to_vec(),
        },
        search: SearchCapabilities {
            supported: search_supported,
            answer: search_llm_ready,
            summarize_results: search_llm_ready,
        },
        screenshot: ScreenshotCapabilities {
            supported: screenshot_supported,
            full_page: screenshot_supported,
        },
        renderers: RendererCapabilities {
            available: state
                .renderer
                .available_renderer_names()
                .into_iter()
                .map(String::from)
                .collect(),
            mode: state.config.renderer.mode,
            render_js_default: state.config.renderer.render_js_default,
            cloak_opt_in: state.config.renderer.cloak_opt_in_only,
        },
        extract: ExtractCapabilities {
            supported: llm_ready_without_caller_key,
            max_urls: state.config.crawler.max_extract_urls,
            // True from the build that shipped `basis`. Scoped exactly like
            // `supported` above: it reports what this binary implements, not
            // whether an LLM happens to be configured (extraction of any kind
            // needs one, and reports that per request).
            per_field_attribution: true,
            // The cap the basis leg is actually bounded by. When no extraction
            // LLM is configured basis cannot run, but the effective default is
            // still reported so a consumer never reads a 0. Matches
            // `config::default_llm_max_tokens()`.
            max_output_tokens: state
                .config
                .extraction
                .llm
                .as_ref()
                .map_or(4096, |c| c.max_tokens),
        },
        documents: DocumentCapabilities {
            parsers: if pdf_on { vec!["pdf"] } else { vec![] },
            file_upload: FileUploadCapabilities {
                supported: upload_on,
                endpoint: "/v2/parse",
                max_bytes: max_upload_bytes,
                types: if upload_on {
                    vec!["application/pdf"]
                } else {
                    vec![]
                },
                ocr: false,
            },
        },
        limits: Limits {
            max_batch_urls: state.config.crawler.max_batch_urls,
            max_extract_urls: state.config.crawler.max_extract_urls,
            search_default_limit: state.config.search.default_limit,
            search_max_limit: state.config.search.max_limit,
            max_upload_bytes,
        },
    })
}
