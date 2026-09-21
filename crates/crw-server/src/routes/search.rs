use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::HeaderMap;
use crw_core::Deadline;
use crw_core::config::LlmConfig;
use crw_core::error::CrwError;
use crw_core::types::{
    ApiResponse, LlmUsage, OutputFormat, ScrapeData, ScrapeRequest, SearchData, SearchRequest,
    SearchResponse, SearchResponseData, SearchResult, SearchScrapeOptions,
};
use crw_crawl::single::scrape_url;
use crw_extract::answer;
use crw_extract::summary;
use crw_search::{
    SearchError, SearxngClient, SearxngParams, SearxngResponse, map_to_searxng_params,
    transform_flat, transform_flat_reranked, transform_grouped,
};
use futures::stream::{self, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::task::JoinSet;

const DEFAULT_ANSWER_TOP_N: u32 = 5;
/// Default top-N for the calibrated answer path (feeds more sources so the
/// answer in result #6-8, or behind a failed top-5 scrape, still reaches the
/// model). Bounded by `MAX_ANSWER_TOP_N`.
const CALIBRATED_ANSWER_TOP_N: u32 = 8;
const MAX_ANSWER_TOP_N: u32 = 10;
/// Upper bound on query-expansion rewrites fetched + unioned per request, so a
/// request-supplied `query_expand_variants` can't fan out unbounded SearXNG
/// fetches. The extra pools are fetched concurrently in `fetch_expanded`.
const MAX_QUERY_EXPAND_VARIANTS: usize = 5;
/// Adaptive multi-round: max follow-up queries the evidence-scout issues in the
/// extra round, and how many results each contributes to the scrape pool.
/// Bounded so the single extra round stays well within the request deadline.
const MAX_SCOUT_QUERIES: usize = 2;
const SCOUT_FETCH_LIMIT: u32 = 6;
/// Minimum request-deadline budget that must remain before the adaptive
/// multi-round scout is allowed to start its extra round (scout LLM + up to
/// `MAX_SCOUT_QUERIES` fetches + scrapes + one re-synthesis). Below this, an
/// abstaining query returns round-1 immediately instead of risking a deadline
/// overrun (504) — this is what bounds the worst-case latency that enabling
/// `multi_round` adds.
const MULTI_ROUND_MIN_BUDGET_MS: u64 = 20_000;

/// Per-result scrape budget for search enrichment when the caller passes no
/// `scrapeOptions.timeout`.
///
/// NOT `effective_deadline_ms(None, None)`: an implicit deadline auto-extends to
/// the full renderer ladder (`http + lightpanda + chrome + 28s per CDP tier` —
/// 92.5s on the docker config), which is the right budget for ONE `/v1/scrape`
/// but not here: search waits for every result, so a single straggler walking
/// the whole ladder stalls the entire response. 15s is the deadline this
/// codebase already validated against the zero-byte-miss class
/// (`config.docker.toml` `[request]`), and it leaves chrome its full
/// `chrome_nav_budget_ms` whenever the HTTP prefetch is quick.
const SEARCH_ENRICH_DEADLINE_MS: u64 = 15_000;

/// Upper bound for a caller-supplied `scrapeOptions.timeout`. Matches the range
/// documented on `ScrapeRequest.deadline_ms`; enforced here because search
/// multiplies the budget across every result.
const SEARCH_ENRICH_DEADLINE_MAX_MS: u64 = 60_000;

/// Fold the C1-overlap prefetch outcomes into the final (reranked-over-union)
/// pool. A successful prefetch hands over its content; a FAILED one hands over
/// its error, which is what stops `enrich_with_scrape` from scraping that URL a
/// second time and spending the per-result budget twice in one request.
fn fold_prescraped(pool: &mut [SearchResult], prescraped: &[SearchResult]) {
    let by_url: std::collections::HashMap<&str, &SearchResult> =
        prescraped.iter().map(|r| (r.url.as_str(), r)).collect();
    for r in pool.iter_mut() {
        if r.metadata.is_some() {
            continue;
        }
        let Some(src) = by_url.get(r.url.as_str()) else {
            continue;
        };
        if src.metadata.is_some() {
            r.markdown = src.markdown.clone();
            r.html = src.html.clone();
            r.raw_html = src.raw_html.clone();
            r.links = src.links.clone();
            r.metadata = src.metadata.clone();
            r.truncated = src.truncated;
        } else if src.error.is_some() {
            r.error = src.error.clone();
        }
    }
}

/// Per-result scrape budget for one enrichment fan-out. Validated in
/// [`validate_request`], so the caller value is already within
/// `(0, SEARCH_ENRICH_DEADLINE_MAX_MS]` by the time it reaches here.
fn enrich_deadline_ms(opts: &SearchScrapeOptions) -> u64 {
    opts.timeout.unwrap_or(SEARCH_ENRICH_DEADLINE_MS)
}

/// Customer-facing text for "the backend could not answer", used both as the
/// non-LLM error and as the LLM-path warning. A single constant because the
/// multi-round leg has to be able to REMOVE it again by value when a later
/// round rescues the request. Names no backend, engine, or provider.
const DEGRADED_MESSAGE: &str = "The search backend could not answer this query. Retry shortly.";

/// Heuristic: did the synthesized answer ABSTAIN (sources lacked the fact)?
/// Aligned with `answer.rs`'s calibrated clause ("ONLY if the sources genuinely
/// do not contain the information, say so plainly"). Triggers the adaptive
/// multi-round scout. Conservative — only well-known abstention phrasings.
fn is_abstention(answer: &str) -> bool {
    let a = answer.to_lowercase();
    const MARKERS: &[&str] = &[
        "do not contain",
        "does not contain",
        "doesn't contain",
        "cannot answer",
        "can't answer",
        "cannot determine",
        "could not find",
        "couldn't find",
        "no information",
        "do not provide",
        "does not provide",
        "not mentioned in",
        "not specified",
        "unable to answer",
        "cannot be answered",
        "sources do not",
        "i cannot",
    ];
    MARKERS.iter().any(|m| a.contains(m))
}

/// Build a short evidence excerpt from the current candidate pool to brief the
/// evidence-scout (title + a markdown/snippet head per source, bounded).
fn evidence_excerpt(data: &SearchData, max_sources: usize, per_chars: usize) -> String {
    let pool: &Vec<SearchResult> = match data {
        SearchData::Flat(v) => v,
        SearchData::Grouped(g) => match g.web.as_ref() {
            Some(v) => v,
            None => return String::new(),
        },
    };
    let mut out = String::new();
    for r in pool.iter().take(max_sources) {
        let body = r.markdown.as_deref().unwrap_or(r.description.as_str());
        let snip: String = body.chars().take(per_chars).collect();
        out.push_str("- ");
        out.push_str(&r.title);
        out.push_str(" :: ");
        out.push_str(snip.trim());
        out.push('\n');
    }
    out
}

/// True when a short scraped body is really a block / error shell (e.g. a page
/// that 403'd to a "Wikimedia Error" datacenter-block, or a bot wall) rather
/// than content. Grounding the answer on such text is worse than useless, so
/// snippet-first drops the body and keeps only the clean SERP snippet. Size-gated
/// so a real article that merely quotes one of these phrases can't false-positive.
fn is_block_shell(md: &str) -> bool {
    if md.len() >= 2000 {
        return false;
    }
    let l = md.to_lowercase();
    [
        "wikimedia error",
        "are forbidden",
        "access denied",
        "request blocked",
        "just a moment",
        "attention required",
        "enable javascript and cookies",
    ]
    .iter()
    .any(|p| l.contains(p))
}

/// Drop rows whose URL is already in the flat pool. Used to skip re-scraping a
/// scout result the answer pool already holds. Recall-safe: `merge_scraped` would
/// discard these same rows anyway (it dedups by URL), and nothing on this path
/// re-scrapes for fresher content — this just avoids paying for the scrape first.
fn drop_known_urls(data: &SearchData, rows: Vec<SearchResult>) -> Vec<SearchResult> {
    let SearchData::Flat(pool) = data else {
        return rows;
    };
    let seen: std::collections::HashSet<&str> = pool.iter().map(|r| r.url.as_str()).collect();
    rows.into_iter()
        .filter(|r| !seen.contains(r.url.as_str()))
        .collect()
}

/// Merge freshly-scraped scout rows into the flat answer pool (dedup by URL,
/// only rows that actually carry markdown). Returns true if any were added.
/// Grouped data (the explicit-`sources` path) is left untouched — multi-round
/// targets the flat answer path. Recall-only: never removes existing sources.
fn merge_scraped(data: &mut SearchData, rows: Vec<SearchResult>) -> bool {
    if let SearchData::Flat(pool) = data {
        let mut seen: std::collections::HashSet<String> =
            pool.iter().map(|r| r.url.clone()).collect();
        let mut added = false;
        for r in rows {
            if r.markdown.is_some() && seen.insert(r.url.clone()) {
                pool.push(r);
                added = true;
            }
        }
        added
    } else {
        false
    }
}
const DEFAULT_MAX_CHARS_PER_SOURCE: usize = 8192;

/// Wave 4 (R2): hard cap on `max_tokens` per LLM leg (one summary call OR
/// the answer call). Independent of the user's configured `cfg.max_tokens`
/// because the SaaS-side `estimateMaxCreditCostForSearch` uses this number
/// to pre-reserve credits; a per-leg cap higher than this would let real
/// usage exceed the reservation. Mirror in
/// `crw-saas/src/lib/llm-pricing.ts::legCost` (default 1024).
const SEARCH_LLM_MAX_TOKENS_PER_LEG: u32 = 1024;

use crate::error::AppError;
use crate::state::AppState;

const MAX_QUERY_CHARS: usize = 2000;
const MAX_LANG_CHARS: usize = 35;

/// A language tag (`en`, `pt-BR`, `zh-Hans-CN`, `es-419`, `en-u-ca`) or one of
/// the `auto`/`all` sentinels the search backends already understand.
///
/// Not a full RFC 5646 grammar: it requires a 2-3 letter primary subtag, so
/// private-use (`x-…`) and grandfathered (`i-klingon`) tags are rejected. Those
/// are not search locales.
///
/// `lang` is forwarded verbatim as SearXNG's `language`, and a deployment may
/// point that at a backend which interpolates it into a request line rather than
/// a URL-encoded parameter, where a `\r\n` would split the request. Anything
/// outside this shape is a caller mistake, so it is rejected here.
fn is_valid_lang(lang: &str) -> bool {
    if lang == "auto" || lang == "all" {
        return true;
    }
    let mut subtags = lang.split('-');
    let Some(primary) = subtags.next() else {
        return false;
    };
    if !(2..=3).contains(&primary.len()) || !primary.bytes().all(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    subtags.all(|s| (1..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// `POST /v1/search` — search the web via SearXNG, optionally enriching
/// each `web` result by running it through the scrape pipeline in-process.
///
/// Mirrors the public contract exposed by `crw-saas/src/app/api/v1/search/route.ts`
/// (minus the credit / quota wrapper, which lives in the SaaS layer).
pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<SearchRequest>, JsonRejection>,
) -> Result<Json<SearchResponse>, AppError> {
    let Json(mut req) = body.map_err(AppError::from)?;
    // Read the entitlement from the header, never from the body: `paid_rescue`
    // is `serde(skip)`, so a caller cannot grant it to itself by sending the
    // field. Only crw-saas sets this header, and only after checking the plan
    // and role it alone can see.
    //
    // Deliberately NOT wired into `/v2/search` (routes/v2), which is an
    // unconditional proxy open to every plan including FREE, nor into the MCP
    // dispatcher or the research legs — all of those reach `search_inner`
    // directly and so keep `paid_rescue: false`.
    req.paid_rescue = headers
        .get(crw_search::PAID_RESCUE_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "1");
    let resp = search_inner(&state, req).await?;
    Ok(Json(resp))
}

/// Shared search logic used by both the HTTP route and the MCP tool dispatcher.
/// Returns the public `SearchResponse` envelope (with `.warning` populated when
/// scrape enrichment partially fails) or a `CrwError` on hard failure.
pub async fn search_inner(
    state: &AppState,
    req: SearchRequest,
) -> Result<SearchResponse, CrwError> {
    validate_request(&req, state.config.search.max_limit)?;

    let client = state
        .searxng
        .as_ref()
        .ok_or_else(|| {
            CrwError::SearchDisabled(
                "Search is disabled. Set [search].search_backend_url in config or define \
                 CRW_SEARCH__SEARXNG_URL to point at a search backend instance."
                    .into(),
            )
        })?
        .clone();

    let limit = req
        .limit
        .unwrap_or(state.config.search.default_limit)
        .min(state.config.search.max_limit)
        .max(1);

    // Request-deadline clock, started at handler entry. Used by the adaptive
    // multi-round gate (below) to decide whether enough budget remains to run a
    // second scout round without risking a 504.
    let req_deadline = Deadline::from_request_ms(state.config.effective_deadline_ms(None, None));

    // BYOK + LLM config is built up-front because multi-query expansion (below)
    // needs the LLM *before* the SearXNG fetch. Reused by the summarize/answer
    // legs further down. `llm_path` = this request enters LLM mode.
    let server_llm = state.config.extraction.llm.clone();
    let byok_llm = build_byok_search_llm_config(&req, server_llm.as_ref());
    let effective_llm = byok_llm.as_ref().or(server_llm.as_ref());
    let llm_path = req.answer.unwrap_or(false) || req.summarize_results.unwrap_or(false);

    let params = map_to_searxng_params(&req, &state.config.search);
    // Multi-query expansion (gated): on the LLM path, also fetch an
    // entity/keyword rewrite of the query and UNION the pools so the answer's
    // source is more likely to surface. Falls back to the single fetch.
    // Per-request override (eval A/B) wins over the server config; clamp so a
    // hostile caller can't fan out unbounded SearXNG fetches.
    let variants_n = req
        .query_expand_variants
        .unwrap_or(state.config.search.query_expand_variants)
        .clamp(1, MAX_QUERY_EXPAND_VARIANTS);
    // Per-request override for query expansion (eval A/B harness), same shape as
    // the multi_round override; None uses the server config. Lets the benchmark
    // A/B expansion against prod via a request param without a global config flip
    // (so real customers are not switched onto the path during a measurement).
    let want_expand = req.query_expand.unwrap_or(state.config.search.query_expand);
    // Phase C1: when expansion + scrapeOptions are both in play, overlap the
    // scrape of the original-query results with the expansion (LLM rewrite +
    // variant fetches) instead of doing them serially. The final pool is the
    // identical union, so the reranked source set is unchanged — only the
    // ~5-10s expansion overhead is hidden behind the original scrape.
    let c1_overlap = state.config.search.pipeline_overlap
        && want_expand
        && llm_path
        && req.scrape_options.is_some()
        && effective_llm.is_some();
    let mut prescraped: Vec<SearchResult> = Vec::new();
    let mut response = if c1_overlap {
        let llm = effective_llm.expect("c1_overlap requires effective_llm");
        let opts = req
            .scrape_options
            .as_ref()
            .expect("c1_overlap requires scrape_options");
        let orig = client
            .fetch(&params)
            .await
            .map_err(|e| map_search_error(e, state.config.search.timeout_ms, client.base_url()))?;
        let mut data_orig = SearchData::Flat(transform_flat_reranked(
            &orig,
            &req.query,
            limit,
            state.config.search.rerank_relevance,
        ));
        // Scrape the original results WHILE the expansion fetches run.
        let (_enr, variant_pools) = tokio::join!(
            enrich_with_scrape(&mut data_orig, opts, state),
            fetch_variant_pools(&client, &req.query, &params, llm, variants_n),
        );
        if let SearchData::Flat(v) = data_orig {
            prescraped = v;
        }
        let mut merged = orig;
        union_pools(&mut merged, variant_pools);
        merged
    } else if want_expand
        && llm_path
        && let Some(llm) = effective_llm
    {
        fetch_expanded(&client, &req.query, &params, llm, variants_n)
            .await
            .map_err(|e| map_search_error(e, state.config.search.timeout_ms, client.base_url()))?
    } else {
        client
            .fetch(&params)
            .await
            .map_err(|e| map_search_error(e, state.config.search.timeout_ms, client.base_url()))?
    };

    // The backend can answer HTTP 200 with an empty result set for two very
    // different reasons: the query genuinely has no results, or the backend
    // itself couldn't answer (engines failed / upstream flagged degraded).
    // `is_degraded()` distinguishes the two. Off the LLM path `response` is
    // final (every rescue leg below is gated on `llm_path`), so a degraded
    // empty response can be rejected outright. On the LLM path, page-2 and
    // Wikidata rescues still run, so we only warn and let the request continue.
    if !llm_path && response.is_degraded() {
        return Err(CrwError::SearchDegraded(DEGRADED_MESSAGE.into()));
    }

    let has_sources = req.sources.as_ref().is_some_and(|s| !s.is_empty());
    // The LLM answer / summarize path feeds the top-N flat sources straight to
    // the model, so it must receive a clean, query-relevant pool. Re-rank the
    // flat pool on that path (unless disabled); the plain path keeps the raw
    // SaaS byte-parity `transform_flat` sort.
    let mut data = if has_sources {
        let sources = req.sources.clone().unwrap_or_default();
        SearchData::Grouped(transform_grouped(&response, &sources, limit))
    } else if llm_path && state.config.search.rerank_enabled {
        SearchData::Flat(transform_flat_reranked(
            &response,
            &req.query,
            limit,
            state.config.search.rerank_relevance,
        ))
    } else {
        SearchData::Flat(transform_flat(&response, limit))
    };

    // W0: parse SearXNG's infoboxes[]/answers[] (Wikidata/Wikipedia structured
    // facts the results[] transform discards) into pinned answer sources. Gated
    // default-off; empty when the flag is off or no structured data was returned.
    let mut structured_sources: Vec<answer::Source> = if state.config.search.use_structured_sources
    {
        crw_search::structured_facts(&response)
            .into_iter()
            .map(|f| {
                let md = f.to_markdown();
                (f.url, f.title, md)
            })
            .collect()
    } else {
        Vec::new()
    };

    // W3: deterministic Wikidata entity-relation lookup (gated, answer path).
    // For `<relation> of <entity>` queries the obscure-entity long tail web
    // search can't surface, resolve the fact via Wikidata and PIN it first.
    // 3s-bounded + cached; any miss/error leaves the normal path untouched.
    if state.config.search.wikidata_lookup
        && llm_path
        && let Some(f) = crw_search::wikidata::lookup(&req.query).await
    {
        let md = f.to_markdown();
        structured_sources.insert(0, (f.url, f.title, md));
    }

    // Page-2 fallback (gated, default-off): when the reranked clean pool came
    // back thinner than the answer needs (junk filter stripped a sparse first
    // page), fetch the SAME query's page 2 ONCE, union it in (dedup by URL like
    // `fetch_expanded`), and re-rank. Trigger is evaluated POST-rerank so a
    // junk-heavy first page doesn't suppress it; extra load only fires on
    // already-under-yielding queries. Recall-only — synthesis/abstention in
    // `answer.rs` is untouched.
    if state.config.search.page2_fallback
        && llm_path
        && state.config.search.rerank_enabled
        && !has_sources
    {
        let top_n = req
            .answer_top_n
            .unwrap_or(DEFAULT_ANSWER_TOP_N)
            .min(MAX_ANSWER_TOP_N) as usize;
        let clean_count = match &data {
            SearchData::Flat(v) => v.len(),
            SearchData::Grouped(_) => top_n,
        };
        if clean_count < top_n {
            let mut p2 = params.clone();
            p2.pageno = Some(2);
            if let Ok(resp2) = client.fetch(&p2).await {
                let mut seen: std::collections::HashSet<String> = response
                    .results
                    .iter()
                    .filter_map(|r| r.url.clone())
                    .collect();
                for row in resp2.results {
                    if let Some(u) = row.url.clone()
                        && seen.insert(u)
                    {
                        response.results.push(row);
                    }
                }
                response.number_of_results = response.results.len() as u64;
                data = SearchData::Flat(transform_flat_reranked(
                    &response,
                    &req.query,
                    limit,
                    state.config.search.rerank_relevance,
                ));
            }
        }
    }

    // Phase C1: fold the original-results scrapes done during the overlap back
    // into the final (reranked-over-union) source set. Entries that match by URL
    // get their scraped outcome reused; enrich_with_scrape then skips them and
    // only scrapes the URLs the expansion newly added. Grouped responses fold
    // too — the prefetch pool is flat either way, and skipping the fold there
    // would scrape those URLs a second time on the same request.
    if !prescraped.is_empty() {
        match &mut data {
            SearchData::Flat(v) => fold_prescraped(v, &prescraped),
            SearchData::Grouped(g) => {
                if let Some(web) = g.web.as_mut() {
                    fold_prescraped(web, &prescraped);
                }
            }
        }
    }

    let mut warning: Option<String> = None;
    let mut warnings: Vec<String> = Vec::new();
    // Re-evaluated here, NOT carried over from the check above: both LLM-path
    // rescues run in between, and a rescued request must not ship a "backend
    // could not answer" warning alongside genuine results.
    //   - page-2 pushes rows into `response.results`, which `is_degraded()`
    //     already self-corrects on (it requires emptiness).
    //   - Wikidata / structured facts populate `structured_sources` WITHOUT
    //     touching `response.results`, so that rescue needs its own term — and
    //     only counts when `answer` is set, since answer synthesis is the sole
    //     consumer of `structured_sources`. A `summarizeResults`-only request
    //     is NOT rescued by them and must still get the warning.
    let structured_rescue = req.answer.unwrap_or(false) && !structured_sources.is_empty();
    if response.is_degraded() && !structured_rescue {
        warnings.push(DEGRADED_MESSAGE.into());
    }
    // Snippet-first (lazy scrape): on the answer path, defer the scrape and try to
    // answer from the free SERP snippets + structured sources first; only scrape
    // if that answer abstains (below, after synthesis). Most factoid queries answer
    // from the snippet, so this skips the expensive scrape on the majority.
    // snippet_first REQUIRES snippet_fallback: the round-1 lazy synth builds its
    // sources from snippets, and with scrape skipped a non-fallback synth would
    // return Err (empty markdown pool) and never reach the escalation, silently
    // yielding no answer. Gate on it so enabling snippet_first alone is a safe
    // no-op (normal eager scrape) rather than a recall regression.
    let want_snippet_first = req.answer.unwrap_or(false)
        && req
            .snippet_first
            .unwrap_or(state.config.search.snippet_first)
        && state.config.search.snippet_fallback;
    let mut scraped = false;
    if let Some(opts) = req.scrape_options.as_ref()
        && !want_snippet_first
    {
        match enrich_with_scrape(&mut data, opts, state).await {
            Ok(()) => scraped = true,
            Err(msg) => {
                tracing::warn!(error = %msg, "scrape enrichment failed");
                warning = Some(msg);
            }
        }
    }

    // `effective_llm` / `byok_llm` / `server_llm` were built up-front (above).
    let wants_summaries = req.summarize_results.unwrap_or(false);
    let wants_answer = req.answer.unwrap_or(false);
    // Wave 4 (R1): once we enter LLM mode the response MUST carry a
    // non-null llmUsage object (the always-present invariant the SaaS
    // 5-branch dispatch relies on). We aggregate summary + answer counts
    // into this builder and emit it at every return path below.
    let mut llm_attempted = false;
    let mut agg_input_tokens: u32 = 0;
    let mut agg_output_tokens: u32 = 0;
    let mut agg_cache_hit: u32 = 0;
    let mut agg_cache_miss: u32 = 0;
    let mut agg_calls: u32 = 0;
    let mut agg_executed_summaries: u32 = 0;
    let mut agg_answer_executed = false;
    let mut agg_provider: String = String::new();
    let mut agg_model: String = String::new();
    let mut agg_truncated = false;
    let merge_usage = |agg_input_tokens: &mut u32,
                       agg_output_tokens: &mut u32,
                       agg_cache_hit: &mut u32,
                       agg_cache_miss: &mut u32,
                       agg_calls: &mut u32,
                       agg_provider: &mut String,
                       agg_model: &mut String,
                       agg_truncated: &mut bool,
                       u: &LlmUsage| {
        *agg_input_tokens = agg_input_tokens.saturating_add(u.input_tokens);
        *agg_output_tokens = agg_output_tokens.saturating_add(u.output_tokens);
        *agg_cache_hit = agg_cache_hit.saturating_add(u.cache_hit_input_tokens.unwrap_or(0));
        *agg_cache_miss = agg_cache_miss.saturating_add(u.cache_miss_input_tokens.unwrap_or(0));
        *agg_calls = agg_calls.saturating_add(u.calls.max(1));
        if agg_provider.is_empty() {
            *agg_provider = u.provider.clone();
        }
        if agg_model.is_empty() {
            *agg_model = u.model.clone();
        }
        *agg_truncated = *agg_truncated || u.truncated;
    };

    if (wants_summaries || wants_answer) && req.scrape_options.is_none() {
        warnings.push(
            "summarizeResults / answer require scrapeOptions to populate markdown; skipped".into(),
        );
    } else if wants_summaries || wants_answer {
        match effective_llm {
            None => warnings.push(
                "summarizeResults / answer require an LLM config (set [extraction.llm] or \
                 pass llm_api_key)"
                    .into(),
            ),
            Some(llm) => {
                llm_attempted = true;
                // Wave 4 (R2): cap max_tokens at SEARCH_LLM_MAX_TOKENS_PER_LEG so
                // a single leg can never exceed the SaaS pre-reservation in
                // estimateMaxCreditCostForSearch.
                let mut leg_cfg = llm.clone();
                leg_cfg.max_tokens = leg_cfg.max_tokens.min(SEARCH_LLM_MAX_TOKENS_PER_LEG);
                // Eval determinism: a request-supplied answer temperature (the
                // benchmark harness sets 0) overrides the provider default so
                // A/B runs are reproducible. None = current prod behavior.
                if req.answer_temperature.is_some() {
                    leg_cfg.temperature = req.answer_temperature;
                }
                if wants_summaries {
                    let (count, usages) = attach_result_summaries(
                        &mut data,
                        &leg_cfg,
                        leg_cfg.max_concurrency,
                        req.summary_prompt.as_deref(),
                        req.max_content_chars,
                    )
                    .await;
                    agg_executed_summaries = count.ok as u32;
                    for u in usages.into_iter().flatten() {
                        merge_usage(
                            &mut agg_input_tokens,
                            &mut agg_output_tokens,
                            &mut agg_cache_hit,
                            &mut agg_cache_miss,
                            &mut agg_calls,
                            &mut agg_provider,
                            &mut agg_model,
                            &mut agg_truncated,
                            &u,
                        );
                    }
                    if count.failed > 0 {
                        warnings.push(format!(
                            "{} of {} per-result summaries failed",
                            count.failed,
                            count.failed + count.ok
                        ));
                    }
                }
                if wants_answer {
                    // List-format answer (gated): a request override wins, else
                    // the server flag; either way it only fires on list-intent
                    // queries ("best/top X in Y"). Factual queries keep prose.
                    let list_format = req
                        .answer_list_format
                        .unwrap_or(state.config.search.answer_list_format)
                        && answer::is_list_intent(&req.query);
                    match synthesize_answer(
                        &req,
                        &data,
                        &leg_cfg,
                        state.config.search.passage_select,
                        state.config.search.answer_bm25_select,
                        state.config.search.answer_calibrated,
                        state.config.search.snippet_fallback,
                        state.config.search.answer_guarded,
                        &structured_sources,
                        list_format,
                    )
                    .await
                    {
                        Ok((mut ans, mut cites, ans_usage, mut ans_warns)) => {
                            warnings.append(&mut ans_warns);
                            agg_answer_executed = true;
                            if let Some(ref u) = ans_usage {
                                merge_usage(
                                    &mut agg_input_tokens,
                                    &mut agg_output_tokens,
                                    &mut agg_cache_hit,
                                    &mut agg_cache_miss,
                                    &mut agg_calls,
                                    &mut agg_provider,
                                    &mut agg_model,
                                    &mut agg_truncated,
                                    u,
                                );
                            }
                            // Snippet-first escalation: the snippet-only answer
                            // abstained, so NOW scrape the original results and
                            // re-synthesize once. Monotone: a still-abstaining
                            // re-synth keeps the snippet answer. Bounded by the
                            // request deadline like the scout's scrape.
                            if want_snippet_first
                                && !scraped
                                && is_abstention(&ans)
                                && let Some(opts) = req.scrape_options.as_ref()
                            {
                                let _ = tokio::time::timeout(
                                    req_deadline.remaining(),
                                    enrich_with_scrape(&mut data, opts, state),
                                )
                                .await;
                                if let Ok((ans2, cites2, usage2, mut warns2)) = synthesize_answer(
                                    &req,
                                    &data,
                                    &leg_cfg,
                                    state.config.search.passage_select,
                                    state.config.search.answer_bm25_select,
                                    state.config.search.answer_calibrated,
                                    state.config.search.snippet_fallback,
                                    state.config.search.answer_guarded,
                                    &structured_sources,
                                    list_format,
                                )
                                .await
                                {
                                    if let Some(ref u) = usage2 {
                                        merge_usage(
                                            &mut agg_input_tokens,
                                            &mut agg_output_tokens,
                                            &mut agg_cache_hit,
                                            &mut agg_cache_miss,
                                            &mut agg_calls,
                                            &mut agg_provider,
                                            &mut agg_model,
                                            &mut agg_truncated,
                                            u,
                                        );
                                    }
                                    if !is_abstention(&ans2) {
                                        ans = ans2;
                                        cites = cites2;
                                        warnings.append(&mut warns2);
                                    }
                                }
                            }
                            // Adaptive multi-round (gated): if round-1 ABSTAINED,
                            // the evidence-scout issues targeted follow-ups; we
                            // scrape them, union into the pool, and re-synthesize
                            // ONCE. Recall-only — a still-abstaining round-2 is
                            // discarded (keep round-1). Only fires on abstention,
                            // so the single-shot fast path is unchanged for hits.
                            let want_multi =
                                req.multi_round.unwrap_or(state.config.search.multi_round);
                            // Deadline budget: the extra scout round can add tens
                            // of seconds. If too little of the request deadline
                            // remains, skip it and return round-1 promptly rather
                            // than risk a 504 — this caps the worst-case latency
                            // multi_round would otherwise add.
                            let multi_budget_ok = req_deadline.remaining().as_millis() as u64
                                >= MULTI_ROUND_MIN_BUDGET_MS;
                            if want_multi && is_abstention(&ans) && !multi_budget_ok {
                                warnings.push(
                                    "multi-round skipped: insufficient deadline budget remaining"
                                        .to_string(),
                                );
                            }
                            if want_multi
                                && is_abstention(&ans)
                                && multi_budget_ok
                                && let Some(opts) = req.scrape_options.as_ref()
                            {
                                let evidence = evidence_excerpt(&data, 5, 400);
                                let scout_qs = crw_extract::llm::scout_followups(
                                    &leg_cfg,
                                    &req.query,
                                    &evidence,
                                    MAX_SCOUT_QUERIES,
                                )
                                .await;
                                let mut grew = false;
                                for sq in scout_qs {
                                    // Bound every scout op by the request deadline.
                                    // `multi_budget_ok` only gates ENTRY; once inside,
                                    // a slow SearXNG fetch or a hung scrape could run
                                    // far past the caller's budget (a scout scrape was
                                    // observed hanging ~6 min → 502). Out of budget:
                                    // stop and keep round-1.
                                    if req_deadline.remaining().is_zero() {
                                        break;
                                    }
                                    let mut sp = params.clone();
                                    sp.q = sq;
                                    if let Ok(Ok(resp2)) = tokio::time::timeout(
                                        req_deadline.remaining(),
                                        client.fetch(&sp),
                                    )
                                    .await
                                    {
                                        let extra = transform_flat_reranked(
                                            &resp2,
                                            &req.query,
                                            SCOUT_FETCH_LIMIT,
                                            state.config.search.rerank_relevance,
                                        );
                                        // Drop scout results already in the pool
                                        // BEFORE scraping. merge_scraped only dedups
                                        // AFTER the scrape, so a URL from round 1 (or
                                        // returned by both scout queries — the pool
                                        // has grown by iteration 2) would otherwise be
                                        // scraped again and its result discarded,
                                        // paying the per-result budget for nothing.
                                        let fresh = drop_known_urls(&data, extra);
                                        if fresh.is_empty() {
                                            continue;
                                        }
                                        let mut sd = SearchData::Flat(fresh);
                                        let _ = tokio::time::timeout(
                                            req_deadline.remaining(),
                                            enrich_with_scrape(&mut sd, opts, state),
                                        )
                                        .await;
                                        if let SearchData::Flat(rows) = sd {
                                            grew |= merge_scraped(&mut data, rows);
                                        }
                                    }
                                }
                                // Skip the round-2 re-synthesis if the scout work
                                // already consumed the deadline, so the extra LLM
                                // call can't run (and bill) past the caller's budget.
                                if grew
                                    && !req_deadline.remaining().is_zero()
                                    && let Ok((ans2, cites2, usage2, mut warns2)) =
                                        synthesize_answer(
                                            &req,
                                            &data,
                                            &leg_cfg,
                                            state.config.search.passage_select,
                                            state.config.search.answer_bm25_select,
                                            state.config.search.answer_calibrated,
                                            state.config.search.snippet_fallback,
                                            state.config.search.answer_guarded,
                                            &structured_sources,
                                            list_format,
                                        )
                                        .await
                                {
                                    // Round-2 actually called the LLM and consumed
                                    // tokens, so its usage MUST be merged for honest
                                    // accounting even if it abstained — only the
                                    // ANSWER/citations are adopted conditionally
                                    // (when round-2 produced a non-abstaining
                                    // answer). Merging usage before the abstention
                                    // check prevents silently dropping the round-2
                                    // token cost.
                                    if let Some(ref u) = usage2 {
                                        merge_usage(
                                            &mut agg_input_tokens,
                                            &mut agg_output_tokens,
                                            &mut agg_cache_hit,
                                            &mut agg_cache_miss,
                                            &mut agg_calls,
                                            &mut agg_provider,
                                            &mut agg_model,
                                            &mut agg_truncated,
                                            u,
                                        );
                                    }
                                    if !is_abstention(&ans2) {
                                        ans = ans2;
                                        cites = cites2;
                                        warnings.append(&mut warns2);
                                        // Round 2 answered. Its scout rows land
                                        // in `data`, never in `response.results`,
                                        // so the degraded warning pushed earlier
                                        // would otherwise ship next to a real
                                        // answer telling the caller to retry.
                                        warnings.retain(|w| w != DEGRADED_MESSAGE);
                                    }
                                }
                            }
                            let aggregated = build_aggregated_usage(
                                agg_input_tokens,
                                agg_output_tokens,
                                agg_cache_hit,
                                agg_cache_miss,
                                agg_calls,
                                agg_executed_summaries,
                                agg_answer_executed,
                                agg_provider.clone(),
                                agg_model.clone(),
                                agg_truncated,
                                &leg_cfg,
                            );
                            let wrapped = SearchResponseData {
                                results: data,
                                answer: Some(ans),
                                citations: cites,
                                llm_usage: Some(aggregated),
                                warnings,
                            };
                            let mut resp = ApiResponse::ok(wrapped);
                            resp.warning = warning;
                            return Ok(resp);
                        }
                        Err(msg) => {
                            // Log the raw upstream error server-side, but never
                            // surface it to the client: `{msg}` can carry the
                            // managed-LLM provider name + raw HTTP status (P1-2).
                            tracing::warn!(error = %msg, "answer synthesis failed");
                            warnings.push("answer synthesis unavailable".to_string());
                        }
                    }
                }
            }
        }
    }

    // R1 always-present invariant: if we attempted LLM work, emit the
    // aggregated usage even when zero tokens were consumed (e.g. all
    // summaries failed and no answer leg ran). The SaaS dispatch maps
    // (executedSummaries == 0 && answerExecuted == false && tokens == 0)
    // to Branch 1 (no-op refund); anything else routes correctly.
    let final_usage = if llm_attempted {
        Some(build_aggregated_usage(
            agg_input_tokens,
            agg_output_tokens,
            agg_cache_hit,
            agg_cache_miss,
            agg_calls,
            agg_executed_summaries,
            agg_answer_executed,
            if agg_provider.is_empty() {
                effective_llm
                    .map(|c| c.provider.clone())
                    .unwrap_or_default()
            } else {
                agg_provider
            },
            if agg_model.is_empty() {
                effective_llm.map(|c| c.model.clone()).unwrap_or_default()
            } else {
                agg_model
            },
            agg_truncated,
            effective_llm
                .map(|c| {
                    let mut c = c.clone();
                    c.max_tokens = c.max_tokens.min(SEARCH_LLM_MAX_TOKENS_PER_LEG);
                    c
                })
                .as_ref()
                .unwrap_or(&crw_core::config::LlmConfig::default()),
        ))
    } else {
        None
    };

    let wrapped = SearchResponseData {
        results: data,
        answer: None,
        citations: Vec::new(),
        llm_usage: final_usage,
        warnings,
    };
    let mut resp = ApiResponse::ok(wrapped);
    resp.warning = warning;
    Ok(resp)
}

#[allow(clippy::too_many_arguments)]
fn build_aggregated_usage(
    input_tokens: u32,
    output_tokens: u32,
    cache_hit: u32,
    cache_miss: u32,
    calls: u32,
    executed_summaries: u32,
    answer_executed: bool,
    provider: String,
    model: String,
    truncated: bool,
    fallback_cfg: &crw_core::config::LlmConfig,
) -> LlmUsage {
    LlmUsage {
        input_tokens,
        output_tokens,
        total_tokens: input_tokens.saturating_add(output_tokens),
        estimated_cost_usd: None,
        model: if model.is_empty() {
            fallback_cfg.model.clone()
        } else {
            model
        },
        provider: if provider.is_empty() {
            fallback_cfg.provider.clone()
        } else {
            provider
        },
        cache_hit_input_tokens: if cache_hit == 0 {
            None
        } else {
            Some(cache_hit)
        },
        cache_miss_input_tokens: if cache_miss == 0 {
            None
        } else {
            Some(cache_miss)
        },
        truncated,
        calls: calls.max(1),
        executed_summaries,
        answer_executed,
    }
}

#[derive(Default)]
struct SummaryFanoutCount {
    ok: usize,
    failed: usize,
}

/// Fan-out summary calls across all results that have markdown. Bounded by
/// `max_concurrency`. Pattern mirrors `crates/crw-crawl/src/sitemap.rs`.
///
/// Wave 4 (R1): returns the per-call `Option<LlmUsage>` for every job
/// alongside the ok/failed count so the caller can aggregate token totals
/// across summaries + answer.
async fn attach_result_summaries(
    data: &mut SearchData,
    cfg: &LlmConfig,
    max_concurrency: usize,
    user_prompt: Option<&str>,
    max_content_chars: Option<usize>,
) -> (SummaryFanoutCount, Vec<Option<LlmUsage>>) {
    let targets: &mut Vec<SearchResult> = match data {
        SearchData::Flat(v) => v,
        SearchData::Grouped(g) => match g.web.as_mut() {
            Some(v) if !v.is_empty() => v,
            _ => return (SummaryFanoutCount::default(), Vec::new()),
        },
    };
    // Capture markdown + index pairs first so we don't hold a borrow of
    // `targets` across the async fan-out.
    let jobs: Vec<(usize, String)> = targets
        .iter()
        .enumerate()
        .filter_map(|(idx, r)| r.markdown.as_ref().map(|md| (idx, md.clone())))
        .collect();
    if jobs.is_empty() {
        return (SummaryFanoutCount::default(), Vec::new());
    }
    let cfg_owned = cfg.clone();
    let user_prompt_owned: Option<String> = user_prompt.map(str::to_owned);
    let concurrency = max_concurrency.max(1);
    type SummaryOutcome = (usize, Result<(String, Option<LlmUsage>), String>);
    let results: Vec<SummaryOutcome> = stream::iter(jobs)
        .map(|(idx, md)| {
            let cfg = cfg_owned.clone();
            let user_prompt = user_prompt_owned.clone();
            async move {
                let outcome =
                    summary::summarize(&md, &cfg, user_prompt.as_deref(), max_content_chars)
                        .await
                        .map(|r| (r.content, r.usage))
                        .map_err(|e| e.to_string());
                (idx, outcome)
            }
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;

    let mut count = SummaryFanoutCount::default();
    let mut usages: Vec<Option<LlmUsage>> = Vec::with_capacity(results.len());
    for (idx, res) in results {
        match res {
            Ok((text, usage)) => {
                if let Some(slot) = targets.get_mut(idx) {
                    slot.summary = Some(text);
                    count.ok += 1;
                    usages.push(usage);
                }
            }
            Err(_) => {
                count.failed += 1;
                usages.push(None);
            }
        }
    }
    (count, usages)
}

#[allow(clippy::too_many_arguments)]
async fn synthesize_answer(
    req: &SearchRequest,
    data: &SearchData,
    cfg: &LlmConfig,
    passage_select: bool,
    bm25_select: bool,
    calibrated: bool,
    snippet_fallback: bool,
    guarded: bool,
    // W0: structured facts (infoboxes/answers) to PIN at the front of the pool.
    structured: &[answer::Source],
    list_format: bool,
) -> Result<
    (
        String,
        Vec<crw_core::types::Citation>,
        Option<crw_core::types::LlmUsage>,
        Vec<String>,
    ),
    String,
> {
    // Calibrated answer feeds more sources by default (Pattern A: the answer
    // often sits in result #6-8, or a failed top-5 scrape thinned the pool) and
    // uses the anti-hedge prompt clause. An explicit request `answer_top_n`
    // still wins. Capped by MAX_ANSWER_TOP_N.
    let default_top_n = if calibrated {
        CALIBRATED_ANSWER_TOP_N
    } else {
        DEFAULT_ANSWER_TOP_N
    };
    let top_n = req
        .answer_top_n
        .unwrap_or(default_top_n)
        .min(MAX_ANSWER_TOP_N) as usize;
    let cap = req
        .max_chars_per_source
        .unwrap_or(DEFAULT_MAX_CHARS_PER_SOURCE)
        .min(answer::MAX_CHARS_PER_SOURCE_CEILING);

    let pool: &Vec<SearchResult> = match data {
        SearchData::Flat(v) => v,
        SearchData::Grouped(g) => match g.web.as_ref() {
            Some(v) => v,
            None => return Err("no web results to synthesize from".into()),
        },
    };
    let scraped: Vec<answer::Source> = pool
        .iter()
        .filter_map(|r| {
            // Legacy path (snippet_fallback off): markdown-only, BYTE-IDENTICAL to
            // the pre-change behavior — a result with markdown is kept as-is (even
            // a block shell), so the multi-round scout still sees it and can
            // recover. Block-guard + snippet-first apply ONLY when snippet_fallback
            // is on (there the clean snippet replaces the dropped block body).
            if !snippet_fallback {
                return r
                    .markdown
                    .as_ref()
                    .map(|md| (r.url.clone(), r.title.clone(), md.clone()));
            }
            let md = r.markdown.as_deref().map(str::trim).unwrap_or("");
            // Block-guard: a fetched-but-blocked page ("Wikimedia Error", bot wall)
            // is noise, not content — drop the body and lean on the clean snippet.
            let body = if md.is_empty() || is_block_shell(md) {
                ""
            } else {
                md
            };
            // Snippet-first: the SERP snippet is the engine's own query-relevant
            // answer passage, so put it FIRST (it then survives the per-source
            // passage budget) and always include it; append the body for depth.
            // Verbatim upstream text can only surface a present fact, never invent.
            let desc = r.description.trim();
            match (desc.is_empty(), body.is_empty()) {
                (true, true) => None,
                (false, true) => {
                    Some((r.url.clone(), r.title.clone(), format!("[snippet] {desc}")))
                }
                (true, false) => Some((r.url.clone(), r.title.clone(), body.to_string())),
                (false, false) => Some((
                    r.url.clone(),
                    r.title.clone(),
                    format!("[snippet] {desc}\n\n{body}"),
                )),
            }
        })
        .take(top_n)
        .collect();
    // W0: PIN structured facts (Wikidata/Wikipedia infobox/answers) at the front
    // so the synthesizer sees them first. They are still UNTRUSTED-wrapped by
    // `answer::synthesize` — this widens evidence, it does not bypass safety.
    let sources: Vec<answer::Source> = if structured.is_empty() {
        scraped
    } else {
        structured.iter().cloned().chain(scraped).collect()
    };
    if sources.is_empty() {
        return Err("no results carry markdown to synthesize an answer from".into());
    }
    // Passage-select reduces each large source to its query-relevant passages
    // before synthesis (monotone-safe: falls back to the full source on any
    // failure). Gated; off = byte-identical to plain synthesize.
    let result = if passage_select {
        answer::synthesize_selected(
            &req.query,
            &sources,
            cfg,
            cap,
            req.answer_prompt.as_deref(),
            calibrated,
            guarded,
            list_format,
        )
        .await
    } else {
        answer::synthesize(
            &req.query,
            &sources,
            cfg,
            cap,
            req.answer_prompt.as_deref(),
            calibrated,
            guarded,
            list_format,
            bm25_select,
        )
        .await
    }
    .map_err(|e| e.to_string())?;
    Ok((
        result.content,
        result.citations,
        result.usage,
        result.warnings,
    ))
}

fn build_byok_search_llm_config(
    req: &SearchRequest,
    server_cfg: Option<&LlmConfig>,
) -> Option<LlmConfig> {
    let api_key = req.llm_api_key.as_ref()?.clone();
    let mut cfg = match server_cfg {
        Some(s) => s.clone(),
        None => LlmConfig::default(),
    };
    cfg.api_key = api_key;
    if let Some(p) = &req.llm_provider {
        cfg.provider = p.clone();
    }
    if let Some(m) = &req.llm_model {
        cfg.model = m.clone();
    }
    if let Some(b) = &req.base_url {
        cfg.base_url = Some(b.clone());
    }
    // Never inherit the server's reasoning_effort into a BYOK request — the
    // customer's endpoint must receive only what they explicitly configure.
    cfg.reasoning_effort = None;
    Some(cfg)
}

fn validate_request(req: &SearchRequest, max_limit: u32) -> Result<(), CrwError> {
    let len = req.query.chars().count();
    if len == 0 {
        return Err(CrwError::InvalidRequest("query is required".into()));
    }
    if len > MAX_QUERY_CHARS {
        return Err(CrwError::InvalidRequest(format!(
            "query length {len} exceeds maximum of {MAX_QUERY_CHARS} characters"
        )));
    }
    if let Some(l) = req.limit
        && (l == 0 || l > max_limit)
    {
        return Err(CrwError::InvalidRequest(format!(
            "limit must be between 1 and {max_limit} (got {l})"
        )));
    }
    if let Some(l) = req.lang.as_deref().map(str::trim)
        && !l.is_empty()
        && (l.chars().count() > MAX_LANG_CHARS || !is_valid_lang(l))
    {
        return Err(CrwError::InvalidRequest(format!(
            "lang must be a language tag such as 'en' or 'pt-BR' (got {l:?})"
        )));
    }
    if let Some(cats) = &req.categories
        && cats.len() > 5
    {
        return Err(CrwError::InvalidRequest(
            "categories accepts at most 5 entries".into(),
        ));
    }
    if let Some(opts) = req.scrape_options.as_ref() {
        // Search enrichment can only carry formats that fit the
        // `SearchResult` shape. `plainText` and `json` (LLM extract) require
        // fields the search-result envelope doesn't expose; rejecting up-front
        // is clearer than silently dropping them post-scrape.
        for f in &opts.formats {
            if matches!(f, OutputFormat::PlainText | OutputFormat::Json) {
                return Err(CrwError::InvalidRequest(format!(
                    "scrapeOptions.formats does not support {f:?} on /v1/search; use \
                     /v1/scrape for plainText/json (extract). Allowed: markdown, html, \
                     rawHtml, links."
                )));
            }
        }
        if let Some(t) = opts.timeout
            && (t == 0 || t > SEARCH_ENRICH_DEADLINE_MAX_MS)
        {
            return Err(CrwError::InvalidRequest(format!(
                "scrapeOptions.timeout must be between 1 and \
                 {SEARCH_ENRICH_DEADLINE_MAX_MS} ms (got {t})"
            )));
        }
    }
    Ok(())
}

/// Map a transport/timeout/upstream `SearchError` onto the HTTP `CrwError`.
/// `base_url` is the configured search backend URL. The operator still learns
/// which host failed (issue #90), but through the log: the response names no
/// host, and an upstream error page is reduced to its status, because both are
/// internal infrastructure a caller may not see. Timeouts keep
/// `error_code: "timeout"`.
pub(crate) fn map_search_error(err: SearchError, timeout_ms: u64, base_url: &str) -> CrwError {
    match err {
        SearchError::Timeout => CrwError::Timeout(timeout_ms),
        SearchError::Upstream { status, body } => {
            tracing::warn!(
                search_backend = %crate::diagnostics::sanitize_url_origin(base_url),
                status,
                body = %body.chars().take(200).collect::<String>(),
                "search backend returned an error"
            );
            CrwError::HttpError(format!("Search backend returned HTTP {status}"))
        }
        SearchError::InvalidResponse(msg) => {
            CrwError::HttpError(format!("Search backend returned invalid JSON: {msg}"))
        }
        SearchError::Transport(msg) => {
            tracing::warn!(
                search_backend = %crate::diagnostics::sanitize_url_origin(base_url),
                "search backend unreachable: {msg}"
            );
            CrwError::TargetUnreachable(format!("Search backend unreachable: {msg}"))
        }
    }
}

/// Multi-query expansion: fetch the original query plus an LLM-generated
/// entity/keyword rewrite, then UNION the candidate pools (dedupe by URL,
/// original results kept first so they retain priority). Recall can only
/// increase vs a single fetch. The original fetch's error propagates (same
/// failure semantics as the single-fetch path); a failed variant fetch is
/// ignored. If the rewrite is empty/trivial, this is exactly the single fetch.
/// Expand the query (LLM rewrite) and fetch all variant pools concurrently
/// (bounded by the variant count) so N rewrites cost ~one extra fetch of
/// wall-clock, not N sequential ones. Does NOT fetch the original query — the
/// caller owns that, which lets the C1 overlap path scrape the original results
/// while this runs. A failed variant fetch is dropped (recall-only, never fatal).
async fn fetch_variant_pools(
    client: &SearxngClient,
    query: &str,
    base_params: &SearxngParams,
    llm: &LlmConfig,
    max_variants: usize,
) -> Vec<SearxngResponse> {
    let mut leg = llm.clone();
    leg.max_tokens = leg.max_tokens.min(SEARCH_LLM_MAX_TOKENS_PER_LEG);
    let variants = crw_extract::llm::expand_query(&leg, query, max_variants).await;
    if variants.is_empty() {
        return Vec::new();
    }
    stream::iter(variants)
        .map(|v| {
            let client = client.clone();
            let mut vp = base_params.clone();
            vp.q = v;
            async move { client.fetch(&vp).await.ok() }
        })
        .buffer_unordered(max_variants.max(1))
        .filter_map(|r| async move { r })
        .collect()
        .await
}

/// Union variant pools into `merged`, deduping by URL (recall-only — never
/// removes existing sources). Shared by the serial and C1-overlap paths so both
/// produce the identical unioned pool.
fn union_pools(merged: &mut SearxngResponse, pools: Vec<SearxngResponse>) {
    let mut seen: std::collections::HashSet<String> = merged
        .results
        .iter()
        .filter_map(|r| r.url.clone())
        .collect();
    for resp in pools {
        for row in resp.results {
            if let Some(u) = row.url.clone()
                && seen.insert(u)
            {
                merged.results.push(row);
            }
        }
    }
    merged.number_of_results = merged.results.len() as u64;
}

async fn fetch_expanded(
    client: &SearxngClient,
    query: &str,
    base_params: &SearxngParams,
    llm: &LlmConfig,
    max_variants: usize,
) -> Result<SearxngResponse, SearchError> {
    // Original fetch overlaps the expansion+variant fetches; union is identical.
    let (orig, variant_pools) = tokio::join!(
        client.fetch(base_params),
        fetch_variant_pools(client, query, base_params, llm, max_variants)
    );
    let mut merged = orig?;
    union_pools(&mut merged, variant_pools);
    Ok(merged)
}

/// Enrich `web` (or flat) results in-place by calling the scrape pipeline
/// for each result URL. Bounded by `[crawler].max_concurrency`. On per-URL
/// failure the result is left without `markdown`/`html`/etc. fields — the
/// search response still succeeds.
async fn enrich_with_scrape(
    data: &mut SearchData,
    opts: &SearchScrapeOptions,
    state: &AppState,
) -> Result<(), String> {
    let targets: &mut Vec<SearchResult> = match data {
        SearchData::Flat(v) => v,
        SearchData::Grouped(g) => match g.web.as_mut() {
            Some(v) if !v.is_empty() => v,
            _ => return Ok(()), // nothing to enrich
        },
    };
    if targets.is_empty() {
        return Ok(());
    }

    // Validate each URL and remember which slot it came from.
    // Each `validate_safe_url_resolved` does a DNS lookup; running them serially
    // added up to ~max_limit cold lookups (~9s at limit 20) on the critical path
    // before any scrape could start. SERP results are diverse domains, so this is
    // the common case, not the worst case. Validate concurrently instead — N is
    // bounded by `max_limit` (≤20), so `join_all` needs no width cap, and it
    // preserves order (irrelevant here since each job carries its own `idx`).
    let candidates: Vec<(usize, url::Url, String)> = targets
        .iter()
        .enumerate()
        // C1 overlap: a slot already handled by the original-results prefetch is
        // reused, not re-scraped — whether it succeeded (metadata set by
        // apply_scrape_to_result) or failed (error set). Re-scraping a failure
        // here would spend the per-result budget twice in one request.
        .filter(|(_, r)| r.metadata.is_none() && r.error.is_none())
        .filter_map(|(idx, r)| {
            url::Url::parse(&r.url)
                .ok()
                .map(|parsed| (idx, parsed, r.url.clone()))
        })
        .collect();
    let jobs: Vec<(usize, String)> = futures::future::join_all(candidates.into_iter().map(
        |(idx, parsed, original)| async move {
            crw_core::url_safety::validate_safe_url_resolved(&parsed)
                .await
                .ok()
                // Scrape the caller's original URL string, not the reparsed
                // (possibly re-normalized) one — same as the serial version.
                .map(|()| (idx, original))
        },
    ))
    .await
    .into_iter()
    .flatten()
    .collect();
    if jobs.is_empty() {
        return Ok(());
    }

    let formats = opts.formats.clone();
    let only_main = opts.only_main_content;
    let country = opts.country.clone();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(
        state.config.crawler.max_concurrency.max(1),
    ));
    let mut set: JoinSet<(usize, Result<ScrapeData, String>)> = JoinSet::new();

    for (idx, url) in jobs {
        let formats = formats.clone();
        let country = country.clone();
        let renderer = state.renderer.clone();
        let llm_config = state.config.extraction.llm.clone();
        let extraction_cfg = state.config.extraction.clone();
        let user_agent = state.config.crawler.user_agent.clone();
        let default_stealth =
            state.config.crawler.stealth.enabled && state.config.crawler.stealth.inject_headers;
        let render_js_default = state.config.renderer.render_js_default;
        let deadline_ms = enrich_deadline_ms(opts);
        let permit_src = semaphore.clone();

        // Deliberately NOT scoped to `ScrapeClass::Batch`: `/v1/search` is a
        // synchronous, latency-sensitive interactive endpoint (not a job entry
        // point), so its enrichment scrapes are interactive traffic and correctly
        // read the `Interactive` default — they legitimately use interactive
        // capacity rather than the batch lane.
        set.spawn(async move {
            let _permit = match permit_src.acquire_owned().await {
                Ok(p) => p,
                Err(e) => return (idx, Err(format!("semaphore closed: {e}"))),
            };
            let scrape_req = ScrapeRequest {
                url: url.clone(),
                formats,
                only_main_content: only_main,
                render_js: None,
                wait_for: None,
                include_tags: vec![],
                exclude_tags: vec![],
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
                country,
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
                deadline_ms: Some(deadline_ms),
                debug: None,
                change_tracking: None,
                goal: None,
                judge_enabled: None,
                parsers: None,
                screenshot_full_page: false,
                // Search enrichment takes the published cache default like any
                // other scrape: a SERP URL fetched twice in a minute is the
                // same page. Its short per-URL budget is recorded with the
                // entry, so a longer-budget caller is never served a page this
                // path had to cut short.
                // Search enrichment does not use the page cache. It could only
                // reach the unscoped bucket from here (`search_inner` never sees
                // the request headers, so it has no tenant scope to pass), and
                // on a hosted deployment nothing may land in a bucket that is
                // shared between customers. SERP URLs are mostly distinct
                // anyway, so there is little to win.
                max_age: Some(0),
                store_in_cache: Some(false),
                cache_scope: None,
            };
            let deadline = Deadline::from_request_ms(deadline_ms);
            let result = scrape_url(
                &scrape_req,
                &renderer,
                llm_config.as_ref(),
                &extraction_cfg,
                &user_agent,
                default_stealth,
                render_js_default,
                deadline,
            )
            .await
            .map_err(|e| e.to_string());
            (idx, result)
        });
    }

    while let Some(joined) = set.join_next().await {
        let (idx, result) = match joined {
            Ok(pair) => pair,
            Err(join_err) => {
                tracing::warn!(error = %join_err, "scrape enrichment task panicked");
                continue;
            }
        };
        let Some(slot) = targets.get_mut(idx) else {
            continue;
        };
        match result {
            Ok(scrape) => apply_scrape_to_result(slot, scrape, &opts.formats),
            Err(msg) => {
                tracing::debug!(url = %slot.url, error = %msg, "scrape enrichment skipped");
                // P3-4: mark the result so a partial scrape is observable to the
                // caller instead of looking identical to "no markdown found".
                slot.error = Some(msg);
            }
        }
    }
    Ok(())
}

fn apply_scrape_to_result(slot: &mut SearchResult, data: ScrapeData, formats: &[OutputFormat]) {
    // A blocked page is not enrichment. `scrape_url` reports a wall as a verdict
    // on the document rather than an `Err`, and `clear_body()` runs in the scrape
    // ROUTE, not here, so without this the interstitial ("Checking your browser",
    // a CloudFront deny page) lands in `slot.markdown` as if it were the result.
    // Downstream that is worse than an empty slot: the SaaS counts a result
    // successful on body presence alone, so the caller is billed for it, and
    // answer synthesis takes the wall text as evidence.
    //
    // Same shape as the `Err(msg)` arm in the caller: name the reason, leave the
    // body unset.
    if let Some(block) = &data.block {
        slot.error = Some(block.message());
        slot.metadata = Some(data.metadata);
        return;
    }
    if formats.contains(&OutputFormat::Markdown) {
        slot.markdown = data.markdown;
    }
    if formats.contains(&OutputFormat::Html) {
        slot.html = data.html;
    }
    if formats.contains(&OutputFormat::RawHtml) {
        slot.raw_html = data.raw_html;
    }
    if formats.contains(&OutputFormat::Links) {
        slot.links = data.links;
    }
    if data.truncated {
        slot.truncated = Some(true);
    }
    slot.metadata = Some(data.metadata);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crw_core::types::SearchSource;

    fn blank_slot(url: &str) -> SearchResult {
        SearchResult {
            url: url.to_string(),
            title: String::new(),
            description: String::new(),
            snippet: String::new(),
            position: 1,
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
        }
    }

    fn scrape_opts(timeout: Option<u64>) -> SearchScrapeOptions {
        SearchScrapeOptions {
            formats: vec![OutputFormat::Markdown],
            only_main_content: true,
            country: None,
            timeout,
        }
    }

    #[test]
    fn blocked_enrichment_reports_the_reason_and_ships_no_body() {
        // A wall reaches `apply_scrape_to_result` as `Ok(data)` carrying a block
        // verdict, because `clear_body()` runs in the scrape route and not here.
        // Copying its markdown into the slot makes the interstitial look like the
        // page: the SaaS counts a result successful on body presence alone, so it
        // bills, and answer synthesis reads the wall as evidence.
        let mut slot = blank_slot("https://example.com/walled");
        let data = ScrapeData {
            markdown: Some("Checking your browser before you access...".to_string()),
            block: Some(crw_core::types::BlockOutcome {
                vendor: "generic_block".to_string(),
                reason: "anti-bot wall that no renderer tier could clear".to_string(),
            }),
            ..Default::default()
        };

        apply_scrape_to_result(&mut slot, data, &[OutputFormat::Markdown]);

        assert!(
            slot.markdown.is_none(),
            "wall text must not become the body"
        );
        let err = slot.error.expect("a blocked slot names its reason");
        assert!(err.contains("anti-bot"), "got {err}");
    }

    #[test]
    fn unblocked_enrichment_still_fills_the_body() {
        // The guard above must not swallow a real page.
        let mut slot = blank_slot("https://example.com/real");
        let data = ScrapeData {
            markdown: Some("# A real page".to_string()),
            ..Default::default()
        };

        apply_scrape_to_result(&mut slot, data, &[OutputFormat::Markdown]);

        assert_eq!(slot.markdown.as_deref(), Some("# A real page"));
        assert!(slot.error.is_none());
    }

    #[test]
    fn enrichment_deadline_is_bounded_not_the_renderer_ladder() {
        // Regression pin: enrichment must NOT inherit the implicit full-ladder
        // deadline (`effective_deadline_ms(None, None)` — 92.5s on the docker
        // renderer config). Search waits for every result, so one straggler
        // walking the ladder stalls the whole response.
        // Prod raises the implicit budget to 60s and the ladder extension takes
        // it to ~92.5s; the enrichment budget must be independent of both.
        let cfg: crw_core::config::AppConfig =
            toml::from_str("[request]\ndeadline_ms_default = 60000\n").expect("config parses");
        assert_eq!(cfg.effective_deadline_ms(None, None), 60_000);
        assert_eq!(enrich_deadline_ms(&scrape_opts(None)), 15_000);
    }

    #[test]
    fn truncated_render_is_marked_on_the_result() {
        // A budget-shortened render still returns content, so without this flag
        // it is indistinguishable from a page that genuinely has little text —
        // which is what would make a tightened budget a silent recall loss.
        let mut slot = bare_result("https://example.com/a");
        let mut data: ScrapeData = serde_json::from_value(serde_json::json!({
            "markdown": "# partial",
            "metadata": {"sourceURL": "https://example.com/a", "statusCode": 200, "elapsedMs": 1}
        }))
        .expect("valid scrape data");
        data.truncated = true;
        apply_scrape_to_result(&mut slot, data, &[OutputFormat::Markdown]);
        assert_eq!(slot.truncated, Some(true));

        let mut slot = bare_result("https://example.com/b");
        let data: ScrapeData = serde_json::from_value(serde_json::json!({
            "markdown": "# whole",
            "metadata": {"sourceURL": "https://example.com/b", "statusCode": 200, "elapsedMs": 1}
        }))
        .expect("valid scrape data");
        apply_scrape_to_result(&mut slot, data, &[OutputFormat::Markdown]);
        assert_eq!(slot.truncated, None);
    }

    fn bare_result(url: &str) -> SearchResult {
        serde_json::from_value(serde_json::json!({
            "url": url, "title": "t", "description": "d", "position": 1
        }))
        .expect("valid result")
    }

    #[test]
    fn scout_dedup_drops_urls_already_in_the_pool() {
        // The scout must not re-scrape a URL the answer pool already holds:
        // enrich_with_scrape spends the per-result budget, then merge_scraped
        // discards the duplicate. drop_known_urls removes them before the scrape.
        let mut known = bare_result("https://example.com/known");
        known.markdown = Some("# known".into());
        let data = SearchData::Flat(vec![known]);
        let scout_rows = vec![
            bare_result("https://example.com/known"), // already in the pool
            bare_result("https://example.com/fresh"), // new -> keep
        ];
        let fresh = drop_known_urls(&data, scout_rows);
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].url, "https://example.com/fresh");
    }

    #[test]
    fn prefetch_outcomes_fold_back_into_the_final_pool() {
        let mut ok = bare_result("https://example.com/ok");
        ok.markdown = Some("# ok".into());
        ok.truncated = Some(true);
        ok.metadata = Some(
            serde_json::from_value(serde_json::json!({
                "sourceURL": "https://example.com/ok", "statusCode": 200, "elapsedMs": 1
            }))
            .expect("valid metadata"),
        );
        let mut failed = bare_result("https://example.com/failed");
        failed.error = Some("Timeout after 15000ms".into());

        let mut pool = vec![
            bare_result("https://example.com/ok"),
            bare_result("https://example.com/failed"),
            bare_result("https://example.com/fresh"),
        ];
        fold_prescraped(&mut pool, &[ok, failed]);

        assert_eq!(pool[0].markdown.as_deref(), Some("# ok"));
        // A truncated prefetch must not lose its marker on the way through.
        assert_eq!(pool[0].truncated, Some(true));
        // A failed prefetch carries its error, which is what makes
        // `enrich_with_scrape` skip it instead of paying the budget twice.
        assert!(pool[1].error.is_some() && pool[1].metadata.is_none());
        // A URL the expansion newly added is untouched and still gets scraped.
        assert!(pool[2].error.is_none() && pool[2].metadata.is_none());
    }

    #[test]
    fn scrape_options_timeout_range_is_validated() {
        let req = |timeout: Option<u64>| {
            let json = serde_json::json!({"query": "q", "scrapeOptions": {}});
            let mut r: SearchRequest = serde_json::from_value(json).expect("valid request");
            r.scrape_options = Some(scrape_opts(timeout));
            r
        };
        assert!(validate_request(&req(Some(15_000)), 20).is_ok());
        assert!(validate_request(&req(None), 20).is_ok());
        assert!(validate_request(&req(Some(0)), 20).is_err());
        assert!(validate_request(&req(Some(60_001)), 20).is_err());
    }

    #[test]
    fn block_shell_detected_but_not_real_articles() {
        assert!(is_block_shell(
            "# Wikimedia Error\nError: 403, Contabo networks are forbidden."
        ));
        assert!(is_block_shell("Just a moment... Attention Required"));
        // a long real article that merely contains a phrase is not a block shell
        let long = format!(
            "Access denied is a common HTTP concept. {}",
            "x".repeat(2100)
        );
        assert!(!is_block_shell(&long));
        assert!(!is_block_shell(
            "Radcliffe College was a women's liberal arts college."
        ));
    }

    fn req(q: &str) -> SearchRequest {
        SearchRequest {
            query: q.into(),
            limit: None,
            lang: None,
            tbs: None,
            sources: None,
            categories: None,
            scrape_options: None,
            summarize_results: None,
            answer: None,
            answer_top_n: None,
            max_chars_per_source: None,
            llm_api_key: None,
            llm_provider: None,
            llm_model: None,
            base_url: None,
            summary_prompt: None,
            answer_prompt: None,
            answer_temperature: None,
            query_expand_variants: None,
            multi_round: None,
            query_expand: None,
            snippet_first: None,
            answer_list_format: None,
            max_content_chars: None,
            paid_rescue: false,
        }
    }

    #[test]
    fn is_abstention_detects_marker_phrases() {
        assert!(is_abstention(
            "The sources do not contain this information."
        ));
        assert!(is_abstention("I cannot answer that from the sources."));
        assert!(is_abstention("That is not mentioned in the sources."));
        assert!(is_abstention("The provided sources do not provide a year."));
        assert!(is_abstention("I could not find the answer."));
        assert!(is_abstention("The date is not specified anywhere."));
    }

    #[test]
    fn is_abstention_false_for_normal_answer() {
        assert!(!is_abstention("The capital of Serbia is Belgrade."));
        assert!(!is_abstention("Rust was first released publicly in 2010."));
    }

    #[test]
    fn validate_rejects_empty_query() {
        assert!(matches!(
            validate_request(&req(""), 20),
            Err(CrwError::InvalidRequest(_))
        ));
    }

    #[test]
    fn validate_rejects_oversized_query() {
        let q = "x".repeat(MAX_QUERY_CHARS + 1);
        assert!(matches!(
            validate_request(&req(&q), 20),
            Err(CrwError::InvalidRequest(_))
        ));
    }

    #[test]
    fn validate_rejects_limit_above_max() {
        let mut r = req("rust");
        r.limit = Some(50);
        assert!(matches!(
            validate_request(&r, 20),
            Err(CrwError::InvalidRequest(_))
        ));
    }

    #[test]
    fn validate_rejects_zero_limit() {
        let mut r = req("rust");
        r.limit = Some(0);
        assert!(matches!(
            validate_request(&r, 20),
            Err(CrwError::InvalidRequest(_))
        ));
    }

    #[test]
    fn validate_accepts_basic_request() {
        assert!(validate_request(&req("rust async"), 20).is_ok());
    }

    #[test]
    fn validate_rejects_malformed_lang() {
        // `lang` is forwarded as SearXNG's `language`; a backend that builds a
        // request line from it rather than a URL-encoded parameter would see the
        // CRLF here as the end of the request line.
        for bad in [
            "a\r\nA:1",
            "en\r\nA:1",
            "e n",
            "en;q=1",
            "../x",
            "e",
            "toolongprimary",
            "en-",
            "en-toolongsubtag",
        ] {
            let mut r = req("rust");
            r.lang = Some(bad.to_string());
            assert!(
                matches!(validate_request(&r, 20), Err(CrwError::InvalidRequest(_))),
                "accepted malformed lang {bad:?}"
            );
        }
    }

    #[test]
    fn validate_accepts_real_language_tags_and_sentinels() {
        // Looser than what the Google tier ultimately sends — the public contract
        // is a language tag, and `auto`/`all` are documented values, so rejecting
        // any of these would be a 400 for a legitimate caller.
        for good in [
            "en",
            "pt-BR",
            "zh-Hans-CN",
            "nb-NO",
            "auto",
            "all",
            "en-US",
            // Real tags whose tail is not language-region: a Latin-American
            // Spanish region code and a BCP-47 extension singleton.
            "es-419",
            "en-u-ca",
            "",
        ] {
            let mut r = req("rust");
            r.lang = Some(good.to_string());
            assert!(
                validate_request(&r, 20).is_ok(),
                "rejected valid lang {good:?}"
            );
        }
    }

    #[test]
    fn map_search_error_timeout_to_timeout() {
        assert!(matches!(
            map_search_error(SearchError::Timeout, 7500, "http://searxng:8080"),
            CrwError::Timeout(7500)
        ));
    }

    #[test]
    fn map_search_error_upstream_to_http_error() {
        let err = SearchError::Upstream {
            status: 503,
            body: "down".into(),
        };
        assert!(matches!(
            map_search_error(err, 5000, "http://searxng:8080"),
            CrwError::HttpError(_)
        ));
    }

    #[test]
    fn map_search_error_transport_names_no_host() {
        // The unreachable error keeps the transport reason for the caller and
        // nothing about the backend: no host, no userinfo, no path token. The
        // operator gets the sanitized origin from the log line instead.
        let err = SearchError::Transport("dns error: failed to lookup address".into());
        let mapped = map_search_error(err, 5000, "https://user:pass@searxng:8080/tok?k=v");
        match mapped {
            CrwError::TargetUnreachable(msg) => {
                assert!(msg.contains("dns error"), "{msg}");
                assert!(
                    !msg.contains("://"),
                    "must not name the backend origin: {msg}"
                );
                assert!(!msg.contains("8080"), "must not leak the port: {msg}");
                assert!(!msg.contains("user"), "must not leak userinfo: {msg}");
                assert!(!msg.contains("pass"), "must not leak credentials: {msg}");
                assert!(!msg.contains("tok"), "must not leak path token: {msg}");
            }
            other => panic!("expected TargetUnreachable, got {other:?}"),
        }
    }

    #[test]
    fn byok_config_clears_reasoning_effort() {
        // A BYOK request must never inherit the server's reasoning_effort.
        let server_cfg = LlmConfig {
            reasoning_effort: Some("none".into()),
            ..Default::default()
        };
        let mut r = req("hello");
        r.llm_api_key = Some("byok-key".into());
        let byok = build_byok_search_llm_config(&r, Some(&server_cfg))
            .expect("byok config built when llm_api_key present");
        assert_eq!(byok.reasoning_effort, None);
        assert_eq!(byok.api_key, "byok-key");
    }

    #[test]
    fn byok_config_none_without_api_key() {
        let server_cfg = LlmConfig {
            reasoning_effort: Some("none".into()),
            ..Default::default()
        };
        // No llm_api_key => not a BYOK request.
        let byok = build_byok_search_llm_config(&req("hello"), Some(&server_cfg));
        assert!(byok.is_none());
    }

    #[test]
    fn _suppress_unused_search_source_warning() {
        let _ = SearchSource::Web;
    }

    // ── validate_request: more boundary/format coverage ────────────────

    #[test]
    fn validate_accepts_categories_at_max_boundary() {
        let mut r = req("rust");
        r.categories = Some(vec![
            crw_core::types::SearchCategory::Github,
            crw_core::types::SearchCategory::Research,
            crw_core::types::SearchCategory::Pdf,
            crw_core::types::SearchCategory::Other("news".into()),
            crw_core::types::SearchCategory::Github,
        ]);
        assert!(validate_request(&r, 20).is_ok());
    }

    #[test]
    fn validate_rejects_categories_over_max() {
        let mut r = req("rust");
        r.categories = Some(vec![
            crw_core::types::SearchCategory::Github,
            crw_core::types::SearchCategory::Research,
            crw_core::types::SearchCategory::Pdf,
            crw_core::types::SearchCategory::Other("news".into()),
            crw_core::types::SearchCategory::Github,
            crw_core::types::SearchCategory::Research,
        ]);
        assert!(matches!(
            validate_request(&r, 20),
            Err(CrwError::InvalidRequest(_))
        ));
    }

    #[test]
    fn validate_accepts_empty_categories_vec() {
        let mut r = req("rust");
        r.categories = Some(vec![]);
        assert!(validate_request(&r, 20).is_ok());
    }

    #[test]
    fn validate_rejects_scrape_options_plain_text_format() {
        let mut r = req("rust");
        let mut opts = scrape_opts(None);
        opts.formats = vec![OutputFormat::PlainText];
        r.scrape_options = Some(opts);
        let err = validate_request(&r, 20).unwrap_err();
        match err {
            CrwError::InvalidRequest(msg) => {
                assert!(
                    msg.contains("plainText") || msg.contains("PlainText"),
                    "{msg}"
                );
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_scrape_options_json_format() {
        let mut r = req("rust");
        let mut opts = scrape_opts(None);
        opts.formats = vec![OutputFormat::Json];
        r.scrape_options = Some(opts);
        assert!(matches!(
            validate_request(&r, 20),
            Err(CrwError::InvalidRequest(_))
        ));
    }

    #[test]
    fn validate_accepts_all_allowed_scrape_formats() {
        let mut r = req("rust");
        let mut opts = scrape_opts(None);
        opts.formats = vec![
            OutputFormat::Markdown,
            OutputFormat::Html,
            OutputFormat::RawHtml,
            OutputFormat::Links,
        ];
        r.scrape_options = Some(opts);
        assert!(validate_request(&r, 20).is_ok());
    }

    #[test]
    fn validate_accepts_scrape_options_timeout_lower_boundary() {
        let mut r = req("rust");
        r.scrape_options = Some(scrape_opts(Some(1)));
        assert!(validate_request(&r, 20).is_ok());
    }

    #[test]
    fn validate_accepts_scrape_options_timeout_upper_boundary() {
        let mut r = req("rust");
        r.scrape_options = Some(scrape_opts(Some(SEARCH_ENRICH_DEADLINE_MAX_MS)));
        assert!(validate_request(&r, 20).is_ok());
    }

    #[test]
    fn validate_accepts_query_at_max_chars_boundary() {
        let q = "x".repeat(MAX_QUERY_CHARS);
        assert!(validate_request(&req(&q), 20).is_ok());
    }

    #[test]
    fn validate_counts_query_length_in_chars_not_bytes() {
        // Multi-byte emoji: well under MAX_QUERY_CHARS in `.chars().count()`
        // even though the byte length is several times larger.
        let q = "🦀".repeat(500);
        assert_eq!(q.chars().count(), 500);
        assert!(validate_request(&req(&q), 20).is_ok());
    }

    #[test]
    fn validate_accepts_limit_at_max_boundary() {
        let mut r = req("rust");
        r.limit = Some(20);
        assert!(validate_request(&r, 20).is_ok());
    }

    #[test]
    fn validate_accepts_limit_of_one() {
        let mut r = req("rust");
        r.limit = Some(1);
        assert!(validate_request(&r, 20).is_ok());
    }

    #[test]
    fn validate_accepts_whitespace_only_lang_as_empty() {
        // `validate_request` trims before checking emptiness, so a
        // whitespace-only lang is treated the same as an absent one.
        let mut r = req("rust");
        r.lang = Some("   ".into());
        assert!(validate_request(&r, 20).is_ok());
    }

    #[test]
    fn validate_none_lang_is_accepted() {
        let mut r = req("rust");
        r.lang = None;
        assert!(validate_request(&r, 20).is_ok());
    }

    // ── is_valid_lang: direct coverage of the private predicate ─────────

    #[test]
    fn is_valid_lang_accepts_sentinels() {
        assert!(is_valid_lang("auto"));
        assert!(is_valid_lang("all"));
    }

    #[test]
    fn is_valid_lang_rejects_single_char_primary() {
        assert!(!is_valid_lang("e"));
    }

    #[test]
    fn is_valid_lang_rejects_four_char_primary() {
        assert!(!is_valid_lang("engl"));
    }

    #[test]
    fn is_valid_lang_accepts_two_and_three_char_primary() {
        assert!(is_valid_lang("en"));
        assert!(is_valid_lang("eng"));
    }

    // ── is_abstention: broader marker + shape coverage ──────────────────

    #[test]
    fn is_abstention_case_insensitive() {
        assert!(is_abstention("THE SOURCES DO NOT CONTAIN THIS."));
        assert!(is_abstention("I Cannot Answer That."));
    }

    #[test]
    fn is_abstention_marker_inside_larger_sentence() {
        assert!(is_abstention(
            "Based on the provided passages, I am unable to answer this question directly."
        ));
    }

    #[test]
    fn is_abstention_empty_string_is_false() {
        assert!(!is_abstention(""));
    }

    #[test]
    fn is_abstention_every_marker_individually_triggers() {
        const MARKERS: &[&str] = &[
            "do not contain",
            "does not contain",
            "doesn't contain",
            "cannot answer",
            "can't answer",
            "cannot determine",
            "could not find",
            "couldn't find",
            "no information",
            "do not provide",
            "does not provide",
            "not mentioned in",
            "not specified",
            "unable to answer",
            "cannot be answered",
            "sources do not",
            "i cannot",
        ];
        for m in MARKERS {
            let sentence = format!("Well, {m} the answer.");
            assert!(is_abstention(&sentence), "marker {m:?} did not trigger");
        }
    }

    // ── is_block_shell: boundary + phrase coverage ───────────────────────

    #[test]
    fn is_block_shell_boundary_exactly_2000_chars_not_flagged() {
        // The size gate is `md.len() >= 2000`, so exactly 2000 bytes already
        // returns false regardless of content.
        let md = format!(
            "{}{}",
            "just a moment ",
            "x".repeat(2000 - "just a moment ".len())
        );
        assert_eq!(md.len(), 2000);
        assert!(!is_block_shell(&md));
    }

    #[test]
    fn is_block_shell_just_under_2000_flagged() {
        let md = format!(
            "{}{}",
            "just a moment ",
            "x".repeat(1999 - "just a moment ".len())
        );
        assert_eq!(md.len(), 1999);
        assert!(is_block_shell(&md));
    }

    #[test]
    fn is_block_shell_case_insensitive() {
        assert!(is_block_shell("ACCESS DENIED"));
        assert!(is_block_shell("Request Blocked by firewall"));
    }

    #[test]
    fn is_block_shell_every_phrase_individually() {
        for phrase in [
            "wikimedia error",
            "are forbidden",
            "access denied",
            "request blocked",
            "just a moment",
            "attention required",
            "enable javascript and cookies",
        ] {
            assert!(is_block_shell(phrase), "phrase {phrase:?} did not flag");
        }
    }

    #[test]
    fn is_block_shell_empty_string_is_false() {
        assert!(!is_block_shell(""));
    }

    // ── map_search_error: remaining variants ─────────────────────────────

    #[test]
    fn map_search_error_invalid_response_to_http_error() {
        let err = SearchError::InvalidResponse("unexpected EOF".into());
        assert!(matches!(
            map_search_error(err, 5000, "http://searxng:8080"),
            CrwError::HttpError(_)
        ));
    }

    #[test]
    fn map_search_error_upstream_body_never_reaches_the_caller() {
        // An upstream error page is internal infrastructure output (an nginx
        // or uwsgi page naming the upstream address); the caller gets the
        // status alone.
        let err = SearchError::Upstream {
            status: 502,
            body: "<html>502 Bad Gateway upstream 10.0.0.7:8080</html>".into(),
        };
        match map_search_error(err, 5000, "http://searxng:8080") {
            CrwError::HttpError(msg) => {
                assert_eq!(msg, "Search backend returned HTTP 502");
            }
            other => panic!("expected HttpError, got {other:?}"),
        }
    }

    #[test]
    fn map_search_error_transport_plain_host_not_named() {
        let err = SearchError::Transport("connection refused".into());
        match map_search_error(err, 5000, "http://searxng-internal:8080") {
            CrwError::TargetUnreachable(msg) => {
                assert!(!msg.contains("searxng-internal"), "{msg}");
                assert!(msg.contains("connection refused"), "{msg}");
            }
            other => panic!("expected TargetUnreachable, got {other:?}"),
        }
    }

    // ── fold_prescraped: remaining shapes ─────────────────────────────────

    #[test]
    fn fold_prescraped_empty_prescraped_is_noop() {
        let mut pool = vec![bare_result("https://example.com/a")];
        fold_prescraped(&mut pool, &[]);
        assert!(pool[0].markdown.is_none());
        assert!(pool[0].error.is_none());
    }

    #[test]
    fn fold_prescraped_skips_target_that_already_has_metadata() {
        // A target the caller already scraped some other way must not be
        // clobbered by a same-URL prescraped row.
        let mut already = bare_result("https://example.com/a");
        already.metadata = Some(
            serde_json::from_value(serde_json::json!({
                "sourceURL": "https://example.com/a", "statusCode": 200, "elapsedMs": 1
            }))
            .expect("valid metadata"),
        );
        already.markdown = Some("# original".into());
        let mut pool = vec![already];

        let mut src = bare_result("https://example.com/a");
        src.markdown = Some("# from prescrape".into());
        src.metadata = Some(
            serde_json::from_value(serde_json::json!({
                "sourceURL": "https://example.com/a", "statusCode": 200, "elapsedMs": 1
            }))
            .expect("valid metadata"),
        );

        fold_prescraped(&mut pool, &[src]);
        assert_eq!(pool[0].markdown.as_deref(), Some("# original"));
    }

    #[test]
    fn fold_prescraped_preserves_links_and_raw_html() {
        let mut ok = bare_result("https://example.com/ok");
        ok.markdown = Some("# ok".into());
        ok.html = Some("<h1>ok</h1>".into());
        ok.raw_html = Some("<html><h1>ok</h1></html>".into());
        ok.links = Some(vec!["https://example.com/child".into()]);
        ok.metadata = Some(
            serde_json::from_value(serde_json::json!({
                "sourceURL": "https://example.com/ok", "statusCode": 200, "elapsedMs": 1
            }))
            .expect("valid metadata"),
        );

        let mut pool = vec![bare_result("https://example.com/ok")];
        fold_prescraped(&mut pool, &[ok]);
        assert_eq!(pool[0].html.as_deref(), Some("<h1>ok</h1>"));
        assert_eq!(
            pool[0].raw_html.as_deref(),
            Some("<html><h1>ok</h1></html>")
        );
        assert_eq!(
            pool[0].links.as_deref(),
            Some(&["https://example.com/child".to_string()][..])
        );
    }

    // ── drop_known_urls: remaining shapes ─────────────────────────────────

    #[test]
    fn drop_known_urls_grouped_variant_returns_rows_unchanged() {
        let data = SearchData::Grouped(crw_core::types::GroupedSearchData {
            web: Some(vec![bare_result("https://example.com/known")]),
            news: None,
            images: None,
        });
        let rows = vec![
            bare_result("https://example.com/known"),
            bare_result("https://example.com/fresh"),
        ];
        let kept = drop_known_urls(&data, rows);
        assert_eq!(kept.len(), 2, "Grouped data is not deduped, left as-is");
    }

    #[test]
    fn drop_known_urls_empty_rows_returns_empty() {
        let data = SearchData::Flat(vec![bare_result("https://example.com/known")]);
        let kept = drop_known_urls(&data, vec![]);
        assert!(kept.is_empty());
    }

    #[test]
    fn drop_known_urls_empty_pool_keeps_all_rows() {
        let data = SearchData::Flat(vec![]);
        let rows = vec![
            bare_result("https://example.com/a"),
            bare_result("https://example.com/b"),
        ];
        let kept = drop_known_urls(&data, rows);
        assert_eq!(kept.len(), 2);
    }

    // ── merge_scraped ──────────────────────────────────────────────────

    #[test]
    fn merge_scraped_adds_rows_with_markdown() {
        let mut data = SearchData::Flat(vec![]);
        let mut fresh = bare_result("https://example.com/fresh");
        fresh.markdown = Some("# fresh".into());
        let added = merge_scraped(&mut data, vec![fresh]);
        assert!(added);
        let SearchData::Flat(pool) = data else {
            panic!("expected flat");
        };
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0].url, "https://example.com/fresh");
    }

    #[test]
    fn merge_scraped_skips_rows_without_markdown() {
        let mut data = SearchData::Flat(vec![]);
        let no_md = bare_result("https://example.com/empty");
        let added = merge_scraped(&mut data, vec![no_md]);
        assert!(!added);
        let SearchData::Flat(pool) = data else {
            panic!("expected flat");
        };
        assert!(pool.is_empty());
    }

    #[test]
    fn merge_scraped_dedups_by_url_against_existing_pool() {
        let mut existing = bare_result("https://example.com/dup");
        existing.markdown = Some("# original".into());
        let mut data = SearchData::Flat(vec![existing]);

        let mut dup = bare_result("https://example.com/dup");
        dup.markdown = Some("# from scout".into());
        let added = merge_scraped(&mut data, vec![dup]);
        assert!(!added, "duplicate URL must not be added again");
        let SearchData::Flat(pool) = data else {
            panic!("expected flat");
        };
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0].markdown.as_deref(), Some("# original"));
    }

    #[test]
    fn merge_scraped_grouped_variant_returns_false_and_is_untouched() {
        let mut data = SearchData::Grouped(crw_core::types::GroupedSearchData {
            web: Some(vec![]),
            news: None,
            images: None,
        });
        let mut fresh = bare_result("https://example.com/fresh");
        fresh.markdown = Some("# fresh".into());
        let added = merge_scraped(&mut data, vec![fresh]);
        assert!(!added);
        let SearchData::Grouped(g) = data else {
            panic!("expected grouped");
        };
        assert_eq!(g.web.unwrap().len(), 0);
    }

    #[test]
    fn merge_scraped_returns_false_when_nothing_to_add() {
        let mut data = SearchData::Flat(vec![]);
        let added = merge_scraped(&mut data, vec![]);
        assert!(!added);
    }

    // ── evidence_excerpt ───────────────────────────────────────────────

    #[test]
    fn evidence_excerpt_flat_pool_includes_title_and_snippet() {
        let mut r = bare_result("https://example.com/a");
        r.markdown = Some("full markdown body here".into());
        let data = SearchData::Flat(vec![r]);
        let out = evidence_excerpt(&data, 5, 400);
        assert!(out.contains('t'), "title should appear: {out}"); // title is "t"
        assert!(out.contains("full markdown body here"), "{out}");
    }

    #[test]
    fn evidence_excerpt_grouped_with_web_uses_web_bucket() {
        let mut r = bare_result("https://example.com/a");
        r.markdown = Some("web bucket body".into());
        let data = SearchData::Grouped(crw_core::types::GroupedSearchData {
            web: Some(vec![r]),
            news: None,
            images: None,
        });
        let out = evidence_excerpt(&data, 5, 400);
        assert!(out.contains("web bucket body"), "{out}");
    }

    #[test]
    fn evidence_excerpt_grouped_without_web_returns_empty() {
        let data = SearchData::Grouped(crw_core::types::GroupedSearchData {
            web: None,
            news: Some(vec![bare_result("https://example.com/n")]),
            images: None,
        });
        assert_eq!(evidence_excerpt(&data, 5, 400), "");
    }

    #[test]
    fn evidence_excerpt_respects_max_sources() {
        let pool: Vec<SearchResult> = (0..10)
            .map(|i| bare_result(&format!("https://example.com/{i}")))
            .collect();
        let data = SearchData::Flat(pool);
        let out = evidence_excerpt(&data, 3, 400);
        assert_eq!(out.lines().count(), 3);
    }

    #[test]
    fn evidence_excerpt_max_sources_zero_returns_empty() {
        let data = SearchData::Flat(vec![bare_result("https://example.com/a")]);
        assert_eq!(evidence_excerpt(&data, 0, 400), "");
    }

    #[test]
    fn evidence_excerpt_respects_per_chars_truncation() {
        let mut r = bare_result("https://example.com/a");
        r.markdown = Some("x".repeat(1000));
        let data = SearchData::Flat(vec![r]);
        let out = evidence_excerpt(&data, 5, 10);
        // "- t :: " prefix plus at most 10 chars of body plus a newline.
        let body_line = out.lines().next().unwrap();
        assert!(body_line.matches('x').count() <= 10, "{body_line}");
    }

    #[test]
    fn evidence_excerpt_falls_back_to_description_when_markdown_absent() {
        let r = bare_result("https://example.com/a"); // description "d", no markdown
        let data = SearchData::Flat(vec![r]);
        let out = evidence_excerpt(&data, 5, 400);
        assert!(out.contains("d"), "{out}");
    }

    #[test]
    fn evidence_excerpt_unicode_safe_truncation() {
        let mut r = bare_result("https://example.com/a");
        r.markdown = Some("🦀".repeat(50));
        let data = SearchData::Flat(vec![r]);
        // Must not panic on a char boundary inside a multi-byte sequence.
        let out = evidence_excerpt(&data, 5, 10);
        assert!(out.matches('🦀').count() <= 10);
    }

    // ── build_byok_search_llm_config: remaining override combinations ────

    #[test]
    fn byok_config_overrides_provider() {
        let server_cfg = LlmConfig {
            provider: "anthropic".into(),
            ..Default::default()
        };
        let mut r = req("hello");
        r.llm_api_key = Some("key".into());
        r.llm_provider = Some("openai".into());
        let byok = build_byok_search_llm_config(&r, Some(&server_cfg)).unwrap();
        assert_eq!(byok.provider, "openai");
    }

    #[test]
    fn byok_config_overrides_model() {
        let mut r = req("hello");
        r.llm_api_key = Some("key".into());
        r.llm_model = Some("gpt-5".into());
        let byok = build_byok_search_llm_config(&r, None).unwrap();
        assert_eq!(byok.model, "gpt-5");
    }

    #[test]
    fn byok_config_overrides_base_url() {
        let mut r = req("hello");
        r.llm_api_key = Some("key".into());
        r.base_url = Some("https://byok.example.com/v1".into());
        let byok = build_byok_search_llm_config(&r, None).unwrap();
        assert_eq!(
            byok.base_url.as_deref(),
            Some("https://byok.example.com/v1")
        );
    }

    #[test]
    fn byok_config_defaults_when_server_cfg_none() {
        let mut r = req("hello");
        r.llm_api_key = Some("key".into());
        let byok = build_byok_search_llm_config(&r, None).unwrap();
        assert_eq!(byok.api_key, "key");
        assert_eq!(byok.provider, LlmConfig::default().provider);
    }

    #[test]
    fn byok_config_keeps_server_fields_when_not_overridden() {
        let server_cfg = LlmConfig {
            provider: "azure".into(),
            model: "kimi-k2".into(),
            ..Default::default()
        };
        let mut r = req("hello");
        r.llm_api_key = Some("key".into());
        let byok = build_byok_search_llm_config(&r, Some(&server_cfg)).unwrap();
        assert_eq!(byok.provider, "azure");
        assert_eq!(byok.model, "kimi-k2");
    }

    // ── build_aggregated_usage ────────────────────────────────────────

    #[test]
    fn build_aggregated_usage_falls_back_to_cfg_model_and_provider_when_empty() {
        let fallback = LlmConfig {
            provider: "fallback-provider".into(),
            model: "fallback-model".into(),
            ..Default::default()
        };
        let usage = build_aggregated_usage(
            0,
            0,
            0,
            0,
            0,
            0,
            false,
            String::new(),
            String::new(),
            false,
            &fallback,
        );
        assert_eq!(usage.provider, "fallback-provider");
        assert_eq!(usage.model, "fallback-model");
    }

    #[test]
    fn build_aggregated_usage_keeps_supplied_model_and_provider() {
        let fallback = LlmConfig::default();
        let usage = build_aggregated_usage(
            10,
            20,
            0,
            0,
            1,
            0,
            false,
            "actual-provider".into(),
            "actual-model".into(),
            false,
            &fallback,
        );
        assert_eq!(usage.provider, "actual-provider");
        assert_eq!(usage.model, "actual-model");
    }

    #[test]
    fn build_aggregated_usage_calls_floor_is_one() {
        let fallback = LlmConfig::default();
        let usage = build_aggregated_usage(
            0,
            0,
            0,
            0,
            0,
            0,
            false,
            "p".into(),
            "m".into(),
            false,
            &fallback,
        );
        assert_eq!(usage.calls, 1, "calls must never report zero LLM calls");
    }

    #[test]
    fn build_aggregated_usage_cache_tokens_none_when_zero() {
        let fallback = LlmConfig::default();
        let usage = build_aggregated_usage(
            5,
            5,
            0,
            0,
            1,
            0,
            false,
            "p".into(),
            "m".into(),
            false,
            &fallback,
        );
        assert_eq!(usage.cache_hit_input_tokens, None);
        assert_eq!(usage.cache_miss_input_tokens, None);
    }

    #[test]
    fn build_aggregated_usage_cache_tokens_some_when_nonzero() {
        let fallback = LlmConfig::default();
        let usage = build_aggregated_usage(
            5,
            5,
            3,
            2,
            1,
            0,
            false,
            "p".into(),
            "m".into(),
            false,
            &fallback,
        );
        assert_eq!(usage.cache_hit_input_tokens, Some(3));
        assert_eq!(usage.cache_miss_input_tokens, Some(2));
    }

    #[test]
    fn build_aggregated_usage_total_tokens_sums_input_and_output() {
        let fallback = LlmConfig::default();
        let usage = build_aggregated_usage(
            100,
            50,
            0,
            0,
            1,
            0,
            false,
            "p".into(),
            "m".into(),
            false,
            &fallback,
        );
        assert_eq!(usage.total_tokens, 150);
    }

    #[test]
    fn build_aggregated_usage_total_tokens_saturates_at_u32_max() {
        let fallback = LlmConfig::default();
        let usage = build_aggregated_usage(
            u32::MAX,
            u32::MAX,
            0,
            0,
            1,
            0,
            false,
            "p".into(),
            "m".into(),
            false,
            &fallback,
        );
        assert_eq!(
            usage.total_tokens,
            u32::MAX,
            "must saturate, not overflow-panic"
        );
    }

    #[test]
    fn build_aggregated_usage_carries_executed_summaries_and_answer_flag() {
        let fallback = LlmConfig::default();
        let usage = build_aggregated_usage(
            0,
            0,
            0,
            0,
            1,
            3,
            true,
            "p".into(),
            "m".into(),
            true,
            &fallback,
        );
        assert_eq!(usage.executed_summaries, 3);
        assert!(usage.answer_executed);
        assert!(usage.truncated);
    }

    // ── apply_scrape_to_result: format gating ────────────────────────────

    #[test]
    fn apply_scrape_to_result_only_requested_formats_are_applied() {
        let mut slot = bare_result("https://example.com/a");
        let data: ScrapeData = serde_json::from_value(serde_json::json!({
            "markdown": "# md",
            "html": "<h1>md</h1>",
            "rawHtml": "<html><h1>md</h1></html>",
            "links": ["https://example.com/child"],
            "metadata": {"sourceURL": "https://example.com/a", "statusCode": 200, "elapsedMs": 1}
        }))
        .expect("valid scrape data");
        // Only markdown was requested: html/rawHtml/links must stay unset.
        apply_scrape_to_result(&mut slot, data, &[OutputFormat::Markdown]);
        assert_eq!(slot.markdown.as_deref(), Some("# md"));
        assert!(slot.html.is_none());
        assert!(slot.raw_html.is_none());
        assert!(slot.links.is_none());
    }

    #[test]
    fn apply_scrape_to_result_links_format_is_applied_when_requested() {
        let mut slot = bare_result("https://example.com/a");
        let data: ScrapeData = serde_json::from_value(serde_json::json!({
            "links": ["https://example.com/child"],
            "metadata": {"sourceURL": "https://example.com/a", "statusCode": 200, "elapsedMs": 1}
        }))
        .expect("valid scrape data");
        apply_scrape_to_result(&mut slot, data, &[OutputFormat::Links]);
        assert_eq!(
            slot.links.as_deref(),
            Some(&["https://example.com/child".to_string()][..])
        );
    }

    #[test]
    fn apply_scrape_to_result_metadata_is_always_set() {
        let mut slot = bare_result("https://example.com/a");
        let data: ScrapeData = serde_json::from_value(serde_json::json!({
            "metadata": {"sourceURL": "https://example.com/a", "statusCode": 404, "elapsedMs": 1}
        }))
        .expect("valid scrape data");
        // No formats requested at all: metadata is still populated.
        apply_scrape_to_result(&mut slot, data, &[]);
        assert!(slot.metadata.is_some());
        assert!(slot.markdown.is_none());
    }

    // ── enrich_deadline_ms ───────────────────────────────────────────────

    #[test]
    fn enrich_deadline_ms_uses_caller_timeout_when_present() {
        assert_eq!(enrich_deadline_ms(&scrape_opts(Some(5_000))), 5_000);
    }

    #[test]
    fn enrich_deadline_ms_default_when_none() {
        assert_eq!(
            enrich_deadline_ms(&scrape_opts(None)),
            SEARCH_ENRICH_DEADLINE_MS
        );
    }

    // ── union_pools ──────────────────────────────────────────────────────

    fn backend_row(url: &str) -> crw_search::SearxngResult {
        serde_json::from_value(serde_json::json!({"url": url, "title": url, "engine": "google"}))
            .expect("valid upstream result")
    }

    #[test]
    fn union_pools_dedups_by_url() {
        let mut merged = SearxngResponse {
            results: vec![backend_row("https://example.com/a")],
            ..Default::default()
        };
        let pools = vec![SearxngResponse {
            results: vec![
                backend_row("https://example.com/a"), // duplicate
                backend_row("https://example.com/b"),
            ],
            ..Default::default()
        }];
        union_pools(&mut merged, pools);
        assert_eq!(merged.results.len(), 2);
        assert_eq!(merged.number_of_results, 2);
    }

    #[test]
    fn union_pools_drops_incoming_rows_with_no_url() {
        // A malformed row without a URL can't be deduped, and `union_pools`'s
        // `if let Some(u) = row.url.clone()` gate means it is never pushed —
        // only URL-bearing rows widen the merged pool. An originally-present
        // no-url row (already in `merged`) is left untouched either way.
        let none_url_row: crw_search::SearxngResult =
            serde_json::from_value(serde_json::json!({"title": "no url", "engine": "google"}))
                .expect("valid upstream result");
        let mut merged = SearxngResponse {
            results: vec![none_url_row.clone()],
            ..Default::default()
        };
        let pools = vec![SearxngResponse {
            results: vec![none_url_row],
            ..Default::default()
        }];
        union_pools(&mut merged, pools);
        assert_eq!(merged.results.len(), 1);
    }

    #[test]
    fn union_pools_empty_pools_list_is_noop() {
        let mut merged = SearxngResponse {
            results: vec![backend_row("https://example.com/a")],
            ..Default::default()
        };
        union_pools(&mut merged, vec![]);
        assert_eq!(merged.results.len(), 1);
        assert_eq!(merged.number_of_results, 1);
    }

    #[test]
    fn union_pools_preserves_original_rows_before_new_ones() {
        let mut merged = SearxngResponse {
            results: vec![backend_row("https://example.com/first")],
            ..Default::default()
        };
        let pools = vec![SearxngResponse {
            results: vec![backend_row("https://example.com/second")],
            ..Default::default()
        }];
        union_pools(&mut merged, pools);
        assert_eq!(
            merged.results[0].url.as_deref(),
            Some("https://example.com/first")
        );
        assert_eq!(
            merged.results[1].url.as_deref(),
            Some("https://example.com/second")
        );
    }

    // ── SearchScrapeOptions defaults / serde shape ──────────────────────

    #[test]
    fn search_scrape_options_default_formats_is_markdown() {
        let opts: SearchScrapeOptions = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(opts.formats, vec![OutputFormat::Markdown]);
        assert!(opts.only_main_content);
        assert!(opts.country.is_none());
        assert!(opts.timeout.is_none());
    }

    #[test]
    fn search_scrape_options_camel_case_timeout_field() {
        let opts: SearchScrapeOptions =
            serde_json::from_value(serde_json::json!({"timeout": 3000, "onlyMainContent": false}))
                .unwrap();
        assert_eq!(opts.timeout, Some(3000));
        assert!(!opts.only_main_content);
    }

    #[test]
    fn search_scrape_options_roundtrip_preserves_all_fields() {
        let opts = SearchScrapeOptions {
            formats: vec![OutputFormat::Markdown, OutputFormat::Links],
            only_main_content: false,
            country: Some("de".into()),
            timeout: Some(9_000),
        };
        let json = serde_json::to_value(&opts).unwrap();
        let back: SearchScrapeOptions = serde_json::from_value(json).unwrap();
        assert_eq!(back.formats, opts.formats);
        assert_eq!(back.only_main_content, opts.only_main_content);
        assert_eq!(back.country, opts.country);
        assert_eq!(back.timeout, opts.timeout);
    }

    // ── stable constants (regression pins for values other systems rely on) ─

    #[test]
    fn constant_max_query_chars_is_2000() {
        assert_eq!(MAX_QUERY_CHARS, 2000);
    }

    #[test]
    fn constant_max_lang_chars_is_35() {
        assert_eq!(MAX_LANG_CHARS, 35);
    }

    #[test]
    fn constant_search_llm_max_tokens_per_leg_matches_saas_mirror() {
        // Mirrored in crw-saas/src/lib/llm-pricing.ts::legCost; changing this
        // without updating the SaaS credit pre-reservation would let real
        // usage exceed what was reserved.
        assert_eq!(SEARCH_LLM_MAX_TOKENS_PER_LEG, 1024);
    }

    #[test]
    fn constant_default_max_chars_per_source_is_8192() {
        assert_eq!(DEFAULT_MAX_CHARS_PER_SOURCE, 8192);
    }

    #[test]
    fn constant_search_enrich_deadline_max_ms_is_60s() {
        assert_eq!(SEARCH_ENRICH_DEADLINE_MAX_MS, 60_000);
    }

    #[test]
    fn degraded_message_text_is_stable() {
        // Several rescue-warning assertions across the pipeline (and the SaaS
        // side) match on this exact string; changing the copy silently would
        // desync those checks.
        assert_eq!(
            DEGRADED_MESSAGE,
            "The search backend could not answer this query. Retry shortly."
        );
    }

    #[test]
    fn is_valid_lang_rejects_subtag_over_8_chars() {
        assert!(!is_valid_lang("en-toolongsubtagvalue"));
    }

    #[test]
    fn is_valid_lang_rejects_non_alphanumeric_subtag() {
        assert!(!is_valid_lang("en-US!"));
    }

    #[test]
    fn fold_prescraped_multiple_entries_map_to_correct_slots() {
        let mut a = bare_result("https://example.com/a");
        a.markdown = Some("# a".into());
        a.metadata = Some(
            serde_json::from_value(serde_json::json!({
                "sourceURL": "https://example.com/a", "statusCode": 200, "elapsedMs": 1
            }))
            .expect("valid metadata"),
        );
        let mut b = bare_result("https://example.com/b");
        b.markdown = Some("# b".into());
        b.metadata = Some(
            serde_json::from_value(serde_json::json!({
                "sourceURL": "https://example.com/b", "statusCode": 200, "elapsedMs": 1
            }))
            .expect("valid metadata"),
        );

        let mut pool = vec![
            bare_result("https://example.com/b"),
            bare_result("https://example.com/a"),
        ];
        fold_prescraped(&mut pool, &[a, b]);
        // Order in `pool` is unchanged; each slot gets its OWN matching entry.
        assert_eq!(pool[0].url, "https://example.com/b");
        assert_eq!(pool[0].markdown.as_deref(), Some("# b"));
        assert_eq!(pool[1].url, "https://example.com/a");
        assert_eq!(pool[1].markdown.as_deref(), Some("# a"));
    }

    #[test]
    fn drop_known_urls_is_exact_string_match_not_normalized() {
        // A trailing-slash variant of a known URL is NOT deduped: the pool
        // dedups by exact URL string, so this documents current behavior
        // rather than a normalization guarantee.
        let known = {
            let mut r = bare_result("https://example.com/known");
            r.markdown = Some("# known".into());
            r
        };
        let data = SearchData::Flat(vec![known]);
        let rows = vec![bare_result("https://example.com/known/")];
        let fresh = drop_known_urls(&data, rows);
        assert_eq!(fresh.len(), 1, "trailing-slash variant is a distinct URL");
    }

    #[test]
    fn validate_rejects_scrape_options_with_first_bad_format_in_list() {
        let mut r = req("rust");
        let mut opts = scrape_opts(None);
        opts.formats = vec![
            OutputFormat::Markdown,
            OutputFormat::Json,
            OutputFormat::PlainText,
        ];
        r.scrape_options = Some(opts);
        assert!(matches!(
            validate_request(&r, 20),
            Err(CrwError::InvalidRequest(_))
        ));
    }

    #[test]
    fn summary_fanout_count_default_is_zero() {
        let c = SummaryFanoutCount::default();
        assert_eq!(c.ok, 0);
        assert_eq!(c.failed, 0);
    }
}
