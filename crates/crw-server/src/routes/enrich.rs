//! `POST /v1/enrich`: fill the empty cells of a small table from the web.
//!
//! Per row: read the row's own website (the anchor) if it has one, search once
//! for whatever is still missing, extract the top results in parallel, and merge
//! per cell in rank order. Every cell is a [`Basis`] record from the existing
//! per-field evidence path, so a filled cell is exactly as trustworthy as an
//! `/v1/extract` field with `basis: true`.
//!
//! Synchronous and deliberately small (a few rows per call). The hosted product
//! drives larger tables one row per call from its own durable job, so nothing
//! here needs a job store or survives a restart.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::time::{Instant, timeout};

use crw_core::Deadline;
use crw_core::config::LlmConfig;
use crw_core::error::CrwError;
use crw_core::evidence::{Basis, FieldStatus};
use crw_core::types::{LlmUsage, OutputFormat, ScrapeRequest, SearchRequest, SearchResult};
use crw_crawl::single::{build_byok_llm_config, scrape_url};
use crw_extract::structured::{
    BasisDoc, SNIPPET_TEXT_KIND, SOURCE_TEXT_KIND, extract_structured_with_basis_sources,
};
use crw_search::{
    StructuredFact, map_to_searxng_params, structured_facts, transform_flat_reranked,
};

use crate::error::AppError;
use crate::routes::search::map_search_error;
use crate::state::AppState;

/// Rows per call. Three rows run as one concurrent wave, which keeps the whole
/// call inside the server's request timeout.
pub const MAX_ROWS: usize = 3;
pub const MAX_COLUMNS: usize = 10;
// ponytail: fixed bounds, not config keys. They size the hosted reserve; make
// them `[enrich]` keys only if a self-hoster needs different ones.
const ROW_TIMEOUT: Duration = Duration::from_secs(80);
const PAGE_DEADLINE_MS: u64 = 15_000;
/// LLM input cap per extraction, all sources together. Keeps every leg inside
/// the hosted reserve (crw-saas `ENRICH_LLM_INPUT_BYTES`).
const LLM_INPUT_CAP: usize = 20_000;
/// Share of `LLM_INPUT_CAP` kept for snippets and facts when a step has any;
/// the page bodies split the rest (one page alone gets nearly all of it).
const SNIPPET_SHARE: usize = 4_000;
/// Basis needs about 800 + 90 tokens per column; 10 columns fit in 2048.
const LLM_MAX_TOKENS: u32 = 2048;
/// One search credit returns up to 10 results whatever the limit.
const SEARCH_LIMIT: u32 = 10;
/// Snippets sent with each extraction.
const SNIPPET_SOURCES: usize = 5;
/// Per row: one search for the entity, one focused search for a column still
/// missing. Pages: the anchor, 3 from the first search, 2 from the second; one
/// extraction per step. crw-saas sizes its reserve from these
/// (`ENRICH_ROW_MAX_*` in `lib/enrich.ts`): 2 searches, 6 pages, 3 LLM legs.
const BROAD_PAGES: usize = 3;
const FOCUS_PAGES: usize = 2;
/// The focused search starts only with time left for its pages and their
/// extraction; otherwise the row ends `completed` with what it has.
const FOCUS_MIN_LEFT: Duration = Duration::from_secs(30);
/// Video, social and app-store pages: in the golden run 68 of 270 pages read
/// were video or social and gave 1 supported cell; app-store listings rank high
/// for a company name (Spotify: Play Store above Wikipedia) and state none of a
/// company's facts. Their snippets still count; their pages are not fetched.
const SKIP_HOSTS: [&str; 6] = [
    "youtube.com",
    "facebook.com",
    "instagram.com",
    "tiktok.com",
    "play.google.com",
    "apps.apple.com",
];
const MAX_IDENTITY_CHARS: usize = 200;
/// Prompt-size bounds, so the per-page LLM input stays near the reserve.
const MAX_IDENTITY_TOTAL_CHARS: usize = 1_000;
const MAX_CONTEXT_CHARS: usize = 500;
const MAX_DESCRIPTION_CHARS: usize = 500;
const MAX_OPTIONS: usize = 30;
const MAX_OPTION_CHARS: usize = 80;
const MAX_NAME_CHARS: usize = 100;

/// NOTE: no `#[derive(Debug)]`: `llm_api_key` is a secret.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichRequest {
    pub rows: Vec<Map<String, Value>>,
    pub columns: Vec<Column>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub llm_api_key: Option<String>,
    #[serde(default)]
    pub llm_provider: Option<String>,
    #[serde(default)]
    pub llm_model: Option<String>,
    /// Parsed only to reject it, as `/v1/extract` does.
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: ColumnType,
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColumnType {
    #[default]
    Text,
    Number,
    Url,
    Options,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichResponse {
    pub success: bool,
    /// Same envelope as the hosted job's GET, so one parser reads both.
    pub status: &'static str,
    pub data: EnrichData,
}

#[derive(Debug, Serialize)]
pub struct EnrichData {
    pub rows: Vec<RowResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RowStatus {
    Completed,
    /// The row ran out of time: cells it never reached are absent from `cells`.
    Incomplete,
    /// No page could be read for this row.
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowResult {
    pub index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_id: Option<Value>,
    pub status: RowStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// One entry per target cell that was looked for. A cell absent here was
    /// never reached (unknown), which is different from `notFound`.
    pub cells: Map<String, Value>,
    pub usage: RowUsage,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowUsage {
    /// Pages fetched and extracted (billable), in the order they were read.
    pub pages: Vec<String>,
    pub searches: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_usage: Option<LlmUsage>,
}

pub async fn enrich(
    State(state): State<AppState>,
    body: Result<Json<EnrichRequest>, JsonRejection>,
) -> Result<Json<EnrichResponse>, AppError> {
    let Json(req) = body.map_err(AppError::from)?;
    Ok(Json(enrich_inner(&state, req).await?))
}

/// Shared by the HTTP route and the MCP tool.
pub async fn enrich_inner(
    state: &AppState,
    mut req: EnrichRequest,
) -> Result<EnrichResponse, CrwError> {
    // Canonical names once, so every lookup below agrees with the schema.
    for c in &mut req.columns {
        c.name = c.name.trim().to_string();
    }
    validate(&req)?;
    let llm = llm_config(state, &req)?;
    // Refuse a column set whose evidence cannot fit the model's output cap
    // before any page is fetched, not after every page fails.
    let all: Vec<&Column> = req.columns.iter().collect();
    if let Some(reason) = crw_extract::basis_reject_reason(&build_schema(&all), llm.max_tokens) {
        return Err(CrwError::InvalidRequest(reason));
    }
    // Stay strictly inside the server's request timeout so a slow row returns
    // its partial cells instead of the whole call becoming a 504.
    let request_secs = state.config.effective_request_timeout_secs();
    let margin = 10.min(request_secs / 2);
    let row_budget = ROW_TIMEOUT.min(Duration::from_secs(request_secs - margin));
    let ctx = RowCtx {
        state,
        columns: &req.columns,
        context: req
            .context
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty()),
        overwrite: req.overwrite,
        llm: &llm,
        row_budget,
    };
    // Batch class: enrich must use the batch lanes, never the interactive
    // reserve. No spawn below, so the task-local scope reaches every scrape.
    let rows = crw_core::REQUEST_CLASS
        .scope(
            crw_core::ScrapeClass::Batch,
            futures::future::join_all(
                req.rows
                    .iter()
                    .enumerate()
                    .map(|(index, row)| enrich_row(&ctx, index, row)),
            ),
        )
        .await;
    Ok(EnrichResponse {
        success: true,
        status: "completed",
        data: EnrichData { rows },
    })
}

fn validate(req: &EnrichRequest) -> Result<(), CrwError> {
    let bad = |m: String| Err(CrwError::InvalidRequest(m));
    if req.rows.is_empty() {
        return bad("`rows` is required and must be non-empty".into());
    }
    if req.rows.len() > MAX_ROWS {
        return bad(format!(
            "too many rows: {} exceeds the per-call limit of {MAX_ROWS}; send larger tables in \
             chunks of {MAX_ROWS}",
            req.rows.len()
        ));
    }
    if req.columns.is_empty() {
        return bad("`columns` is required and must be non-empty".into());
    }
    if req.columns.len() > MAX_COLUMNS {
        return bad(format!(
            "too many columns: {} exceeds the limit of {MAX_COLUMNS}",
            req.columns.len()
        ));
    }
    if req
        .context
        .as_deref()
        .is_some_and(|c| c.chars().count() > MAX_CONTEXT_CHARS)
    {
        return bad(format!(
            "`context` is longer than {MAX_CONTEXT_CHARS} characters"
        ));
    }
    let mut seen = HashSet::new();
    for c in &req.columns {
        let name = c.name.as_str();
        // `basis` is where per-field evidence rides in the extraction schema.
        if name.is_empty() || name == "id" || name == "basis" {
            return bad("every column needs a non-empty `name` other than `id` or `basis`".into());
        }
        if name.chars().count() > MAX_NAME_CHARS {
            return bad(format!(
                "column names are limited to {MAX_NAME_CHARS} characters"
            ));
        }
        if !seen.insert(name) {
            return bad(format!("duplicate column `{name}`"));
        }
        if c.description
            .as_deref()
            .is_some_and(|d| d.chars().count() > MAX_DESCRIPTION_CHARS)
        {
            return bad(format!(
                "column `{name}`: `description` is longer than {MAX_DESCRIPTION_CHARS} characters"
            ));
        }
        if c.kind == ColumnType::Options {
            validate_options(name, &c.options)?;
        }
    }
    if req.base_url.is_some() {
        return bad(
            "`baseUrl` is not supported on /v1/enrich; configure the LLM endpoint server-side \
             ([extraction.llm.base_url])"
                .into(),
        );
    }
    Ok(())
}

fn validate_options(name: &str, options: &[String]) -> Result<(), CrwError> {
    let bad = |m: String| Err(CrwError::InvalidRequest(m));
    if options.is_empty() || options.len() > MAX_OPTIONS {
        return bad(format!(
            "column `{name}` has type `options` and needs 1 to {MAX_OPTIONS} `options`"
        ));
    }
    let mut keys = HashSet::new();
    for o in options {
        let key = norm(o);
        if key.is_empty() || o.chars().count() > MAX_OPTION_CHARS {
            return bad(format!(
                "column `{name}`: every option needs a letter or digit and at most \
                 {MAX_OPTION_CHARS} characters"
            ));
        }
        if !keys.insert(key) {
            return bad(format!(
                "column `{name}`: options `{o}` and another label differ only in case, \
                 spacing or punctuation"
            ));
        }
    }
    Ok(())
}

fn llm_config(state: &AppState, req: &EnrichRequest) -> Result<LlmConfig, CrwError> {
    let server = state.config.extraction.llm.as_ref();
    if server.is_some_and(|c| c.require_byok_header.is_some()) && req.llm_api_key.is_none() {
        return Err(CrwError::InvalidRequest(
            "LLM features require a per-request llmApiKey (BYOK header guard active)".into(),
        ));
    }
    let byok = ScrapeRequest {
        llm_api_key: req.llm_api_key.clone(),
        llm_provider: req.llm_provider.clone(),
        llm_model: req.llm_model.clone(),
        ..Default::default()
    };
    let mut cfg = build_byok_llm_config(&byok, server)
        .or_else(|| server.cloned())
        .ok_or_else(|| {
            CrwError::InvalidRequest(
                "enrich requires an LLM: set [extraction.llm] in server config or pass \
                 llmApiKey in the request body"
                    .into(),
            )
        })?;
    cfg.max_tokens = cfg.max_tokens.min(LLM_MAX_TOKENS);
    Ok(cfg)
}

struct RowCtx<'a> {
    state: &'a AppState,
    columns: &'a [Column],
    context: Option<&'a str>,
    overwrite: bool,
    llm: &'a LlmConfig,
    row_budget: Duration,
}

/// What one row has learned so far.
#[derive(Default)]
struct RowWork {
    /// Best cell per column: a `supported` or `unverified` Basis.
    found: HashMap<String, Basis>,
    /// Columns that at least one completed extraction looked for. Empty means
    /// no extraction ran: the row failed.
    looked: HashSet<String>,
    usage: RowUsage,
    timed_out: bool,
    last_error: Option<PageError>,
}

impl RowWork {
    fn missing<'c>(&self, targets: &[&'c Column]) -> Vec<&'c Column> {
        targets
            .iter()
            .filter(|c| {
                !self
                    .found
                    .get(&c.name)
                    .is_some_and(|b| b.status == FieldStatus::Supported)
            })
            .copied()
            .collect()
    }

    /// Fold one extraction in. Steps arrive in order, so the first `supported`
    /// value wins; an `unverified` one only holds a cell until a `supported`
    /// one shows up, and a page-backed `unverified` replaces a snippet-backed one.
    fn merge(&mut self, step: StepOutcome, columns: &[&Column]) {
        for url in step.pages {
            // Two candidates can land on one document (a redirect, `www.`): bill it once.
            let key = url_key(&url);
            if !self.usage.pages.iter().any(|p| url_key(p) == key) {
                self.usage.pages.push(url);
            }
        }
        LlmUsage::accumulate(&mut self.usage.llm_usage, step.usage);
        self.looked.extend(columns.iter().map(|c| c.name.clone()));
        for b in step.basis {
            let replace = match (self.found.get(&b.field), b.status) {
                (_, FieldStatus::Unsupported | FieldStatus::NotFound) => false,
                (None, _) => true,
                (Some(old), FieldStatus::Supported) => old.status == FieldStatus::Unverified,
                (Some(old), FieldStatus::Unverified) => {
                    old.status == FieldStatus::Unverified && from_snippet(old) && !from_snippet(&b)
                }
            };
            if replace {
                self.found.insert(b.field.clone(), b);
            }
        }
    }
}

fn from_snippet(b: &Basis) -> bool {
    b.citations
        .first()
        .is_some_and(|c| c.source_text_kind == SNIPPET_TEXT_KIND)
}

/// One extraction's result: the pages it read (billable) and its cells.
struct StepOutcome {
    pages: Vec<String>,
    basis: Vec<Basis>,
    usage: Option<LlmUsage>,
}

/// Why a page or the search gave nothing. The wire carries only these
/// crw-owned messages; upstream detail goes to the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageError {
    Unsafe,
    Unreachable,
    Blocked,
    Empty,
    Extraction,
    Search,
}

impl PageError {
    fn message(self) -> &'static str {
        match self {
            PageError::Unsafe => "the row's URL is not a public web address",
            PageError::Unreachable => "no page for this row could be fetched",
            PageError::Blocked => "the pages for this row could not be read",
            PageError::Empty => "the pages for this row had no readable content",
            PageError::Extraction => "extraction failed for every source read",
            PageError::Search => "search failed for this row",
        }
    }
}

/// What one search returned: reranked results and the knowledge-panel facts.
#[derive(Default)]
struct Hits {
    results: Vec<SearchResult>,
    facts: Vec<StructuredFact>,
}

/// What a row is about, worked out once.
struct RowKey<'c> {
    targets: Vec<&'c Column>,
    identity: Vec<String>,
    /// Sources must mention it to be read; `None` reads every source.
    entity: Option<String>,
}

async fn enrich_row(ctx: &RowCtx<'_>, index: usize, row: &Map<String, Value>) -> RowResult {
    let started = Instant::now();
    let deadline = started + ctx.row_budget;
    let targets = target_columns(ctx.columns, row, ctx.overwrite);
    let mut work = RowWork::default();

    if !targets.is_empty() {
        let key = RowKey {
            identity: identity_values(row, &targets),
            entity: entity_name(row, &targets).map(str::to_string),
            targets: targets.clone(),
        };
        let mut seen = HashSet::new();

        // 0. The row's own website: it is the row's entity, so no filter.
        if let Some(anchor) = anchor_url(row, ctx.columns) {
            seen.insert(url_key(&anchor));
            let own = RowKey {
                entity: None,
                identity: key.identity.clone(),
                targets: targets.clone(),
            };
            let hits = Hits::default();
            read_step(ctx, &mut work, &own, &hits, &[anchor], &mut seen, deadline).await;
        }

        // 1-2. Search for the entity, then read its snippets, facts and top pages
        //      in ONE extraction. 3. One focused search for a column still
        //      missing, only if the first search found the entity at all.
        let mut found_entity = false;
        for focus in [false, true] {
            let missing = work.missing(&targets);
            if missing.is_empty() || work.timed_out || ctx.state.searxng.is_none() {
                break;
            }
            if focus && (!found_entity || remaining(deadline) < FOCUS_MIN_LEFT) {
                break;
            }
            // The first missing column in the caller's order. A column that only
            // a snippet backs counts as missing: confirming it on a page (a
            // company's own about or leadership page) is what the search is for.
            let query = search_query(&key.identity, ctx.context, focus.then_some(missing[0]));
            let hits = match timeout(remaining(deadline), search_hits(ctx.state, &query)).await {
                Err(_) => {
                    work.timed_out = true;
                    break;
                }
                Ok(Err(e)) => {
                    tracing::debug!(row = index, "enrich search failed: {e}");
                    work.last_error = Some(PageError::Search);
                    break;
                }
                Ok(Ok(hits)) => hits,
            };
            work.usage.searches += 1;
            let budget = if focus { FOCUS_PAGES } else { BROAD_PAGES };
            let pages: Vec<String> = hits
                .results
                .iter()
                .filter(|r| !skip_host(&r.url))
                .filter(|r| on_entity(&key, &format!("{} {} {}", r.title, r.description, r.url)))
                .filter(|r| seen.insert(url_key(&r.url)))
                .take(budget)
                .map(|r| r.url.clone())
                .collect();
            let read = read_step(ctx, &mut work, &key, &hits, &pages, &mut seen, deadline).await;
            if !read && !focus && !work.timed_out && work.last_error.is_none() {
                // The search ran and nothing names this entity: every column is
                // honestly not found, which is an answer, not a failure.
                work.looked.extend(missing.iter().map(|c| c.name.clone()));
            }
            found_entity |= read;
        }
    }

    finish_row(index, row, &targets, work)
}

fn on_entity(key: &RowKey<'_>, text: &str) -> bool {
    key.entity.as_deref().is_none_or(|e| mentions(text, e))
}

/// Prompt bytes a source costs beyond its text: the `Source #i` / `URL:` header,
/// the two fence lines and its line in the URL list.
fn framing(url: &str) -> usize {
    2 * url.len() + 128
}

/// Fetch `pages`, then run ONE extraction over their bodies plus the search's
/// facts and snippets, keeping only sources about the row's entity. Returns
/// whether any source was sent to the model.
async fn read_step(
    ctx: &RowCtx<'_>,
    work: &mut RowWork,
    key: &RowKey<'_>,
    hits: &Hits,
    pages: &[String],
    seen: &mut HashSet<String>,
    deadline: Instant,
) -> bool {
    let missing = work.missing(&key.targets);
    if missing.is_empty() {
        return false;
    }
    let fetched = futures::future::join_all(
        pages
            .iter()
            .map(|url| async move { timeout(remaining(deadline), fetch_page(ctx, url)).await }),
    )
    .await;
    let mut bodies = Vec::new();
    for outcome in fetched {
        match outcome {
            Err(_) => work.timed_out = true,
            Ok(Err(e)) => work.last_error = Some(e),
            Ok(Ok((url, md))) => {
                // A redirect target is the same document for the next step.
                seen.insert(url_key(&url));
                if on_entity(key, &md) {
                    bodies.push((url, md));
                }
            }
        }
    }
    let snippets: Vec<(String, String)> = hits
        .facts
        .iter()
        .filter(|f| !f.url.is_empty())
        .map(|f| (f.url.clone(), format!("{}\n{}", f.title, f.to_markdown())))
        .chain(
            hits.results
                .iter()
                .filter(|r| !r.description.trim().is_empty() && !skip_host(&r.url))
                .take(SNIPPET_SOURCES)
                .map(|r| {
                    (
                        r.url.clone(),
                        format!("{}\n{}", r.title, r.description.trim()),
                    )
                }),
        )
        .filter(|(_, text)| on_entity(key, text))
        .collect();

    // The whole prompt stays within LLM_INPUT_CAP: snippets get a fixed share
    // when there are any, the pages split the rest.
    let snippet_share = if snippets.is_empty() {
        0
    } else {
        SNIPPET_SHARE
    };
    let per_page = (LLM_INPUT_CAP - snippet_share) / bodies.len().max(1);
    let focus = passage_query(key, &missing);
    let mut docs = Vec::new();
    let mut read = Vec::new();
    let mut room = LLM_INPUT_CAP;
    for (url, md) in bodies {
        let cap = per_page.saturating_sub(framing(&url));
        let body = crw_extract::answer::select_relevant_passages(&md, &focus, cap);
        room = room.saturating_sub(body.len() + framing(&url));
        read.push(url.clone());
        docs.push(BasisDoc::new(url, body, SOURCE_TEXT_KIND));
    }
    for (url, text) in snippets {
        let cost = text.len() + framing(&url);
        if cost > room {
            continue;
        }
        room -= cost;
        docs.push(BasisDoc::new(url, text, SNIPPET_TEXT_KIND));
    }
    if docs.is_empty() {
        return false;
    }
    let schema = build_schema(&missing);
    let prompt = extraction_prompt(&key.identity, ctx.context);
    let result = match timeout(
        remaining(deadline),
        extract_structured_with_basis_sources(&docs, &schema, Some(&prompt), ctx.llm),
    )
    .await
    {
        Err(_) => {
            work.timed_out = true;
            return true;
        }
        Ok(Err(e)) => {
            tracing::warn!(sources = docs.len(), "enrich extraction failed: {e}");
            work.last_error = Some(PageError::Extraction);
            return true;
        }
        Ok(Ok(r)) => r,
    };
    let mut basis = result.basis;
    map_options(&mut basis, &missing);
    for b in &mut basis {
        settle_cell(b);
    }
    work.merge(
        StepOutcome {
            pages: read,
            basis,
            usage: result.usage,
        },
        &missing,
    );
    true
}

/// Snippets go stale and a knowledge panel can describe a namesake, so a cell
/// only a snippet backs is never `supported`: it stays `unverified` (without
/// its excerpt, which ADR 0001 reserves for `supported`) and its column stays
/// missing, so a page can confirm it. An empty string is no value.
fn settle_cell(b: &mut Basis) {
    if b.value
        .as_ref()
        .and_then(Value::as_str)
        .is_some_and(|v| v.trim().is_empty())
    {
        *b = not_found(&b.field);
        return;
    }
    if b.status == FieldStatus::Supported && from_snippet(b) {
        b.status = FieldStatus::Unverified;
        for c in &mut b.citations {
            c.excerpt = None;
        }
    }
}

async fn fetch_page(ctx: &RowCtx<'_>, url: &str) -> Result<(String, String), PageError> {
    let parsed = url::Url::parse(url).map_err(|_| PageError::Unsafe)?;
    crw_core::url_safety::validate_safe_url_resolved(&parsed)
        .await
        .map_err(|e| {
            tracing::debug!(url, "enrich url refused: {e}");
            PageError::Unsafe
        })?;

    let state = ctx.state;
    // Same process-wide aggregate cap the batch pipeline takes per URL.
    let _permit = match &state.batch_pipeline_sem {
        Some(sem) => sem.acquire().await.ok(),
        None => None,
    };
    let config = &state.config;
    let req = ScrapeRequest {
        url: url.to_string(),
        formats: vec![OutputFormat::Markdown],
        deadline_ms: Some(PAGE_DEADLINE_MS),
        // Tenant pages never enter the shared cache bucket (as search does).
        max_age: Some(0),
        store_in_cache: Some(false),
        ..Default::default()
    };
    let doc = scrape_url(
        &req,
        &state.renderer,
        // The request's own (bounded, possibly BYOK) config, so an operator's
        // LLM fallback never spends the server key on a BYOK row.
        Some(ctx.llm),
        &config.extraction,
        &config.crawler.user_agent,
        config.crawler.stealth.enabled && config.crawler.stealth.inject_headers,
        config.renderer.render_js_default,
        Deadline::from_request_ms(PAGE_DEADLINE_MS),
    )
    .await
    .map_err(|e| {
        tracing::debug!(url, "enrich fetch failed: {e}");
        PageError::Unreachable
    })?;
    if doc.block.is_some() || doc.http_error().is_some() {
        return Err(PageError::Blocked);
    }
    let md = doc.markdown.unwrap_or_default();
    if md.trim().is_empty() {
        return Err(PageError::Empty);
    }
    let final_url = doc.metadata.final_url.unwrap_or_else(|| url.to_string());
    Ok((final_url, md))
}

/// The search stack's own retrieval, without the answer path: one backend
/// call, the relevance rerank `/v1/search` uses for answers, and the
/// knowledge-panel facts that `/v1/search` itself only reads for answers.
async fn search_hits(state: &AppState, query: &str) -> Result<Hits, CrwError> {
    let client = state
        .searxng
        .as_ref()
        .ok_or_else(|| CrwError::SearchDisabled("search is not configured".into()))?;
    let limit = SEARCH_LIMIT.min(state.config.search.max_limit);
    let req: SearchRequest = serde_json::from_value(json!({ "query": query, "limit": limit }))
        .map_err(|e| CrwError::Internal(format!("search request: {e}")))?;
    let params = map_to_searxng_params(&req, &state.config.search);
    let resp = client
        .fetch(&params)
        .await
        .map_err(|e| map_search_error(e, state.config.search.timeout_ms, client.base_url()))?;
    if resp.is_degraded() {
        return Err(CrwError::SearchDegraded(
            "search backend could not answer".into(),
        ));
    }
    Ok(Hits {
        results: transform_flat_reranked(&resp, query, limit, state.config.search.rerank_relevance),
        facts: structured_facts(&resp),
    })
}

fn finish_row(
    index: usize,
    row: &Map<String, Value>,
    targets: &[&Column],
    work: RowWork,
) -> RowResult {
    let mut cells = Map::new();
    let mut found = work.found;
    for c in targets {
        let cell = match found.remove(&c.name) {
            Some(b) => b,
            None if work.looked.contains(&c.name) => not_found(&c.name),
            None => continue,
        };
        if let Ok(v) = serde_json::to_value(cell) {
            cells.insert(c.name.clone(), v);
        }
    }
    let (status, stop_reason, error) = if !targets.is_empty() && work.looked.is_empty() {
        let err = if work.timed_out {
            "timed out before any source was read"
        } else {
            work.last_error
                .map_or("no page could be found for this row", PageError::message)
        };
        (
            RowStatus::Failed,
            work.timed_out.then_some("timeout"),
            Some(err.to_string()),
        )
    } else if work.timed_out {
        (RowStatus::Incomplete, Some("timeout"), None)
    } else {
        (RowStatus::Completed, None, None)
    };
    RowResult {
        index,
        row_id: row
            .get("id")
            .filter(|v| v.is_string() || v.is_number())
            .cloned(),
        status,
        stop_reason,
        error,
        cells,
        usage: work.usage,
    }
}

fn not_found(field: &str) -> Basis {
    Basis {
        basis_version: 1,
        field: field.to_string(),
        value: None,
        status: FieldStatus::NotFound,
        confidence: None,
        reasoning: None,
        citations: Vec::new(),
    }
}

fn is_blank(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        _ => false,
    }
}

fn target_columns<'c>(
    columns: &'c [Column],
    row: &Map<String, Value>,
    overwrite: bool,
) -> Vec<&'c Column> {
    columns
        .iter()
        .filter(|c| overwrite || is_blank(row.get(c.name.trim())))
        .collect()
}

/// Non-empty scalar row values that are not this row's targets and not `id`:
/// what identifies the row's entity. A value being refilled (`overwrite`) is
/// left out, so a stale value never steers the search or the extraction.
fn identity_values(row: &Map<String, Value>, targets: &[&Column]) -> Vec<String> {
    let targets: HashSet<&str> = targets.iter().map(|c| c.name.as_str()).collect();
    let mut total = 0;
    row.iter()
        .filter(|(k, _)| k.as_str() != "id" && !targets.contains(k.as_str()))
        .filter_map(|(k, v)| {
            let s = match v {
                Value::String(s) => s.trim().to_string(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => return None,
            };
            (!s.is_empty()).then(|| format!("{k}: {}", truncate(&s, MAX_IDENTITY_CHARS)))
        })
        .take_while(|kv| {
            total += kv.chars().count();
            total <= MAX_IDENTITY_TOTAL_CHARS
        })
        .collect()
}

/// Dedup key for candidate pages: host without `www.`, path without a
/// trailing slash, and the query. Scheme and fragment do not matter.
fn url_key(u: &str) -> String {
    match url::Url::parse(u) {
        Ok(p) => format!(
            "{}{}?{}",
            p.host_str().unwrap_or("").trim_start_matches("www."),
            p.path().trim_end_matches('/'),
            p.query().unwrap_or("")
        ),
        Err(_) => u.to_string(),
    }
}

/// The first row value that reads as a website: a URL or a bare domain.
fn anchor_url(row: &Map<String, Value>, columns: &[Column]) -> Option<String> {
    let url_cols: HashSet<&str> = columns
        .iter()
        .filter(|c| c.kind == ColumnType::Url)
        .map(|c| c.name.trim())
        .collect();
    let mut values: Vec<(&String, &str)> = row
        .iter()
        .filter(|(k, _)| k.as_str() != "id")
        .filter_map(|(k, v)| v.as_str().map(|s| (k, s.trim())))
        .collect();
    // A declared url column wins over a value that merely looks like a domain.
    values.sort_by_key(|(k, _)| !url_cols.contains(k.as_str()));
    values.into_iter().find_map(|(_, s)| as_website(s))
}

fn as_website(s: &str) -> Option<String> {
    if s.is_empty() || s.contains(char::is_whitespace) || s.contains('@') {
        return None;
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        // An explicit URL is a website whatever its host looks like; the SSRF
        // check runs before any fetch.
        let parsed = url::Url::parse(s).ok()?;
        return parsed.host_str().is_some().then(|| parsed.to_string());
    }
    let parsed = url::Url::parse(&format!("https://{s}")).ok()?;
    let host = parsed.host_str()?;
    // A bare word ("Stripe") is not a domain; require a dot and a 2+ letter TLD.
    let tld = host.rsplit('.').next()?;
    (host.contains('.') && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()))
        .then(|| parsed.to_string())
}

/// The entity's values and the table context, plus, for a focused search, the
/// one column it looks for in words a search engine reads: its description,
/// or its name split apart (`hqCity` reads as noise, `hq city` does not).
fn search_query(identity: &[String], context: Option<&str>, focus: Option<&Column>) -> String {
    let mut parts: Vec<String> = identity
        .iter()
        .map(|kv| {
            kv.split_once(": ")
                .map_or(kv.as_str(), |(_, v)| v)
                .to_string()
        })
        .collect();
    if let Some(c) = context {
        parts.push(truncate(c, 80).to_string());
    }
    if let Some(col) = focus {
        parts.push(match col.description.as_deref().map(str::trim) {
            Some(d) if !d.is_empty() => truncate(d, 80).to_string(),
            _ => column_words(&col.name),
        });
    }
    truncate(parts.join(" ").trim(), 300).to_string()
}

/// Row keys that name the entity, in the order they are tried.
const NAME_KEYS: [&str; 10] = [
    "name",
    "company",
    "organization",
    "organisation",
    "university",
    "school",
    "project",
    "product",
    "repo",
    "brand",
];

/// The row's name for its entity: a non-target text value under a name-like
/// key, else the row's only non-URL text value. Rows are maps in key order, not
/// in the caller's column order, so position cannot tell the name apart from,
/// say, an `industry` column. `None` (no filter) when it cannot be told.
fn entity_name<'r>(row: &'r Map<String, Value>, targets: &[&Column]) -> Option<&'r str> {
    let texts: Vec<(&str, &str)> = row
        .iter()
        .filter(|(k, _)| k.as_str() != "id" && !targets.iter().any(|c| c.name == **k))
        .filter_map(|(k, v)| v.as_str().map(|v| (k.as_str(), v.trim())))
        .filter(|(_, v)| !v.is_empty() && as_website(v).is_none() && !fold(v).is_empty())
        .collect();
    let by_key = NAME_KEYS.iter().find_map(|n| {
        texts
            .iter()
            .find(|(k, _)| k.to_lowercase().contains(n))
            .map(|(_, v)| *v)
    });
    by_key.or(match texts.as_slice() {
        [(_, only)] => Some(*only),
        _ => None,
    })
}

/// Trailing legal forms a page usually leaves out ("Stripe, Inc." -> "stripe").
const LEGAL_SUFFIXES: [&str; 12] = [
    "inc",
    "llc",
    "ltd",
    "limited",
    "gmbh",
    "ag",
    "sa",
    "plc",
    "corp",
    "corporation",
    "co",
    "incorporated",
];

/// Whether `text` names `entity`: as whole words, accent, case and punctuation
/// insensitive, without a trailing legal form. A multi-word or non-Latin name
/// also counts when written without spaces ("MercadoLibre", "メルカリは"). For an
/// `owner/name` slug the name alone also counts.
fn mentions(text: &str, entity: &str) -> bool {
    let hay = fold(text);
    let spaced = format!(" {hay} ");
    let compact: String = hay.split(' ').collect();
    let names = [Some(entity), entity.rsplit_once('/').map(|(_, n)| n)];
    names.into_iter().flatten().any(|n| {
        let mut w: Vec<String> = fold(n).split(' ').map(str::to_string).collect();
        while w.len() > 1
            && w.last()
                .is_some_and(|l| LEGAL_SUFFIXES.contains(&l.as_str()))
        {
            w.pop();
        }
        let name = w.join(" ");
        if name.is_empty() {
            return false;
        }
        spaced.contains(&format!(" {name} "))
            || ((w.len() > 1 || !name.is_ascii()) && compact.contains(&w.concat()))
    })
}

/// Lowercase alphanumeric words joined by single spaces, accents removed.
fn fold(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn skip_host(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .is_some_and(|h| {
            SKIP_HOSTS
                .iter()
                .any(|s| h == *s || h.ends_with(&format!(".{s}")))
        })
}

/// What passage selection keeps a page's text for: the entity and the
/// columns still missing.
fn passage_query(key: &RowKey<'_>, missing: &[&Column]) -> String {
    let mut q = match &key.entity {
        Some(e) => e.clone(),
        None => key.identity.join(" "),
    };
    for c in missing {
        q.push(' ');
        q.push_str(c.description.as_deref().unwrap_or(""));
        q.push(' ');
        q.push_str(&column_words(&c.name));
    }
    q
}

/// `hqCity`, `first_release_year`, `pricing-page` -> lowercase words.
fn column_words(name: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for ch in name.trim().chars() {
        if ch == '_' || ch == '-' || ch.is_whitespace() {
            out.push(' ');
            prev_lower = false;
            continue;
        }
        if ch.is_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
        out.extend(ch.to_lowercase());
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extraction_prompt(identity: &[String], context: Option<&str>) -> String {
    let mut p = format!(
        "The sources below are read to fill one row of a table. The row is about: {}.",
        identity.join("; ")
    );
    if let Some(c) = context {
        p.push_str(&format!(" Table context: {c}."));
    }
    p.push_str(
        " A source counts only if it is clearly about this exact entity, consistent with the \
         row's other values; a namesake does not count. If none counts, return null for every \
         field. If sources disagree, use the most recent source that states the value as \
         current, and ignore anyone described as former, interim or past. Copy text values \
         exactly as they are written in the source; do not reformat or translate them. Give \
         numbers as plain digits exactly as one source states them; never add, average or \
         convert them. A count of profiles, members or followers on a third-party site (for \
         example \"View all 16,691 employees\" on a social network) is not the \
         organization's own figure. Return null for anything no source states.",
    );
    p
}

/// JSON Schema from the columns. Plain `string`/`number` leaves only: basis
/// needs a `type` key, and an `enum` would fail validation on "series a" and
/// lose every column on the page. Options are matched to labels afterwards.
fn build_schema(columns: &[&Column]) -> Value {
    let props: Map<String, Value> = columns
        .iter()
        .map(|c| {
            let mut desc = c.description.clone().unwrap_or_default();
            let ty = match c.kind {
                ColumnType::Number => "number",
                ColumnType::Text | ColumnType::Url | ColumnType::Options => "string",
            };
            match c.kind {
                ColumnType::Url => desc.push_str(" (a URL, as written on the page)"),
                ColumnType::Number => desc.push_str(" (a plain number, digits only)"),
                ColumnType::Options => {
                    desc.push_str(&format!(" (one of: {})", c.options.join(", ")))
                }
                _ => {}
            }
            (
                c.name.trim().to_string(),
                json!({ "type": ty, "description": desc.trim() }),
            )
        })
        .collect();
    json!({ "type": "object", "properties": props })
}

/// Map a verified `options` value onto one of the column's labels, comparing
/// case, space and punctuation insensitively. No match means the page did not
/// state one of the allowed values: the cell becomes `notFound`.
fn map_options(basis: &mut [Basis], columns: &[&Column]) {
    for b in basis.iter_mut() {
        let Some(col) = columns.iter().find(|c| c.name.trim() == b.field) else {
            continue;
        };
        if col.kind != ColumnType::Options {
            continue;
        }
        let label = b
            .value
            .as_ref()
            .and_then(Value::as_str)
            .map(norm)
            .filter(|v| !v.is_empty())
            .and_then(|v| col.options.iter().find(|o| norm(o) == v));
        match label {
            Some(l) => b.value = Some(Value::String(l.clone())),
            None if b.value.is_some() => *b = not_found(&b.field),
            None => {}
        }
    }
}

/// Case, space and punctuation insensitive key. Unicode letters and digits
/// survive, so labels in any script stay distinct.
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crw_core::evidence::EvidenceCitation;

    fn col(name: &str, kind: ColumnType) -> Column {
        Column {
            name: name.into(),
            description: None,
            kind,
            options: vec!["Seed".into(), "Series A".into(), "Series B".into()],
        }
    }

    fn basis(field: &str, value: &str, status: FieldStatus, url: &str) -> Basis {
        Basis {
            basis_version: 1,
            field: field.into(),
            value: Some(json!(value)),
            status,
            confidence: None,
            reasoning: None,
            citations: vec![EvidenceCitation {
                url: url.into(),
                title: None,
                excerpt: None,
                source_hash: "sha256:test".into(),
                source_text_kind: "llmInput".into(),
                char_start: None,
                char_end: None,
            }],
        }
    }

    fn page(url: &str, basis: Vec<Basis>) -> StepOutcome {
        StepOutcome {
            pages: vec![url.into()],
            basis,
            usage: None,
        }
    }

    #[test]
    fn merge_first_supported_wins_and_unverified_only_holds_a_cell() {
        let ceo = col("ceo", ColumnType::Text);
        let cols = [&ceo];
        let mut w = RowWork::default();
        w.merge(
            page("a", vec![basis("ceo", "Old", FieldStatus::Unverified, "a")]),
            &cols,
        );
        w.merge(
            page(
                "b",
                vec![basis("ceo", "Right", FieldStatus::Supported, "b")],
            ),
            &cols,
        );
        w.merge(
            page(
                "c",
                vec![basis("ceo", "Later", FieldStatus::Supported, "c")],
            ),
            &cols,
        );
        assert_eq!(w.found["ceo"].value, Some(json!("Right")));
        assert_eq!(w.usage.pages, vec!["a", "b", "c"]);
        assert!(w.missing(&cols).is_empty());
    }

    #[test]
    fn unsupported_never_fills_a_cell_and_becomes_not_found() {
        let ceo = col("ceo", ColumnType::Text);
        let mut w = RowWork::default();
        w.merge(
            page(
                "a",
                vec![basis("ceo", "Guess", FieldStatus::Unsupported, "a")],
            ),
            &[&ceo],
        );
        let row = Map::new();
        let r = finish_row(0, &row, &[&ceo], w);
        assert_eq!(r.cells["ceo"]["status"], "notFound");
        assert_eq!(r.cells["ceo"]["value"], Value::Null);
        assert_eq!(r.status, RowStatus::Completed);
    }

    #[test]
    fn unreached_cells_are_omitted_and_a_timeout_marks_the_row_incomplete() {
        let ceo = col("ceo", ColumnType::Text);
        let hq = col("hq", ColumnType::Text);
        let mut w = RowWork::default();
        w.merge(
            page("a", vec![basis("ceo", "X", FieldStatus::Supported, "a")]),
            &[&ceo],
        );
        w.timed_out = true;
        let r = finish_row(0, &Map::new(), &[&ceo, &hq], w);
        assert_eq!(r.status, RowStatus::Incomplete);
        assert_eq!(r.stop_reason, Some("timeout"));
        assert!(r.cells.contains_key("ceo"));
        assert!(!r.cells.contains_key("hq"));
    }

    fn snippet_basis(field: &str, value: &str, status: FieldStatus, url: &str) -> Basis {
        let mut b = basis(field, value, status, url);
        b.citations[0].source_text_kind = SNIPPET_TEXT_KIND.into();
        b.citations[0].excerpt = Some(value.into());
        b
    }

    #[test]
    fn a_snippet_alone_never_makes_a_cell_supported() {
        let ceo = col("ceo", ColumnType::Text);
        let mut b = snippet_basis(
            "ceo",
            "Jane Doe",
            FieldStatus::Supported,
            "https://news.example",
        );
        settle_cell(&mut b);
        assert_eq!(b.status, FieldStatus::Unverified);
        assert!(
            b.citations[0].excerpt.is_none(),
            "excerpt only on supported"
        );
        let mut w = RowWork::default();
        w.merge(
            StepOutcome {
                pages: vec![],
                basis: vec![b],
                usage: None,
            },
            &[&ceo],
        );
        // Still missing, so a later step looks for a page that states it.
        assert_eq!(w.missing(&[&ceo]).len(), 1);
        // No page read, yet an extraction ran: the row completed, nothing billed as a page.
        let r = finish_row(0, &Map::new(), &[&ceo], w);
        assert_eq!(r.status, RowStatus::Completed);
        assert!(r.usage.pages.is_empty());
        assert_eq!(r.cells["ceo"]["status"], "unverified");
    }

    #[test]
    fn a_page_backed_guess_replaces_a_snippet_backed_one() {
        let ceo = col("ceo", ColumnType::Text);
        let mut w = RowWork::default();
        let mut s = snippet_basis(
            "ceo",
            "Old",
            FieldStatus::Unverified,
            "https://news.example",
        );
        s.citations[0].excerpt = None;
        w.merge(page("a", vec![s]), &[&ceo]);
        w.merge(
            page("b", vec![basis("ceo", "New", FieldStatus::Unverified, "b")]),
            &[&ceo],
        );
        assert_eq!(w.found["ceo"].value, Some(json!("New")));
        // ...but one page guess never displaces another.
        w.merge(
            page(
                "c",
                vec![basis("ceo", "Later", FieldStatus::Unverified, "c")],
            ),
            &[&ceo],
        );
        assert_eq!(w.found["ceo"].value, Some(json!("New")));
    }

    #[test]
    fn an_empty_string_is_no_value_but_zero_is() {
        let mut e = basis("ceo", " ", FieldStatus::Unverified, "a");
        settle_cell(&mut e);
        assert_eq!(e.status, FieldStatus::NotFound);
        let mut z = basis("n", "x", FieldStatus::Unverified, "a");
        z.value = Some(json!(0));
        settle_cell(&mut z);
        assert_eq!(z.status, FieldStatus::Unverified);
    }

    #[test]
    fn sources_must_name_the_row_entity() {
        let cols = vec![col("ceo", ColumnType::Text)];
        let row = |v: Value| -> Map<String, Value> { serde_json::from_value(v).unwrap() };
        let t = |r: &Map<String, Value>| {
            entity_name(r, &target_columns(&cols, r, false)).map(str::to_string)
        };
        // A name-like key wins over a column that sorts first.
        let r = row(json!({ "id": 1, "industry": "fintech", "name": "Stripe", "ceo": null }));
        assert_eq!(t(&r).as_deref(), Some("Stripe"));
        let r = row(json!({ "city": "Berlin", "company": "Acme", "site": "acme.com" }));
        assert_eq!(t(&r).as_deref(), Some("Acme"));
        // One plain text value is the name; two unknown ones mean no filter.
        assert_eq!(
            t(&row(json!({ "firm": "Klarna" }))).as_deref(),
            Some("Klarna")
        );
        assert_eq!(t(&row(json!({ "a": "x y", "b": "z" }))), None);
        assert_eq!(t(&row(json!({ "name": "!!!" }))), None);

        assert!(mentions("Patrick Collison, CEO of Stripe, Inc.", "Stripe"));
        assert!(mentions("Stripe raised again", "Stripe, Inc."));
        assert!(!mentions("Stripes and stars", "Stripe"));
        assert!(mentions(
            "Investor relations | MercadoLibre",
            "Mercado Libre"
        ));
        assert!(mentions("Campus of ETH Zurich", "ETH Zürich"));
        assert!(mentions("メルカリは日本の会社です", "メルカリ"));
        assert!(mentions("Tokio is an async runtime", "tokio-rs/tokio"));
        assert!(!mentions("A page about something else", "tokio-rs/tokio"));
        assert!(skip_host("https://www.youtube.com/watch?v=1"));
        assert!(skip_host("https://m.facebook.com/stripe"));
        assert!(!skip_host("https://www.linkedin.com/company/stripe"));
        assert!(!skip_host("https://notyoutube.com/"));
        assert!(skip_host(
            "https://play.google.com/store/apps/details?id=com.spotify.music"
        ));
        assert!(skip_host(
            "https://apps.apple.com/us/app/spotify-music-and-podcasts/id324684580"
        ));
        // Only the store hosts: Google and Apple's own pages stay readable.
        assert!(!skip_host("https://www.google.com/about/"));
        assert!(!skip_host("https://www.apple.com/leadership/"));
    }

    #[test]
    fn a_row_with_no_readable_page_fails() {
        let ceo = col("ceo", ColumnType::Text);
        let w = RowWork {
            last_error: Some(PageError::Blocked),
            ..Default::default()
        };
        let r = finish_row(0, &Map::new(), &[&ceo], w);
        assert_eq!(r.status, RowStatus::Failed);
        assert_eq!(r.error.as_deref(), Some(PageError::Blocked.message()));
    }

    #[test]
    fn options_map_to_labels_or_become_not_found() {
        let stage = col("stage", ColumnType::Options);
        let mut b = vec![
            basis("stage", "series-b", FieldStatus::Supported, "a"),
            basis("stage", "Pre-IPO", FieldStatus::Supported, "a"),
        ];
        map_options(&mut b, &[&stage]);
        assert_eq!(b[0].value, Some(json!("Series B")));
        assert_eq!(b[1].status, FieldStatus::NotFound);
        assert_eq!(b[1].value, None);
    }

    #[test]
    fn schema_uses_plain_typed_leaves_without_enum_or_required() {
        let stage = col("stage", ColumnType::Options);
        let n = col("employees", ColumnType::Number);
        let s = build_schema(&[&stage, &n]);
        assert_eq!(s["properties"]["stage"]["type"], "string");
        assert!(s["properties"]["stage"].get("enum").is_none());
        assert_eq!(s["properties"]["employees"]["type"], "number");
        assert!(s.get("required").is_none());
    }

    #[test]
    fn targets_identity_and_anchor_come_from_the_row() {
        let cols = vec![
            col("ceo", ColumnType::Text),
            col("website", ColumnType::Url),
        ];
        let row: Map<String, Value> = serde_json::from_value(json!({
            "id": 7, "company": "Stripe", "website": "stripe.com", "ceo": null
        }))
        .unwrap();
        let t: Vec<&str> = target_columns(&cols, &row, false)
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(t, vec!["ceo"]);
        let tcols = target_columns(&cols, &row, false);
        let id = identity_values(&row, &tcols);
        assert!(id.contains(&"company: Stripe".to_string()));
        assert!(
            id.iter()
                .all(|v| !v.starts_with("id:") && !v.starts_with("ceo:"))
        );
        assert_eq!(
            anchor_url(&row, &cols).as_deref(),
            Some("https://stripe.com/")
        );
    }

    #[test]
    fn only_real_domains_become_anchors() {
        assert_eq!(as_website("Stripe"), None);
        assert_eq!(as_website("ceo@stripe.com"), None);
        assert_eq!(as_website("Series B"), None);
        assert_eq!(as_website("1.5"), None);
        assert_eq!(
            as_website("https://linear.app/about").as_deref(),
            Some("https://linear.app/about")
        );
    }

    #[test]
    fn validation_rejects_oversized_and_malformed_tables() {
        let mk = |rows: usize, cols: Vec<Column>| EnrichRequest {
            rows: vec![Map::new(); rows],
            columns: cols,
            context: None,
            overwrite: false,
            llm_api_key: None,
            llm_provider: None,
            llm_model: None,
            base_url: None,
        };
        assert!(validate(&mk(1, vec![col("ceo", ColumnType::Text)])).is_ok());
        assert!(validate(&mk(MAX_ROWS + 1, vec![col("ceo", ColumnType::Text)])).is_err());
        assert!(validate(&mk(0, vec![col("ceo", ColumnType::Text)])).is_err());
        assert!(validate(&mk(1, vec![])).is_err());
        assert!(validate(&mk(1, vec![col("id", ColumnType::Text)])).is_err());
        assert!(
            validate(&mk(
                1,
                vec![col("a", ColumnType::Text), col("a", ColumnType::Text)]
            ))
            .is_err()
        );
        let mut empty_opts = col("stage", ColumnType::Options);
        empty_opts.options.clear();
        assert!(validate(&mk(1, vec![empty_opts])).is_err());
        let mut with_base = mk(1, vec![col("ceo", ColumnType::Text)]);
        with_base.base_url = Some("http://x".into());
        assert!(validate(&with_base).is_err());
    }

    #[test]
    fn options_in_any_script_stay_distinct_and_never_match_empty() {
        let mut cities = col("city", ColumnType::Options);
        cities.options = vec!["東京".into(), "北京".into()];
        let mut b = vec![
            basis("city", "北京", FieldStatus::Supported, "a"),
            basis("city", "!!", FieldStatus::Supported, "a"),
        ];
        map_options(&mut b, &[&cities]);
        assert_eq!(b[0].value, Some(json!("北京")));
        assert_eq!(b[1].status, FieldStatus::NotFound);
        assert!(validate_options("x", &["A b".into(), "a-B".into()]).is_err());
        assert!(validate_options("x", &["??".into()]).is_err());
    }

    #[test]
    fn overwrite_keeps_stale_target_values_out_of_the_identity() {
        let cols = vec![col("ceo", ColumnType::Text)];
        let row: Map<String, Value> =
            serde_json::from_value(json!({ "company": "Acme", "ceo": "Old Name" })).unwrap();
        let tcols = target_columns(&cols, &row, true);
        assert_eq!(tcols.len(), 1);
        assert_eq!(
            identity_values(&row, &tcols),
            vec!["company: Acme".to_string()]
        );
    }

    #[test]
    fn one_document_reached_twice_is_billed_once() {
        assert_eq!(
            url_key("https://www.stripe.com/"),
            url_key("http://stripe.com")
        );
        assert_ne!(
            url_key("https://stripe.com/a"),
            url_key("https://stripe.com/b")
        );
        let ceo = col("ceo", ColumnType::Text);
        let mut w = RowWork::default();
        w.merge(page("https://stripe.com/", vec![]), &[&ceo]);
        w.merge(page("https://www.stripe.com", vec![]), &[&ceo]);
        assert_eq!(w.usage.pages.len(), 1);
    }

    #[test]
    fn reserved_and_oversized_inputs_are_refused() {
        let mk = |cols: Vec<Column>, context: Option<String>| EnrichRequest {
            rows: vec![Map::new()],
            columns: cols,
            context,
            overwrite: false,
            llm_api_key: None,
            llm_provider: None,
            llm_model: None,
            base_url: None,
        };
        assert!(validate(&mk(vec![col("basis", ColumnType::Text)], None)).is_err());
        assert!(
            validate(&mk(
                vec![col("ceo", ColumnType::Text)],
                Some("x".repeat(501))
            ))
            .is_err()
        );
        let mut long = col("ceo", ColumnType::Text);
        long.description = Some("x".repeat(501));
        assert!(validate(&mk(vec![long], None)).is_err());
    }

    #[test]
    fn queries_name_the_entity_then_one_column_in_plain_words() {
        let id = vec!["company: Stripe".to_string()];
        let ctx = Some("Technology companies");
        assert_eq!(search_query(&id, ctx, None), "Stripe Technology companies");

        // No raw camelCase key ever reaches the search engine.
        let hq = col("hqCity", ColumnType::Text);
        assert_eq!(
            search_query(&id, ctx, Some(&hq)),
            "Stripe Technology companies hq city"
        );
        let mut stage = col("stage", ColumnType::Options);
        stage.description = Some("Latest funding stage or status".into());
        assert_eq!(
            search_query(&id, None, Some(&stage)),
            "Stripe Latest funding stage or status"
        );

        assert_eq!(column_words("firstReleaseYear"), "first release year");
        assert_eq!(column_words("pricing_page"), "pricing page");
        assert_eq!(column_words(" CEO "), "ceo");
        assert_eq!(
            column_words("cheapestPaidUsdMonthly"),
            "cheapest paid usd monthly"
        );
    }

    #[test]
    fn number_columns_ask_for_plain_digits() {
        let n = col("employees", ColumnType::Number);
        let s = build_schema(&[&n]);
        assert!(
            s["properties"]["employees"]["description"]
                .as_str()
                .unwrap()
                .contains("digits only")
        );
    }
}
