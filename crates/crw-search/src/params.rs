//! Translate a public [`SearchRequest`] into SearXNG query parameters.
//!
//! Direct port of `crw-saas/src/lib/search-params.ts:mapToSearxngParams`.
//! Fixture-tested in `tests/transform_tests.rs` for byte-for-byte parity
//! with the SaaS implementation.
//!
//! [`SearchRequest`]: crw_core::types::SearchRequest

use crw_core::config::SearchConfig;
use crw_core::types::{SearchCategory, SearchRequest, SearchSource};

/// Owned representation of the SearXNG query parameters we send. The client
/// constructs the URL-encoded form from these fields; this struct stays
/// JSON-neutral so it's easy to assert on in tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearxngParams {
    pub q: String,
    pub categories: Option<String>,
    pub language: Option<String>,
    pub time_range: Option<String>,
    pub engines: Option<String>,
    pub pageno: Option<u32>,
    pub safesearch: Option<u8>,
    /// Does this request carry an entitlement to a PAID rescue tier in the
    /// backend, if the free tiers come back empty?
    ///
    /// Deliberately not a query parameter: it is a property of the caller's
    /// account, not of the query, so it travels as a header
    /// (`X-Crw-Paid-Rescue`) and never widens the search surface or the
    /// backend's cache key. Only crw-saas can set it — it decides from the
    /// plan/role it alone can see — and it defaults to false everywhere else,
    /// so self-hosters, the CLI, MCP and the research legs never spend.
    pub paid_rescue: bool,
}

/// Map the public [`SearchRequest`] to SearXNG query parameters.
///
/// Replicates SaaS quirks faithfully:
/// - `categories: ["pdf"]` appends ` filetype:pdf` to the query (not engines).
/// - `categories: ["github"]` adds the `github` engine.
/// - `categories: ["research"]` expands to `arxiv,crossref,google scholar,semantic scholar`
///   (configurable via `[search].research_engines`).
/// - Any other `categories` value (e.g. `science`, `it`, `news`) is forwarded
///   verbatim to SearXNG's native `categories` param, merged with the
///   `sources`-derived categories — SearXNG owns the engine→category routing.
/// - `tbs: "qdr:h"` is mapped to SearXNG `time_range=day` — SearXNG has no
///   hour granularity. (See `SearchTimeFilter::searxng_time_range`.)
///
/// [`SearchRequest`]: crw_core::types::SearchRequest
/// Leading filler tokens that SearXNG's `bing` engine keyword-matches into
/// dictionary / shopping junk ("top"/"best"/...). Lowercased.
const LEADING_FILLER: &[&str] = &["top", "best", "good", "greatest", "finest", "cheapest"];

/// Strip a leading filler token ("best restaurants ..." → "restaurants ...")
/// so SearXNG doesn't keyword-match the stopword into definition pages.
///
/// Only fires when ALL hold, to stay conservative:
/// - the query has >= 3 whitespace-separated tokens (single phrases are left
///   intact — "best buy" must not become "buy"),
/// - the first token (lowercased) is in [`LEADING_FILLER`],
/// - the query is not a quoted / operator query (a `"` or `:` anywhere, e.g.
///   `"top gun" movie` or `site:imdb.com`) — those are intentional.
///
/// Returns the original string when no rule applies.
pub fn clean_query(query: &str) -> String {
    let trimmed = query.trim();
    if trimmed.contains('"') || trimmed.contains(':') {
        return query.to_string();
    }
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.len() < 3 {
        return query.to_string();
    }
    if LEADING_FILLER.contains(&tokens[0].to_lowercase().as_str()) {
        tokens[1..].join(" ")
    } else {
        query.to_string()
    }
}

pub fn map_to_searxng_params(req: &SearchRequest, config: &SearchConfig) -> SearxngParams {
    let mut query = clean_query(&req.query);
    let mut engines: Vec<String> = Vec::new();
    // Native SearXNG categories collected from passthrough `Other(..)` values.
    // Merged with the `sources`-derived categories below into one param.
    let mut passthrough_cats: Vec<String> = Vec::new();

    if let Some(cats) = &req.categories {
        for cat in cats {
            match cat {
                SearchCategory::Pdf => {
                    query.push_str(" filetype:pdf");
                }
                SearchCategory::Github => {
                    engines.extend(config.github_engines.iter().cloned());
                }
                SearchCategory::Research => {
                    engines.extend(config.research_engines.iter().cloned());
                }
                // Unknown value: hand it to SearXNG's own category routing
                // (`science`, `it`, `news`, `files`, ...) instead of failing.
                SearchCategory::Other(name) => passthrough_cats.push(name.clone()),
            }
        }
    }

    // `categories` is the union of the `sources`-derived buckets and any
    // passthrough category names, de-duplicated while preserving order.
    // SearXNG unions `engines` with every engine of each category, so when a
    // curated category (github / research) already picked the engines, the
    // Web source's `general` would pull in all general engines and bury them.
    let mut category_names: Vec<String> = req
        .sources
        .as_ref()
        .map(|srcs| {
            srcs.iter()
                .filter(|s| engines.is_empty() || **s != SearchSource::Web)
                .map(|s| s.searxng_category().to_string())
                .collect()
        })
        .unwrap_or_default();
    for name in passthrough_cats {
        if !category_names.contains(&name) {
            category_names.push(name);
        }
    }
    let categories = if category_names.is_empty() {
        None
    } else {
        Some(category_names.join(","))
    };

    let time_range = req.tbs.map(|t| t.searxng_time_range().to_string());
    // Pin language to "en" when the request omits it (or sends empty), so
    // SearXNG doesn't fall back to a locale-mixed result set that pollutes the
    // re-rank pool. An explicit per-request language is always honored.
    let language = match req.lang.as_deref().map(str::trim) {
        Some(l) if !l.is_empty() => Some(l.to_string()),
        _ => Some("en".to_string()),
    };
    let engines = if engines.is_empty() {
        None
    } else {
        Some(engines.join(","))
    };

    SearxngParams {
        q: query,
        categories,
        language,
        time_range,
        engines,
        pageno: None,
        safesearch: None,
        paid_rescue: req.paid_rescue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crw_core::types::SearchTimeFilter;

    fn cfg() -> SearchConfig {
        SearchConfig::default()
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
    fn paid_rescue_travels_from_request_to_params() {
        // The entitlement has to survive the mapper, or the header never gets
        // sent and the backend silently keeps returning nothing.
        let mut r = req("rust async");
        assert!(!map_to_searxng_params(&r, &cfg()).paid_rescue);
        r.paid_rescue = true;
        assert!(map_to_searxng_params(&r, &cfg()).paid_rescue);
    }

    #[test]
    fn paid_rescue_cannot_be_set_from_the_request_body() {
        // `serde(skip)`: a caller sending the field must NOT be able to grant
        // itself a metered tier. This is the whole reason the flag is not a
        // normal request field.
        let r: SearchRequest =
            serde_json::from_str(r#"{"query":"x","paidRescue":true,"paid_rescue":true}"#).unwrap();
        assert!(!r.paid_rescue);
    }

    #[test]
    fn plain_query_is_passed_through() {
        let p = map_to_searxng_params(&req("rust async"), &cfg());
        assert_eq!(p.q, "rust async");
        assert!(p.categories.is_none());
        assert!(p.engines.is_none());
        assert!(p.time_range.is_none());
    }

    #[test]
    fn pdf_category_modifies_query_only() {
        let mut r = req("rust");
        r.categories = Some(vec![SearchCategory::Pdf]);
        let p = map_to_searxng_params(&r, &cfg());
        assert_eq!(p.q, "rust filetype:pdf");
        assert!(p.engines.is_none());
    }

    #[test]
    fn github_category_sets_engines() {
        let mut r = req("rust");
        r.categories = Some(vec![SearchCategory::Github]);
        let p = map_to_searxng_params(&r, &cfg());
        assert_eq!(p.engines.as_deref(), Some("github"));
        assert_eq!(p.q, "rust");
    }

    #[test]
    fn research_category_expands_to_default_engines() {
        let mut r = req("transformers");
        r.categories = Some(vec![SearchCategory::Research]);
        let p = map_to_searxng_params(&r, &cfg());
        assert_eq!(
            p.engines.as_deref(),
            Some("arxiv,crossref,google scholar,semantic scholar")
        );
    }

    #[test]
    fn sources_join_to_searxng_categories() {
        let mut r = req("rust");
        r.sources = Some(vec![SearchSource::Web, SearchSource::News]);
        let p = map_to_searxng_params(&r, &cfg());
        assert_eq!(p.categories.as_deref(), Some("general,news"));
    }

    #[test]
    fn unknown_category_passes_through_to_searxng() {
        let mut r = req("crispr");
        r.categories = Some(vec![SearchCategory::Other("science".into())]);
        let p = map_to_searxng_params(&r, &cfg());
        // Passed to SearXNG's native category routing, not crw engines.
        assert_eq!(p.categories.as_deref(), Some("science"));
        assert!(p.engines.is_none());
        assert_eq!(p.q, "crispr");
    }

    #[test]
    fn curated_engines_are_not_diluted_by_the_web_source() {
        // `sources: ["web"]` + `categories: ["github"]` used to also send
        // `categories=general`, and SearXNG's general engines outranked GitHub.
        let mut r = req("rust web framework");
        r.sources = Some(vec![SearchSource::Web]);
        r.categories = Some(vec![SearchCategory::Github]);
        let p = map_to_searxng_params(&r, &cfg());
        assert_eq!(p.engines.as_deref(), Some("github"));
        assert!(p.categories.is_none());

        // Other buckets keep their category.
        r.sources = Some(vec![SearchSource::Web, SearchSource::News]);
        let p = map_to_searxng_params(&r, &cfg());
        assert_eq!(p.categories.as_deref(), Some("news"));
    }

    #[test]
    fn passthrough_categories_merge_with_sources() {
        let mut r = req("rust");
        r.sources = Some(vec![SearchSource::Web]);
        r.categories = Some(vec![
            SearchCategory::Other("it".into()),
            SearchCategory::Other("science".into()),
        ]);
        let p = map_to_searxng_params(&r, &cfg());
        assert_eq!(p.categories.as_deref(), Some("general,it,science"));
    }

    #[test]
    fn curated_and_passthrough_categories_coexist() {
        let mut r = req("memory safety");
        r.categories = Some(vec![
            SearchCategory::Research,
            SearchCategory::Other("it".into()),
        ]);
        let p = map_to_searxng_params(&r, &cfg());
        // `research` still drives curated engines (Firecrawl behavior intact)...
        assert_eq!(
            p.engines.as_deref(),
            Some("arxiv,crossref,google scholar,semantic scholar")
        );
        // ...while `it` is forwarded as a native SearXNG category.
        assert_eq!(p.categories.as_deref(), Some("it"));
    }

    #[test]
    fn passthrough_category_dedupes_against_sources() {
        let mut r = req("rust");
        r.sources = Some(vec![SearchSource::News]);
        r.categories = Some(vec![SearchCategory::Other("news".into())]);
        let p = map_to_searxng_params(&r, &cfg());
        assert_eq!(p.categories.as_deref(), Some("news"));
    }

    #[test]
    fn category_string_roundtrip_keeps_curated_and_passthrough() {
        // Curated names deserialize to their typed variants; everything else
        // becomes Other(..) and survives a JSON round-trip verbatim.
        let cats: Vec<SearchCategory> =
            serde_json::from_str(r#"["research","science","github"]"#).unwrap();
        assert_eq!(
            cats,
            vec![
                SearchCategory::Research,
                SearchCategory::Other("science".into()),
                SearchCategory::Github,
            ]
        );
        let back = serde_json::to_string(&cats).unwrap();
        assert_eq!(back, r#"["research","science","github"]"#);
    }

    #[test]
    fn tbs_hour_collapses_to_day() {
        let mut r = req("rust");
        r.tbs = Some(SearchTimeFilter::Hour);
        let p = map_to_searxng_params(&r, &cfg());
        assert_eq!(p.time_range.as_deref(), Some("day"));
    }

    #[test]
    fn tbs_year_maps_to_year() {
        let mut r = req("rust");
        r.tbs = Some(SearchTimeFilter::Year);
        let p = map_to_searxng_params(&r, &cfg());
        assert_eq!(p.time_range.as_deref(), Some("year"));
    }

    #[test]
    fn empty_lang_defaults_to_en() {
        let mut r = req("rust");
        r.lang = Some(String::new());
        let p = map_to_searxng_params(&r, &cfg());
        assert_eq!(p.language.as_deref(), Some("en"));
    }

    #[test]
    fn missing_lang_defaults_to_en() {
        let p = map_to_searxng_params(&req("rust"), &cfg());
        assert_eq!(p.language.as_deref(), Some("en"));
    }

    #[test]
    fn explicit_lang_is_honored() {
        let mut r = req("rust");
        r.lang = Some("de".into());
        let p = map_to_searxng_params(&r, &cfg());
        assert_eq!(p.language.as_deref(), Some("de"));
    }

    #[test]
    fn clean_query_strips_leading_best() {
        assert_eq!(
            clean_query("best restaurants in belgrade"),
            "restaurants in belgrade"
        );
    }

    #[test]
    fn clean_query_strips_leading_top() {
        assert_eq!(clean_query("top museums in vienna"), "museums in vienna");
    }

    #[test]
    fn clean_query_keeps_quoted_top_gun() {
        // Quoted / phrase-intent queries must survive untouched.
        assert_eq!(
            clean_query("\"top gun\" movie review"),
            "\"top gun\" movie review"
        );
    }

    #[test]
    fn clean_query_keeps_operator_query() {
        assert_eq!(
            clean_query("best site:imdb.com movie"),
            "best site:imdb.com movie"
        );
    }

    #[test]
    fn clean_query_keeps_short_query() {
        // < 3 tokens: "best buy" must not collapse to "buy".
        assert_eq!(clean_query("best buy"), "best buy");
        assert_eq!(clean_query("top gun"), "top gun");
    }

    #[test]
    fn clean_query_leaves_non_filler_leading_token() {
        assert_eq!(clean_query("python snake habitat"), "python snake habitat");
    }

    #[test]
    fn clean_query_applied_in_params() {
        let p = map_to_searxng_params(&req("best coffee shops in lisbon"), &cfg());
        assert_eq!(p.q, "coffee shops in lisbon");
    }

    #[test]
    fn pdf_plus_github_combine() {
        let mut r = req("memory");
        r.categories = Some(vec![SearchCategory::Pdf, SearchCategory::Github]);
        let p = map_to_searxng_params(&r, &cfg());
        assert_eq!(p.q, "memory filetype:pdf");
        assert_eq!(p.engines.as_deref(), Some("github"));
    }
}
