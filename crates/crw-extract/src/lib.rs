//! HTML content extraction and format conversion for the CRW web scraper.
//!
//! Converts raw HTML into clean, structured output formats:
//!
//! - **Markdown** — via [`markdown::html_to_markdown`] (htmd)
//! - **Plain text** — via [`plaintext::html_to_plaintext`]
//! - **Cleaned HTML** — boilerplate removal with [`clean::clean_html`]
//! - **Readability** — main-content extraction with text-density scoring
//! - **CSS/XPath selector** — narrow content to a specific element
//! - **Chunking** — split content into sentence/topic/regex chunks
//! - **Filtering** — BM25 or cosine-similarity ranking of chunks
//! - **Structured JSON** — LLM-based extraction with JSON Schema validation

pub mod antibot;
mod basis;
pub mod chunking;
pub mod clean;
pub mod dom_features;
pub mod dom_util;
pub mod filter;
pub mod judge;
pub mod markdown;
pub mod pdf;
pub mod plaintext;
pub mod quality;
pub mod readability;
mod responses;
pub mod selector;
pub mod structured;
pub mod table_normalize;
pub mod tables;
pub mod untrusted;

use crw_core::error::{CrwError, CrwResult};
use crw_core::types::{
    CapturedNetworkResponse, ChunkResult, ChunkStrategy, DebugAttempt, DebugCandidate,
    DebugExtraction, FilterMode, OutputFormat, PageMetadata, RenderDecision, ScrapeData,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Per-request collector for extraction debug traces. Wired in through
/// [`ExtractOptions::debug_sink`]; the extractor pushes one
/// [`DebugAttempt`] per `extract()` invocation, capturing the candidate
/// ladder and the chosen output. Wrapped in an `Arc<Mutex<_>>` so the
/// renderer / multi-attempt loop in `crw-crawl` can share a single sink
/// across the JS-escalation retry.
#[derive(Debug, Default)]
pub struct DebugCollector {
    attempts: Vec<DebugAttempt>,
}

impl DebugCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_attempt(&mut self, attempt: DebugAttempt) {
        self.attempts.push(attempt);
    }

    pub fn into_extraction(self) -> DebugExtraction {
        DebugExtraction {
            attempts: self.attempts,
        }
    }
}

/// Convenience: lift a single candidate description into a
/// [`DebugCandidate`].
pub fn debug_candidate(
    kind: impl Into<String>,
    text: Option<String>,
    score: f64,
    cap_chars: Option<usize>,
) -> DebugCandidate {
    let text_excerpt = text.as_ref().map(|s| {
        let mut idx = 200.min(s.len());
        while idx > 0 && !s.is_char_boundary(idx) {
            idx -= 1;
        }
        s[..idx].to_string()
    });
    DebugCandidate {
        kind: kind.into(),
        text,
        text_excerpt,
        cap_chars,
        score,
    }
}

pub mod answer;
pub mod llm;
pub mod llm_gate;
pub mod pricing;
pub mod summary;

/// Options for the high-level extraction pipeline.
pub struct ExtractOptions<'a> {
    pub raw_html: &'a str,
    /// Declared response content type, e.g. `Some("text/plain")`. `None` when
    /// unknown. Used to decide whether `raw_html` is safe to run through an
    /// HTML parser / HTML-to-markdown converter at all — see
    /// `crw_core::is_html_like_content_type`.
    pub content_type: Option<&'a str>,
    pub source_url: &'a str,
    pub status_code: u16,
    pub rendered_with: Option<String>,
    pub elapsed_ms: u64,
    /// Routing decision metadata to surface to API consumers.
    pub render_decision: Option<RenderDecision>,
    /// Credit cost attributed to this fetch.
    pub credit_cost: u32,
    /// Soft-failure warnings collected through the render chain.
    pub warnings: Vec<String>,
    pub formats: &'a [OutputFormat],
    pub only_main_content: bool,
    pub include_tags: &'a [String],
    pub exclude_tags: &'a [String],
    /// CSS selector to narrow content before readability extraction.
    pub css_selector: Option<&'a str>,
    /// XPath expression to narrow content before readability extraction.
    pub xpath: Option<&'a str>,
    /// Strategy for chunking the extracted markdown.
    pub chunk_strategy: Option<&'a ChunkStrategy>,
    /// Query for chunk filtering (requires filter_mode).
    pub query: Option<&'a str>,
    /// Filtering algorithm for chunk ranking.
    pub filter_mode: Option<&'a FilterMode>,
    /// Number of top chunks to return (default: 5).
    pub top_k: Option<usize>,
    /// Per-host CSS selector overrides. Used only when the request did not
    /// supply an explicit `css_selector` / `xpath`. The selector for the
    /// source URL's host is applied before readability narrowing.
    pub domain_selectors: Option<&'a HashMap<String, String>>,
    /// XHR/fetch responses captured during navigation. Used as a fallback
    /// content source when DOM-based extraction is low quality.
    pub captured_responses: &'a [CapturedNetworkResponse],
    /// LLM-assisted extraction fallback configuration. When the chosen
    /// candidate's quality score is below `quality_threshold` and `enable`
    /// is true, the raw HTML (truncated to `max_html_bytes`) is sent to the
    /// configured LLM provider for re-extraction.
    pub llm_fallback: Option<LlmFallbackParams<'a>>,
    /// Opt-in extraction debug trace. When true, the extractor populates
    /// `debug_sink` with one [`DebugAttempt`] per `extract()` invocation.
    pub debug: bool,
    /// Sink for debug attempts. Shared across the multi-attempt
    /// JS-escalation loop so that all attempts land in one trace.
    pub debug_sink: Option<Arc<Mutex<DebugCollector>>>,
    /// Expand table `rowspan`/`colspan` into a flat grid before markdown
    /// conversion. From `ExtractionConfig::normalize_tables`; ships false.
    pub normalize_tables: bool,
}

/// Owned counterpart to [`ExtractOptions`], used to ship an extraction job
/// across the `'static` `spawn_blocking` boundary — the borrowed
/// [`ExtractOptions`] can't cross it. Build it from the same inputs as a
/// `build_extract_opts` call site, move it onto the blocking pool, then
/// reconstruct the borrowed view inside the closure with [`Self::as_opts`].
///
/// [`ExtractOptions`] and [`extract`] are intentionally left unchanged so every
/// existing borrow-caller keeps compiling; this is purely additive.
pub struct OwnedExtractInput {
    pub raw_html: String,
    pub content_type: Option<String>,
    pub source_url: String,
    pub status_code: u16,
    pub rendered_with: Option<String>,
    pub elapsed_ms: u64,
    pub render_decision: Option<RenderDecision>,
    pub credit_cost: u32,
    pub warnings: Vec<String>,
    pub formats: Vec<OutputFormat>,
    pub only_main_content: bool,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub css_selector: Option<String>,
    pub xpath: Option<String>,
    pub chunk_strategy: Option<ChunkStrategy>,
    pub query: Option<String>,
    pub filter_mode: Option<FilterMode>,
    pub top_k: Option<usize>,
    /// Shared host→selector overrides. Held behind an `Arc` so callers can clone
    /// the (potentially large) map by refcount instead of deep-copying it per
    /// request.
    pub domain_selectors: Option<Arc<HashMap<String, String>>>,
    pub captured_responses: Vec<CapturedNetworkResponse>,
    pub debug: bool,
    pub debug_sink: Option<Arc<Mutex<DebugCollector>>>,
    pub normalize_tables: bool,
}

impl OwnedExtractInput {
    /// Reconstruct the borrowed [`ExtractOptions`] view over this owned input.
    ///
    /// `llm_fallback` is always `None`: [`extract`] ignores that field (the LLM
    /// fallback runs as a separate async stage in `crw-crawl`), and no offloaded
    /// caller sets it — so it does not need to survive the offload boundary.
    pub fn as_opts(&self) -> ExtractOptions<'_> {
        ExtractOptions {
            raw_html: &self.raw_html,
            content_type: self.content_type.as_deref(),
            source_url: &self.source_url,
            status_code: self.status_code,
            rendered_with: self.rendered_with.clone(),
            elapsed_ms: self.elapsed_ms,
            render_decision: self.render_decision.clone(),
            credit_cost: self.credit_cost,
            warnings: self.warnings.clone(),
            formats: &self.formats,
            only_main_content: self.only_main_content,
            include_tags: &self.include_tags,
            exclude_tags: &self.exclude_tags,
            css_selector: self.css_selector.as_deref(),
            xpath: self.xpath.as_deref(),
            chunk_strategy: self.chunk_strategy.as_ref(),
            query: self.query.as_deref(),
            filter_mode: self.filter_mode.as_ref(),
            top_k: self.top_k,
            domain_selectors: self.domain_selectors.as_deref(),
            captured_responses: &self.captured_responses,
            llm_fallback: None,
            debug: self.debug,
            debug_sink: self.debug_sink.clone(),
            normalize_tables: self.normalize_tables,
        }
    }
}

/// Parameters for the LLM-assisted extraction fallback. See
/// [`LlmFallbackConfig`](crw_core::config::LlmFallbackConfig).
#[derive(Debug, Clone)]
pub struct LlmFallbackParams<'a> {
    pub api_key: &'a str,
    pub model: &'a str,
    pub provider: &'a str,
    pub base_url: Option<&'a str>,
    pub quality_threshold: f32,
    pub max_html_bytes: usize,
    pub max_tokens: u32,
    pub azure_api_version: Option<&'a str>,
    /// When true, run the LLM regardless of DOM-extraction quality
    /// ("primary extractor" mode); when false, only run as a fallback for
    /// candidates scoring below `quality_threshold`.
    pub always_run: bool,
}

/// Re-extract via the configured LLM provider when the current markdown
/// scores below `params.quality_threshold`. If the LLM result has a higher
/// quality score, it replaces `data.markdown` in place and a warning is
/// appended noting the swap. On any failure (network, auth, parse) the
/// original markdown is preserved and the error is logged.
pub async fn maybe_run_llm_fallback(
    data: &mut ScrapeData,
    raw_html: &str,
    params: &LlmFallbackParams<'_>,
) -> CrwResult<()> {
    let current_md = match data.markdown.as_deref() {
        Some(m) if !m.trim().is_empty() => m,
        _ => "",
    };
    let current_quality = quality::analyze_md_only(current_md);
    if !params.always_run && current_quality.score >= params.quality_threshold {
        return Ok(());
    }
    match llm::extract_via_llm(
        raw_html,
        params.api_key,
        params.provider,
        params.model,
        params.base_url,
        params.max_tokens,
        params.max_html_bytes,
        params.azure_api_version,
    )
    .await
    {
        Ok(llm_md) => {
            let llm_quality = quality::analyze_md_only(&llm_md);
            if llm_quality.score > current_quality.score {
                tracing::info!(
                    prior_score = current_quality.score,
                    llm_score = llm_quality.score,
                    "LLM fallback produced higher-quality markdown"
                );
                data.markdown = Some(llm_md);
                data.warnings.push("extracted_via=llm".to_string());
            } else {
                tracing::debug!(
                    prior_score = current_quality.score,
                    llm_score = llm_quality.score,
                    "LLM fallback produced lower-quality markdown; keeping original"
                );
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "LLM fallback call failed; keeping DOM extraction");
        }
    }
    Ok(())
}

/// Look up the host-specific CSS selector override for a URL.
fn lookup_domain_selector(source_url: &str, map: &HashMap<String, String>) -> Option<String> {
    if map.is_empty() {
        return None;
    }
    let host = url::Url::parse(source_url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))?;
    map.get(&host).cloned()
}

/// Decode the small set of HTML entities that commonly appear in `<meta>`
/// `content` attributes. We don't pull in a full entity decoder because the
/// metadata path only sees author-curated short text, and the long tail of
/// `&amp_lt_named_;` references is empty in practice.
fn decode_basic_html_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices();
    while let Some((i, ch)) = chars.next() {
        if ch != '&' {
            out.push(ch);
            continue;
        }
        let rest = &s[i..];
        let replacement: Option<(&str, &str)> = [
            ("&amp;", "&"),
            ("&lt;", "<"),
            ("&gt;", ">"),
            ("&quot;", "\""),
            ("&apos;", "'"),
            ("&#39;", "'"),
            ("&nbsp;", " "),
            ("&hellip;", "…"),
            ("&mdash;", "—"),
            ("&ndash;", "–"),
            ("&rsquo;", "\u{2019}"),
            ("&lsquo;", "\u{2018}"),
            ("&rdquo;", "\u{201D}"),
            ("&ldquo;", "\u{201C}"),
        ]
        .into_iter()
        .find(|(needle, _)| rest.starts_with(needle));
        if let Some((needle, value)) = replacement {
            out.push_str(value);
            for _ in 0..(needle.len() - 1) {
                chars.next();
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Collapse blank-line splits inside an inline punctuation-separated link list.
///
/// Pages like `dnb.com/business-directory/...` emit each `<a>` for an
/// industry tag inside its own block-level wrapper, so htmd serialises
/// `Industry:\u{a0}<a>X</a>, <a>Y</a>, Z` as
/// `Industry:\u{a0}\n\n[X],\n\n[Y],\n\nZ`. Substring matching against
/// `"Industry: X, Y, Z"` then fails over the embedded blank lines. The
/// rendered page (and our plainText output) keeps the items inline; only
/// the markdown emitter breaks them up. We undo that locally without
/// changing real paragraph structure: NBSP → space (markdown has no
/// NBSP semantics), then collapse exactly the blank-lines that sit
/// between trailing punctuation (`,`, `:`, `)`) and the next inline link
/// (`[`) or a continuing list-item word.
fn reflow_inline_lists(s: String) -> String {
    if !s.contains('\u{00a0}') && !s.contains(",\n\n") && !s.contains(":\n\n") {
        return s;
    }
    let mut t = s.replace('\u{00a0}', " ");
    // ":<spaces>?\n+<spaces>?[" → ": ["
    t = INLINE_LINK_AFTER_PUNCT.replace_all(&t, "$p [").into_owned();
    // "),<spaces>?\n+<spaces>?[" → "), ["
    t = INLINE_LINK_AFTER_CLOSE.replace_all(&t, "), [").into_owned();
    // ",<spaces>?\n+<spaces>?<letter>" → ", <letter>" (trailing list item that
    // isn't itself a link, e.g. "[X], [Y], \n\nMarketing consulting services")
    t = TRAILING_LIST_ITEM.replace_all(&t, ", $w").into_owned();
    t
}

static INLINE_LINK_AFTER_PUNCT: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"(?P<p>[,:])[ \t]*\n[\s]*\[").expect("inline-link regex compiles")
    });
static INLINE_LINK_AFTER_CLOSE: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"\),[ \t]*\n[\s]*\[").expect("inline-link close regex compiles")
    });
static TRAILING_LIST_ITEM: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(r",[ \t]*\n\n+(?P<w>[A-Za-z\u{00C0}-\u{FFFF}])")
        .expect("trailing list-item regex compiles")
});

/// High-level extraction: given raw HTML + options, produce ScrapeData.
pub fn extract(opts: ExtractOptions<'_>) -> CrwResult<ScrapeData> {
    let ExtractOptions {
        raw_html,
        content_type,
        source_url,
        status_code,
        rendered_with,
        elapsed_ms,
        render_decision,
        credit_cost,
        mut warnings,
        formats,
        only_main_content,
        include_tags,
        exclude_tags,
        css_selector,
        xpath,
        chunk_strategy,
        query,
        filter_mode,
        top_k,
        domain_selectors,
        captured_responses,
        llm_fallback: _,
        debug: _,
        debug_sink: _,
        normalize_tables,
    } = opts;

    // Per-host fallback selector — used only when the caller didn't pass an
    // explicit css_selector / xpath. User input always wins over host defaults.
    // Track whether the *caller* opted into a narrow extraction; downstream
    // metadata injection (title prepend) keys off this, not the merged value,
    // so a domain-config default doesn't suppress the title fallback.
    let user_selected = css_selector.is_some() || xpath.is_some();
    let domain_selector_owned: Option<String> =
        if !user_selected && let Some(map) = domain_selectors {
            lookup_domain_selector(source_url, map)
        } else {
            None
        };
    let css_selector = css_selector.or(domain_selector_owned.as_deref());

    // A declared non-HTML content type (text/plain, application/json, ...)
    // means `raw_html` is NOT markup — it is the literal response body. The
    // HTTP tier decodes every non-PDF response through this same pipeline
    // regardless of its declared type, so running html5ever + the HTML-to-
    // markdown converter over it corrupts the body: htmd escapes any
    // markdown-metacharacter byte (backticks, `#`, `*`, ...) it finds in a
    // "text node" and HTML's whitespace-collapse rules merge newlines into
    // spaces, destroying fenced code blocks and paragraph structure in a way
    // no response-side repair can undo (crw#530). Skip the HTML pipeline
    // entirely and treat the body as already-final text for every format.
    let treat_as_plain = !crw_core::is_html_like_content_type(content_type);

    // Step 1: Extract metadata from raw HTML.
    let meta = readability::extract_metadata(raw_html);

    // Steps 2-4 (clean / selector / readability narrowing) only make sense on
    // real markup. For a non-HTML body, `raw_html` already IS the content —
    // none of the markup-aware selection below has anything to select against.
    let (content_html, cleaned_ref, had_selection, include_tags_no_match) = if treat_as_plain {
        (raw_html.to_string(), None, false, false)
    } else {
        // Step 2: Clean HTML (remove boilerplate, nav, ads, etc.).
        // `include_tags` narrowing happens inside clean_html; when it matches
        // nothing, clean_html empties the output and pushes `selector_no_match`.
        // Capture that here (via the warning delta this call produced) so the rest
        // of the pipeline can treat it exactly like an unmatched css/xpath selector.
        let warns_before = warnings.len();
        let cleaned = clean::clean_html_with_warnings(
            raw_html,
            only_main_content,
            include_tags,
            exclude_tags,
            &mut warnings,
        )
        .unwrap_or_else(|_| raw_html.to_string());
        let include_tags_no_match = warnings[warns_before..]
            .iter()
            .any(|w| w == "selector_no_match");

        // Step 3: Apply CSS/XPath selector if provided (narrows to a specific element).
        let selected_html = apply_selector(&cleaned, css_selector, xpath)?;
        // A user-requested narrowing that matched nothing must NOT fall back to the
        // whole page (a silent context-bloat footgun: an unmatched `--css`/`--xpath`
        // or `includeTags` previously returned the entire document). Treat it as an
        // intentional narrow extraction that yielded empty output. `Some("")` also
        // short-circuits the alternates ladder below, so the whole page can't sneak
        // back in via the basic_clean fallback.
        // Domain-configured default selectors are excluded: a host default that
        // doesn't apply should still show the page (its no-match sets neither flag).
        let selected_html = if include_tags_no_match {
            // include_tags matched nothing, so `cleaned` is already empty. This
            // takes precedence over any css/xpath applied afterwards: an xpath
            // scalar like `count(//x)` would otherwise evaluate to "0" against the
            // empty document and leak a bogus value. clean_html already pushed the
            // `selector_no_match` warning, so don't push it again.
            Some(String::new())
        } else if user_selected && selected_html.is_none() {
            warnings.push("selector_no_match".to_string());
            Some(String::new())
        } else {
            selected_html
        };
        let after_selection = selected_html.as_deref().unwrap_or(&cleaned);

        // Step 4: If only_main_content, try to narrow further with readability scoring.
        let (content_html, cleaned_ref) = if only_main_content && selected_html.is_none() {
            match readability::extract_main_content_with_provenance(after_selection) {
                readability::ReadabilityOutcome::Selected { html: main, .. } => {
                    // Re-clean: readability may have selected a broad container
                    // (e.g. <article>) that still contains noise elements
                    // (infobox, navbox, catlinks, etc.).
                    let re_cleaned = clean::clean_html(&main, true, &[], &[]).unwrap_or(main);
                    (re_cleaned, Some(cleaned))
                }
                readability::ReadabilityOutcome::Rejected { .. } => {
                    // Listing root or empty body — skip readability and let the
                    // alternates ladder pick from cleaned / basic-clean.
                    (cleaned.clone(), Some(cleaned))
                }
            }
        } else {
            (after_selection.to_string(), None)
        };
        (
            content_html,
            cleaned_ref,
            selected_html.is_some(),
            include_tags_no_match,
        )
    };

    // Step 5: Produce requested formats. `Summary` also needs markdown
    // internally — the summary path feeds the markdown into the LLM and then
    // strips it from the response unless the caller also asked for markdown.
    let md = if formats.contains(&OutputFormat::Markdown)
        || formats.contains(&OutputFormat::Json)
        || formats.contains(&OutputFormat::Summary)
    {
        if treat_as_plain {
            // Not HTML — `content_html` (== raw_html) is already the final
            // body. Do not run it through an HTML-to-markdown converter.
            Some(content_html.clone())
        } else {
            let primary_md = markdown::html_to_markdown_with(&content_html, normalize_tables);
            let primary_quality = quality::analyze_md_only(&primary_md);

            // Skip alternates when a selector was explicitly used (short output is
            // intentional) or when the primary extraction is healthy.
            // Threshold 0.4 (not 0.6) — readability output that scores 0.4+ is
            // good enough; running alternates on it tends to swap in basic_clean
            // (whole-body) which boosts word count but reintroduces nav noise.
            if had_selection || primary_quality.score > 0.4 {
                Some(primary_md)
            } else {
                let mut candidates: Vec<(&'static str, String, quality::Quality)> = Vec::new();

                // Alt 1: cleaned HTML (only_main_content path bypasses readability).
                if only_main_content && let Some(c) = cleaned_ref.as_ref() {
                    let m = markdown::html_to_markdown_with(c, normalize_tables);
                    let q = quality::analyze_md_only(&m);
                    candidates.push(("cleaned", m, q));
                }

                // Alt 2: whole-page clean without only_main_content, i.e. the page
                // with its nav/footer intact. It is the rescue candidate for pages
                // where readability narrows onto the wrong container.
                //
                // Known wart: because the quality score rewards word count, that
                // boilerplate bulk is also what lets this candidate win on
                // page-builder sites, which is how a mega menu reaches the markdown
                // even for onlyMainContent requests. Making it honour
                // only_main_content fixes those pages but costs recall on the
                // frozen 1000-URL set (0.3828 -> 0.3651), so it is left alone here.
                let basic_cleaned = clean::clean_html_with_warnings(
                    raw_html,
                    false,
                    include_tags,
                    exclude_tags,
                    &mut warnings,
                )
                .unwrap_or_else(|_| raw_html.to_string());
                let basic_md = markdown::html_to_markdown_with(&basic_cleaned, normalize_tables);
                let basic_q = quality::analyze_md_only(&basic_md);
                candidates.push(("basic_clean", basic_md, basic_q));

                // No structural table/list alternate. It harvested <ul>/<table>
                // straight out of raw_html, and its only guard was an ancestor tag
                // check for nav/footer/header — which page builders sail past,
                // since Elementor and friends render menus as plain <div><ul>. On
                // an Elementor product page it was the winning candidate and put
                // 118 nav links into the markdown. Measured contribution across a
                // labelled corpus: none (identical recall with and without).
                // Alt 4: XHR/fetch JSON capture — recursively walk every captured
                // JSON body and gather long text fields. Useful when the article
                // body lives in an API response loaded after `loadEventFired`
                // (newsroom feeds, infinite-scroll, paywall-shielded prose).
                if let Some(xhr_md) = extract_xhr_text(captured_responses) {
                    let q = quality::analyze_md_only(&xhr_md);
                    candidates.push(("xhr_json", xhr_md, q));
                }

                // Alt 5: plaintext fallback.
                let plain_md = {
                    let text = plaintext::html_to_plaintext(&content_html);
                    if text.trim().is_empty() {
                        plaintext::html_to_plaintext(&basic_cleaned)
                    } else {
                        text
                    }
                };
                let plain_q = quality::analyze_md_only(&plain_md);
                candidates.push(("plaintext", plain_md, plain_q));

                // Include the primary at the head of the candidate list.
                candidates.insert(0, ("primary", primary_md, primary_quality));

                // Primary-biased pick: keep primary unless an alternate beats it by
                // a clear margin (0.15). Without this margin, basic_clean tends to
                // win simply by including more nav/footer words, which boosts its
                // word count but reintroduces noise the readability primary had
                // correctly excluded.
                const PRIMARY_MARGIN: f32 = 0.15;
                let primary_score = candidates[0].2.score;
                let chosen_idx = candidates
                    .iter()
                    .enumerate()
                    .skip(1)
                    .filter(|(_, c)| c.2.score >= primary_score + PRIMARY_MARGIN)
                    .max_by(|(_, a), (_, b)| {
                        a.2.score
                            .partial_cmp(&b.2.score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then(a.2.bytes.cmp(&b.2.bytes))
                    })
                    .map(|(i, _)| i)
                    .unwrap_or(0);

                let names: Vec<&'static str> = candidates.iter().map(|c| c.0).collect();
                let scores: Vec<f32> = candidates.iter().map(|c| c.2.score).collect();
                let chosen_name = candidates[chosen_idx].0;
                tracing::debug!(
                    strategies = ?names,
                    scores = ?scores,
                    chosen = %chosen_name,
                    "quality-selected markdown extraction"
                );

                Some(candidates.swap_remove(chosen_idx).1)
            }
        }
    } else {
        None
    };

    // Collapse the repeated navigation a responsive template ships — a mobile
    // copy, a desktop copy, one per dropdown. Applied after the candidate is
    // chosen, so scoring (and therefore which candidate wins) is unaffected.
    // Skipped without `onlyMainContent`: there the caller asked for the
    // document as it is.
    let md = md.map(|m| {
        // Not HTML: there is no nav/menu structure to dedupe, and any
        // heuristic text match here would mutate the caller's exact bytes.
        if treat_as_plain {
            return m;
        }
        if only_main_content {
            let m = markdown::drop_repeated_nav_lines(&m);
            // `drop_repeated_nav_lines` only catches a menu that a responsive
            // template rendered more than once. A menu rendered once survives
            // it, and the class-name rules in `clean` miss it whenever the site
            // does not name the container (`nav`, `sidebar`, …). What is left
            // is a run of short lines that are nothing but a link, which is the
            // same shape in every language. Drop those too, unless too little
            // text would survive — then the links are the page.
            //
            // Never when the caller narrowed the page themselves: asking for
            // `includeTags: ["nav"]` and getting the nav removed would be
            // absurd, and a `css_selector` means they already said what they
            // want.
            if user_selected || !include_tags.is_empty() {
                m
            } else {
                quality::strip_nav_lines(&m).unwrap_or(m)
            }
        } else {
            m
        }
    });

    // News/blog templates frequently render the article H1 inside a `<header>`
    // sibling of the scored container, so readability drops it. Prepend the
    // metadata title (preferring the cleaner og:title) when it isn't already
    // present in the markdown — otherwise downstream recall scoring loses the
    // most important phrase on the page (the title itself).
    let md = md.map(|m| {
        // Not HTML: there is no real `<title>`/`og:title` to prepend — any
        // match `extract_metadata` found is a false positive off a stray
        // HTML-looking snippet inside the plain-text body itself.
        if treat_as_plain {
            return m;
        }
        // A narrowing that matched nothing is intentionally empty; padding it
        // with the page title would re-leak content the caller narrowed away.
        if user_selected || include_tags_no_match {
            return m;
        }
        let title = meta
            .og_title
            .as_deref()
            .or(meta.title.as_deref())
            .map(str::trim)
            .filter(|t| !t.is_empty());
        let Some(title) = title else { return m };
        // <title> commonly carries " | Site Name", " – Site Name", " — Site Name",
        // or " - Site Name" suffix; og:title is usually clean, but strip
        // defensively in either case. Pipe is rare inside real titles so we
        // split on the first occurrence (no whitespace required). En/em dash
        // and ASCII hyphen REQUIRE surrounding whitespace — a bare en dash
        // appears inside titles like "Northern Song Dynasty (960–1127)" and
        // must not split there; bare ASCII hyphens are common in compound
        // words. Dash splits are right-anchored so multi-segment titles like
        // "Foo – Bar – Site Name" reduce to "Foo – Bar" rather than "Foo".
        let core = title
            .split('|')
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(title);
        let core = core
            .rsplit_once(" – ")
            .map(|(l, _)| l.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(core);
        let core = core
            .rsplit_once(" — ")
            .map(|(l, _)| l.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(core);
        let core = core
            .rsplit_once(" - ")
            .map(|(l, _)| l.trim())
            .unwrap_or(core);
        if m.contains(core) || m.contains(title) {
            return m;
        }
        format!("# {core}\n\n{m}")
    });

    // When the extracted markdown is unusually short, append the page's
    // meta description / og:description — these are author-curated summaries
    // that frequently contain the article's key phrases, especially on:
    //   - Forum threads where readability picked one comment instead of the
    //     question (Discourse, vBulletin: meta description = first post body)
    //   - Listing pages whose readability rejection drops to a thin
    //     post-fallback ladder
    //   - Login-walled / app-shell pages where the SSR'd description is the
    //     only signal of what the page is about
    // Skip when the caller used a selector (intentional narrowness), the md
    // is already substantial, the description is short or already present,
    // or it duplicates the page title.
    let md = md.map(|m| {
        // Not HTML: same false-positive-metadata reasoning as the title
        // prepend above.
        if treat_as_plain {
            return m;
        }
        if user_selected || include_tags_no_match {
            return m;
        }
        if m.len() >= 1500 {
            return m;
        }
        // Prefer whichever of `<meta name="description">` and
        // `<meta property="og:description">` is longer — Discourse and other
        // forum templates set the two to *different* posts (name=description
        // → original question, og:description → currently-displayed reply),
        // so picking the longer surfaces more unique content. When the two
        // diverge significantly (e.g. forum threads), append both so the
        // markdown captures the question *and* the highlighted reply.
        let name_desc = meta
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty());
        let og_desc = meta
            .og_description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty());
        let combined = match (name_desc, og_desc) {
            (Some(a), Some(b)) if a == b => decode_basic_html_entities(a),
            (Some(a), Some(b)) => {
                let (longer, shorter) = if a.len() >= b.len() { (a, b) } else { (b, a) };
                let l = decode_basic_html_entities(longer);
                let s = decode_basic_html_entities(shorter);
                let probe_len = s.chars().take(60).map(char::len_utf8).sum::<usize>();
                let probe = &s[..probe_len.min(s.len())];
                if l.contains(probe) {
                    l
                } else {
                    format!("{l}\n\n{s}")
                }
            }
            (Some(a), None) | (None, Some(a)) => decode_basic_html_entities(a),
            (None, None) => return m,
        };
        let trimmed = combined.trim();
        // Defend against tagline-only descriptions (~30-50 chars) which add
        // no signal but pollute the leading content. Real article summaries
        // are nearly always >80 chars.
        if trimmed.chars().count() < 80 {
            return m;
        }
        let title_lc = meta
            .og_title
            .as_deref()
            .or(meta.title.as_deref())
            .map(|t| t.trim().to_lowercase())
            .unwrap_or_default();
        if !title_lc.is_empty() && trimmed.to_lowercase() == title_lc {
            return m;
        }
        // Cheap containment check — if the first ~120 chars of the
        // description already appear in the markdown, the body covers it.
        let probe_len = trimmed.chars().take(120).map(char::len_utf8).sum::<usize>();
        let probe = &trimmed[..probe_len.min(trimmed.len())];
        if m.contains(probe) {
            return m;
        }
        format!("{m}\n\n{trimmed}\n")
    });

    // Inline-list reflow: htmd emits each `<a>` inside a `<div>`/`<p>` wrapper as
    // its own paragraph, so a comma-separated label-and-link list (common on
    // company directories like dnb.com — `Industry: <a>X</a>, <a>Y</a>, Z`)
    // becomes `Industry:\u{00a0}\n\n[X], \n\n[Y], \n\nZ`. The runtime
    // `<a>` markup keeps the items inline; the surrounding blank lines are
    // a markdown-emit artefact that breaks substring matching across them.
    // Two passes: 1) NBSP → space (lossless: markdown has no NBSP semantics),
    // 2) collapse a blank line that sits between `, : )` punctuation and the
    // next inline link or comma-continuation paragraph.
    let md = md.map(|m| {
        if treat_as_plain {
            m
        } else {
            reflow_inline_lists(m)
        }
    });

    let plain = if formats.contains(&OutputFormat::PlainText) {
        Some(if treat_as_plain {
            content_html.clone()
        } else {
            plaintext::html_to_plaintext(&content_html)
        })
    } else {
        None
    };

    let raw = if formats.contains(&OutputFormat::RawHtml) {
        Some(raw_html.to_string())
    } else {
        None
    };

    let html = if formats.contains(&OutputFormat::Html) {
        Some(content_html)
    } else {
        None
    };

    let links = if formats.contains(&OutputFormat::Links) {
        Some(readability::extract_links(raw_html, source_url))
    } else {
        None
    };

    let images = if formats.contains(&OutputFormat::Images) {
        Some(readability::extract_images(raw_html, source_url))
    } else {
        None
    };

    // JSON extraction is handled asynchronously in scrape_url after extract() returns.
    let json = None;

    // Warn if filtering params are provided without a chunking strategy.
    let orphan_chunk_warning =
        if chunk_strategy.is_none() && (query.is_some() || filter_mode.is_some()) {
            Some(
                "'query' and 'filterMode' require 'chunkStrategy' to be set. \
             These parameters were ignored."
                    .to_string(),
            )
        } else {
            None
        };

    // Step 6: Chunk the markdown if a strategy is provided.
    let chunks = if let Some(strategy) = chunk_strategy
        && let Some(ref markdown_text) = md
        && !markdown_text.trim().is_empty()
    {
        let raw_chunks = chunking::chunk_text(markdown_text, strategy);

        // Step 7: Filter chunks by relevance if query + filter_mode are set.
        let chunk_results = if let (Some(q), Some(mode)) = (query, filter_mode)
            && !q.trim().is_empty()
            && !raw_chunks.is_empty()
        {
            filter::filter_chunks_scored(&raw_chunks, q, mode, top_k.unwrap_or(5))
                .into_iter()
                .map(|sc| ChunkResult {
                    content: sc.content,
                    score: Some(sc.score),
                    index: sc.index,
                })
                .collect::<Vec<_>>()
        } else {
            let mut results: Vec<_> = raw_chunks
                .into_iter()
                .enumerate()
                .map(|(i, c)| ChunkResult {
                    content: c,
                    score: None,
                    index: i,
                })
                .collect();
            if let Some(k) = top_k {
                results.truncate(k);
            }
            results
        };

        if chunk_results.is_empty() {
            None
        } else {
            Some(chunk_results)
        }
    } else {
        None
    };

    Ok(ScrapeData {
        markdown: md,
        // Set at the scrape choke point (crw-crawl::single::scrape_url) where
        // crw-diff is available; the extractor stays free of crw-diff.
        source_hash: None,
        html,
        raw_html: raw,
        plain_text: plain,
        links,
        images,
        json,
        summary: None,
        llm_usage: None,
        chunks,
        warning: orphan_chunk_warning,
        warnings,
        render_decision,
        credit_cost,
        // Basis rides the structured-extraction path (single.rs), not this
        // extractor; it is stamped there when the request asks for it.
        basis: None,
        basis_warnings: Vec::new(),
        llm_input_hash: None,
        metadata: PageMetadata {
            title: meta.title,
            description: meta.description,
            og_title: meta.og_title,
            og_description: meta.og_description,
            og_image: meta.og_image,
            canonical_url: meta.canonical_url,
            source_url: source_url.to_string(),
            language: meta.language,
            status_code,
            rendered_with,
            elapsed_ms,
            page_count: None,
            source_filename: None,
            extra: meta.extra,
        },
        debug_extraction: None,
        // Populated post-extract by the caller (single.rs / crawl.rs) from
        // FetchResult.content_type; change_tracking + screenshot are set there too.
        content_type: None,
        change_tracking: None,
        screenshot: None,
        // Anti-bot verdict is stamped post-extract at the scrape choke.
        block: None,
        // Copied post-extract from FetchResult.truncated, same as content_type.
        truncated: false,
    })
}

/// Apply CSS selector or XPath to narrow HTML content.
/// Returns None if no selector is set or no match is found.
fn apply_selector(html: &str, css: Option<&str>, xpath: Option<&str>) -> CrwResult<Option<String>> {
    if let Some(sel) = css {
        let result = selector::extract_by_css(html, sel).map_err(CrwError::ExtractionError)?;
        if result.is_some() {
            return Ok(result);
        }
    }
    if let Some(xp) = xpath
        && let Some(texts) =
            selector::extract_by_xpath(html, xp).map_err(CrwError::ExtractionError)?
    {
        let wrapped = texts
            .into_iter()
            .map(|text| {
                let escaped = text
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                format!("<div>{escaped}</div>")
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Ok(Some(wrapped));
    }
    Ok(None)
}

fn extract_xhr_text(captured: &[CapturedNetworkResponse]) -> Option<String> {
    const MIN_FIELD_LEN: usize = 120;
    const MIN_TOTAL_LEN: usize = 400;

    if captured.is_empty() {
        return None;
    }
    let mut paragraphs: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for resp in captured {
        let body = match resp.body.as_deref() {
            Some(b) if !b.is_empty() => b,
            _ => continue,
        };
        let value: serde_json::Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(_) => continue,
        };
        walk_json_strings(&value, &mut |s| {
            if s.len() >= MIN_FIELD_LEN && seen.insert(s.to_string()) {
                paragraphs.push(s.to_string());
            }
        });
    }

    if paragraphs.is_empty() {
        return None;
    }
    let joined = paragraphs.join("\n\n");
    if joined.len() < MIN_TOTAL_LEN {
        return None;
    }
    Some(joined)
}

fn walk_json_strings(value: &serde_json::Value, on_string: &mut dyn FnMut(&str)) {
    match value {
        serde_json::Value::String(s) => {
            // Skip URLs, IDs, dates, and HTML tag fragments — keep prose only.
            let trimmed = s.trim();
            if trimmed.starts_with("http://")
                || trimmed.starts_with("https://")
                || trimmed.starts_with('/')
                || trimmed.starts_with('<')
            {
                return;
            }
            on_string(trimmed);
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                walk_json_strings(v, on_string);
            }
        }
        serde_json::Value::Object(map) => {
            for (_, v) in map {
                walk_json_strings(v, on_string);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod private_tests {
    use super::*;
    use crw_core::types::{CapturedNetworkResponse, OutputFormat};

    #[test]
    fn extract_preserves_linked_pilcrow_in_body_prose() {
        let formats = [OutputFormat::Markdown];
        let data = extract(ExtractOptions {
            raw_html: r##"<p>See <a href="#para12">¶</a> 12 for details.</p>"##,
            content_type: Some("text/html"),
            source_url: "https://example.com/",
            status_code: 200,
            rendered_with: None,
            elapsed_ms: 0,
            render_decision: None,
            credit_cost: 0,
            warnings: Vec::new(),
            formats: &formats,
            only_main_content: true,
            include_tags: &[],
            exclude_tags: &[],
            css_selector: None,
            xpath: None,
            chunk_strategy: None,
            query: None,
            filter_mode: None,
            top_k: None,
            domain_selectors: None,
            captured_responses: &[],
            llm_fallback: None,
            debug: false,
            debug_sink: None,
            normalize_tables: false,
        })
        .unwrap();

        assert!(
            data.markdown
                .as_deref()
                .is_some_and(|md| md.contains("See [¶](#para12) 12 for details."))
        );
    }

    #[test]
    fn domain_selector_matches_exact_host() {
        let mut map = HashMap::new();
        map.insert("news.example.com".to_string(), ".article".to_string());
        let got = lookup_domain_selector("https://news.example.com/p/42", &map);
        assert_eq!(got.as_deref(), Some(".article"));
    }

    #[test]
    fn domain_selector_misses_on_other_host() {
        let mut map = HashMap::new();
        map.insert("news.example.com".to_string(), ".article".to_string());
        let got = lookup_domain_selector("https://other.example.com/p/42", &map);
        assert!(got.is_none());
    }

    #[test]
    fn domain_selector_empty_map_returns_none() {
        let map = HashMap::new();
        assert!(lookup_domain_selector("https://x.example.com/", &map).is_none());
    }

    #[test]
    fn xhr_extract_returns_none_for_empty_input() {
        assert!(extract_xhr_text(&[]).is_none());
    }

    #[test]
    fn xhr_extract_collects_long_string_fields() {
        let body = serde_json::json!({
            "title": "short",
            "body": "a".repeat(300),
            "meta": { "summary": "b".repeat(200) },
            "tags": ["c".repeat(150), "short"],
            "url": "https://example.com/should/skip",
        })
        .to_string();
        let resp = vec![CapturedNetworkResponse {
            url: "https://api.example.com/article/1".to_string(),
            request_id: "1".to_string(),
            status: 200,
            mime_type: Some("application/json".to_string()),
            body: Some(body),
            body_size_bytes: 800,
        }];
        let got = extract_xhr_text(&resp).expect("expected long-text fields");
        assert!(got.contains(&"a".repeat(300)));
        assert!(got.contains(&"b".repeat(200)));
        assert!(got.contains(&"c".repeat(150)));
        assert!(!got.contains("short"));
        assert!(!got.contains("example.com/should/skip"));
    }

    #[test]
    fn xhr_extract_skips_invalid_json() {
        let resp = vec![CapturedNetworkResponse {
            url: "x".into(),
            request_id: "1".into(),
            status: 200,
            mime_type: Some("application/json".into()),
            body: Some("not json".into()),
            body_size_bytes: 8,
        }];
        assert!(extract_xhr_text(&resp).is_none());
    }
}
