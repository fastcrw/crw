//! Re-ranking pipeline for the LLM "answer" / "summarize" search path.
//!
//! SearXNG's raw `.score` is rank-inverse and content-blind: a `bing` keyword
//! match on a stopword ("top" / "best" / "fix") lets dictionary, shopping, and
//! bot-check pages tie or outrank the real results, feeding junk to the LLM.
//!
//! The **default path is lexical-core**: drop junk (structural signatures +
//! a host blocklist), gate on query-term coverage, drop competing-region rows,
//! then order the survivors by SearXNG's raw score and dedupe by registrable
//! domain. This is the only variant the frozen 56-query benchmark
//! (`tests/fixtures/bench/{rerank,score}.py`) proves beats the raw-score
//! baseline (CleanRel 0.471->0.536, Recall 0.314->0.318, nDCG-mean
//! 0.227->0.231) with no junk regression.
//!
//! The composite RRF + BM25 + geo-score step was **removed from the default
//! path**: it *regresses* the baseline (Recall -9%, nDCG 0.227->0.221) because
//! our cross-engine overlap is near-zero (positions median = 1, so RRF is the
//! single worst variant). The `rrf` / `bm25_lite` / `geo_score` helpers are
//! retained (`#[allow(dead_code)]`) for a future config-gated experiment; the
//! benchmark is the gate.
//!
//! The graceful-degrade fallback keeps the junk filter applied (it only relaxes
//! the coverage / geo guards) so junk can never re-enter the top-N.
//!
//! No network, no heavy dependencies — `std` + the `url` crate already in the
//! workspace.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use crate::client::SearxngResult;

// ---- tunable knobs (mirror rerank.py) ----
// K_RRF / K1 / B feed the retained-but-disabled rrf/bm25 helpers. The composite
// weights (W_RRF/W_REL/W_GEO) were removed with the composite scoring step — the
// default path orders by raw score (see module docs).
const K_RRF: f64 = 60.0;
const K1: f64 = 1.2;
const B: f64 = 0.5;
const MIN_COVERAGE: f64 = 0.5;

/// Query stopwords. Leading filler ("top"/"best") plus connective tokens that
/// would dilute coverage / BM25 if treated as content terms. Mirrors
/// `score.py::STOPWORDS`.
pub static STOPWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "top", "best", "good", "greatest", "finest", "cheapest", "cheap", "the", "a", "an", "in",
        "of", "to", "for", "and", "or", "near", "how", "is", "are", "do", "does", "from", "with",
        "you", "your", "should", "per",
        "what",
        // NOTE: year literals ("2025"/"2026") removed — corpus-specific and they
        // rot annually. Kept in lockstep with score.py::STOPWORDS.
    ]
    .into_iter()
    .collect()
});

/// Host-exact junk signatures (dictionary / shopping / news-aggregator /
/// asset hosts). Mirrors `score.py::JUNK_HOSTS`.
static JUNK_HOSTS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "merriam-webster.com",
        "dictionary.cambridge.org",
        "usdictionary.com",
        "dictionary.com",
        "vocabulary.com",
        "thefreedictionary.com",
        "collinsdictionary.com",
        "wiktionary.org",
        "zara.com",
        "bestbuy.com",
        "ebay.com",
        "aliexpress.com",
        "foxnews.com",
        "apnews.com",
        "news.google.com",
        "culturedcode.com",
        "thingiverse.com",
        "apps.apple.com",
        "fix.com",
    ]
    .into_iter()
    .collect()
});

const JUNK_HOST_SUFFIXES: &[&str] = &["myshopify.com"];

/// A geo entry: tokens that confirm the intended region, and competing tokens
/// that mark a homonymous wrong region (e.g. "belgrad" forest near Istanbul).
struct GeoEntry {
    region: &'static [&'static str],
    competing: &'static [&'static str],
}

/// Ambiguous toponyms from the corpus. Mirrors `score.py::GEO`. The map key is
/// a token that, when present in the query, selects the entry.
static GEO: LazyLock<HashMap<&'static str, GeoEntry>> = LazyLock::new(|| {
    HashMap::from([
        (
            "belgrad",
            GeoEntry {
                region: &["belgrade", "beograd", "serbia"],
                competing: &["istanbul", "forest", "turkey", "maine", "lakes", "montana"],
            },
        ),
        (
            "lisbon",
            GeoEntry {
                region: &["lisbon", "lisboa", "portugal"],
                competing: &[],
            },
        ),
        (
            "kyoto",
            GeoEntry {
                region: &["kyoto", "japan"],
                competing: &[],
            },
        ),
        (
            "tbilisi",
            GeoEntry {
                region: &["tbilisi", "georgia"],
                competing: &["atlanta"],
            },
        ),
        (
            "danang",
            GeoEntry {
                region: &["nang", "danang", "vietnam"],
                competing: &[],
            },
        ),
        (
            "porto",
            GeoEntry {
                region: &["porto", "portugal"],
                competing: &[],
            },
        ),
        (
            "tokyo",
            GeoEntry {
                region: &["tokyo", "japan"],
                competing: &[],
            },
        ),
        (
            "oaxaca",
            GeoEntry {
                region: &["oaxaca", "mexico"],
                competing: &[],
            },
        ),
        (
            "zurich",
            GeoEntry {
                region: &["zurich", "switzerland", "swiss"],
                competing: &[],
            },
        ),
        (
            "vienna",
            GeoEntry {
                region: &["vienna", "austria", "wien"],
                competing: &["virginia"],
            },
        ),
    ])
});

/// Lowercase + strip combining diacritics (NFKD fold). Mirrors `score.py::norm`.
fn norm(s: &str) -> String {
    // We avoid pulling `unicode-normalization`; the corpus toponyms only need
    // ASCII-folding of the common Latin diacritics that appear in snippets.
    s.to_lowercase()
        .chars()
        .map(fold_diacritic)
        .collect::<String>()
}

/// Best-effort fold of a single combining-Latin character to its base letter.
/// Covers the accents present in the corpus (Beograd, São, Zürich, ...).
fn fold_diacritic(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        other => other,
    }
}

/// Tokenize on non-alphanumeric boundaries over the normalized string.
/// Mirrors `score.py::toks`.
fn toks(s: &str) -> Vec<String> {
    norm(s)
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Host of a URL, with a leading `www.` stripped. Mirrors `score.py::domain`.
fn domain(url: &str) -> String {
    // url.split("/")[2] in Python — the authority component.
    let host = url
        .split("//")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("")
        .split('@')
        .next_back()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_lowercase();
    host.strip_prefix("www.").unwrap_or(&host).to_string()
}

/// Last two labels of the host (registrable-ish). Mirrors
/// `score.py::registrable` — deliberately the same naive two-label rule so the
/// Rust dedupe matches the proven reference exactly. A full PSL would change
/// dedupe behavior on `co.uk`-style suffixes; none appear in the corpus and
/// the reference is the contract we're porting.
fn registrable(url: &str) -> String {
    let d = domain(url);
    let parts: Vec<&str> = d.split('.').collect();
    if parts.len() >= 2 {
        format!("{}.{}", parts[parts.len() - 2], parts[parts.len() - 1])
    } else {
        d
    }
}

fn url_of(r: &SearxngResult) -> &str {
    r.url.as_deref().unwrap_or("")
}

fn title_of(r: &SearxngResult) -> &str {
    r.title.as_deref().unwrap_or("")
}

fn content_of(r: &SearxngResult) -> &str {
    r.content.as_deref().unwrap_or("")
}

/// Reciprocal Rank Fusion contribution for one row. Mirrors `rerank.py::rrf`.
/// Reciprocal-rank fusion of a row's per-engine positions. DISABLED in the
/// default path (RRF regresses on our near-zero cross-engine overlap); retained
/// for a future config-gated experiment.
#[allow(dead_code)]
fn rrf(r: &SearxngResult) -> f64 {
    if r.positions.is_empty() {
        1.0 / (K_RRF + 1.0) // single unknown-rank vote
    } else {
        r.positions.iter().map(|&p| 1.0 / (K_RRF + p as f64)).sum()
    }
}

/// Build a min-max normalizer closure. Returns a constant 0.0 when the range
/// collapses, matching `rerank.py::minmax`. DISABLED in the default path
/// (only used by the retained RRF/BM25 scoring).
#[allow(dead_code)]
fn minmax(vals: &[f64]) -> impl Fn(f64) -> f64 {
    let lo = vals.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let rng = hi - lo;
    move |v: f64| if rng > 1e-9 { (v - lo) / rng } else { 0.0 }
}

/// Title-weighted (2x) token multiset for a row. Mirrors the doc construction
/// in `rerank.py::bm25_lite`. DISABLED in the default path.
#[allow(dead_code)]
fn doc_tokens(r: &SearxngResult) -> Vec<String> {
    let mut d = toks(title_of(r));
    d.extend(toks(title_of(r)));
    d.extend(toks(content_of(r)));
    d
}

/// BM25-lite relevance over the candidate set (df / idf computed across
/// candidates, k1/b fixed). Mirrors `rerank.py::bm25_lite`. DISABLED in the
/// default path (BM25 did not beat the lexical core on the benchmark).
#[allow(dead_code)]
fn bm25_lite(rows: &[&SearxngResult], important: &HashSet<String>) -> Vec<f64> {
    let docs: Vec<Vec<String>> = rows.iter().map(|r| doc_tokens(r)).collect();
    let n = docs.len().max(1) as f64;
    let avgdl = docs.iter().map(|d| d.len()).sum::<usize>() as f64 / n;
    let mut df: HashMap<&str, usize> = HashMap::new();
    for d in &docs {
        let uniq: HashSet<&str> = d.iter().map(String::as_str).collect();
        for t in uniq {
            *df.entry(t).or_insert(0) += 1;
        }
    }
    let n_docs = docs.len() as f64;
    docs.iter()
        .map(|d| {
            let dl = d.len() as f64;
            let mut rel = 0.0;
            for term in important {
                let tf = d.iter().filter(|t| t.as_str() == term.as_str()).count() as f64;
                if tf == 0.0 {
                    continue;
                }
                let dfi = *df.get(term.as_str()).unwrap_or(&0) as f64;
                let idf = (1.0 + (n_docs - dfi + 0.5) / (dfi + 0.5)).ln();
                rel += idf * (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * dl / avgdl.max(1.0)));
            }
            rel
        })
        .collect()
}

/// `true` if the row matches a junk signature. Mirrors `score.py::is_junk`.
fn is_junk(r: &SearxngResult) -> bool {
    let url = url_of(r);
    let d = domain(url);
    if JUNK_HOSTS.contains(d.as_str()) || JUNK_HOST_SUFFIXES.iter().any(|s| d.ends_with(s)) {
        return true;
    }
    let title = norm(title_of(r));
    // Dictionary / definition title pattern: a definition keyword in a short
    // (<= 6 token) title.
    let title_toks = toks(title_of(r));
    if title_toks.len() <= 6
        && [
            "definition",
            "meaning",
            "synonym",
            "synonyms",
            "antonym",
            "antonyms",
        ]
        .iter()
        .any(|kw| {
            title
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|w| w == *kw)
        })
    {
        return true;
    }
    // Bot-check / interstitial titles.
    for needle in [
        "just a moment",
        "attention required",
        "verify you are human",
        "are you a robot",
        "access denied",
        "enable javascript",
    ] {
        if title.contains(needle) {
            return true;
        }
    }
    // Asset-leak / non-content paths.
    let url_l = url.to_lowercase();
    url_l.contains("/mapfiles/")
        || url_l.contains("/apple-app-site-association/")
        || url_l.contains("/.well-known/")
}

/// Important-term coverage guard. Mirrors `score.py::covers`.
fn covers(r: &SearxngResult, important: &HashSet<String>) -> bool {
    if important.is_empty() {
        return true;
    }
    let mut doc: HashSet<String> = toks(title_of(r)).into_iter().collect();
    doc.extend(toks(content_of(r)));
    let hit = important.iter().filter(|t| doc.contains(*t)).count();
    hit as f64 / important.len() as f64 >= MIN_COVERAGE
}

/// Graded form of [`covers`]: the COUNT of important query terms present in a
/// row (title + content). Used by the relevance gate in [`rerank_relevance`] to
/// rank/keep rows by how many of the query's distinctive terms they actually
/// cover, rather than by raw upstream score alone.
fn coverage_count(r: &SearxngResult, important: &HashSet<String>) -> usize {
    if important.is_empty() {
        return 0;
    }
    let mut doc: HashSet<String> = toks(title_of(r)).into_iter().collect();
    doc.extend(toks(content_of(r)));
    important.iter().filter(|t| doc.contains(*t)).count()
}

/// `true` if a competing-region token appears anywhere in the row.
/// Mirrors `score.py::geo_competing`.
fn geo_competing(r: &SearxngResult, competing: &[&str]) -> bool {
    if competing.is_empty() {
        return false;
    }
    let blob = norm(&format!("{} {} {}", title_of(r), content_of(r), url_of(r)));
    competing.iter().any(|c| blob.contains(c))
}

/// Geo signal: +1 for an in-region token, -1 for a competing token.
/// Mirrors `rerank.py::geo_score`. DISABLED in the default path (the geo
/// *filter* `geo_competing` stays; only the geo *boost* is dropped).
#[allow(dead_code)]
fn geo_score(r: &SearxngResult, region: &[&str], competing: &[&str]) -> f64 {
    if region.is_empty() {
        return 0.0;
    }
    let blob = norm(&format!("{} {} {}", title_of(r), content_of(r), url_of(r)));
    let mut s = 0.0;
    if region.iter().any(|t| blob.contains(t)) {
        s += 1.0;
    }
    if !competing.is_empty() && competing.iter().any(|c| blob.contains(c)) {
        s -= 1.0;
    }
    s
}

/// Resolve the geo entry for a query, if any. Mirrors `score.py::geo_for`.
fn geo_for(query: &str) -> (&'static [&'static str], &'static [&'static str]) {
    let qn: HashSet<String> = toks(query).into_iter().collect();
    for (key, entry) in GEO.iter() {
        if qn.contains(*key) || (*key == "danang" && qn.contains("nang")) {
            return (entry.region, entry.competing);
        }
    }
    (&[], &[])
}

/// Important content terms of a query: tokens minus stopwords.
fn important_terms(query: &str) -> HashSet<String> {
    toks(query)
        .into_iter()
        .filter(|t| !STOPWORDS.contains(t.as_str()))
        .collect()
}

/// Run the full re-rank pipeline over raw SearXNG rows and return them ordered
/// best-first, deduped by registrable domain. Never returns empty unless
/// `rows` is empty (graceful degrade). Mirrors `rerank.py::rank_full` with the
/// junk filter always applied (including the degrade fallback).
///
/// This is the frozen lexical-core default path (raw-score ordering) proven on
/// the benchmark. For the relevance-gated variant, see [`rerank_relevance`].
pub fn rerank<'a>(rows: &'a [SearxngResult], query: &str) -> Vec<&'a SearxngResult> {
    rerank_core(rows, query, false)
}

/// Relevance-gated re-rank (config flag `rerank_relevance`, default off). Same
/// pipeline as [`rerank`], plus a final **coverage gate**: among the survivors,
/// keep rows whose important (non-stopword) query-term coverage is within ONE
/// term of the pool maximum (`>= max_cov - 1` once `max_cov >= 2`). So for
/// "best pizza in belgrade" — important terms `{pizza, belgrade}` — a genuine
/// "pizza … belgrade" row (coverage 2/2) is kept while a "pizza … REDMOND"
/// homonym (coverage 1/2) is evicted. The one-term slack (rather than a hard
/// `== max_cov`) keeps a strong result that misses exactly one query term from
/// being evicted by a lone keyword-stuffed spam row sitting at full coverage.
///
/// Deployment-agnostic by design: it ranks purely on the query's own
/// distinctive tokens, injecting NO geo / country / IP signal — so it behaves
/// identically whether crw is hosted in Belgrade, Redmond, or a datacenter
/// anywhere else (the self-host reality). Monotone-safe: the gate only fires
/// when a strictly-better-covered row exists, and never empties a non-empty
/// pool (the degrade fallback still applies first).
pub fn rerank_relevance<'a>(rows: &'a [SearxngResult], query: &str) -> Vec<&'a SearxngResult> {
    rerank_core(rows, query, true)
}

fn rerank_core<'a>(
    rows: &'a [SearxngResult],
    query: &str,
    relevance: bool,
) -> Vec<&'a SearxngResult> {
    if rows.is_empty() {
        return Vec::new();
    }
    let important = important_terms(query);
    // Only the competing-region *filter* runs in the default path; the geo
    // *boost* (geo_score, which would use `region`) is disabled.
    let (_region, competing) = geo_for(query);

    // STAGE2 junk filter is unconditional and survives the degrade fallback.
    let non_junk: Vec<&SearxngResult> = rows.iter().filter(|r| !is_junk(r)).collect();

    // STAGE3 coverage + geo-competing guards.
    let mut cands: Vec<&SearxngResult> = non_junk
        .iter()
        .copied()
        .filter(|r| covers(r, &important))
        .filter(|r| !geo_competing(r, competing))
        .collect();

    // DEGRADE: relax coverage / geo (but NOT junk). If even the non-junk pool
    // is empty (all rows were junk), fall back to the raw rows so we never
    // return empty on non-empty input.
    if cands.is_empty() {
        cands = if non_junk.is_empty() {
            rows.iter().collect()
        } else {
            non_junk
        };
    }

    // RELEVANCE GATE (config-gated, default off — see `rerank_relevance`). Keep
    // only rows whose important-term coverage EQUALS the pool maximum. This
    // evicts partial-match homonyms (the wrong-city "pizza" that misses the
    // location term) from the pool fed to the LLM, using only the query's own
    // tokens (no geo database) — the feature's whole purpose. Among the kept
    // max-coverage rows the prior raw-score ordering still decides rank. Skipped
    // when there are no important terms or nothing covers > 0 (degrade-safe; the
    // gate can never empty a non-empty pool).
    if relevance && !important.is_empty() {
        // Compute coverage once per row (used for both max and the filter).
        let covs: Vec<usize> = cands
            .iter()
            .map(|r| coverage_count(r, &important))
            .collect();
        let max_cov = covs.iter().copied().max().unwrap_or(0);
        if max_cov > 0 {
            let filtered: Vec<&SearxngResult> = cands
                .iter()
                .copied()
                .zip(covs.iter().copied())
                .filter(|&(_, c)| c == max_cov)
                .map(|(r, _)| r)
                .collect();
            if !filtered.is_empty() {
                cands = filtered;
            }
        }
    }

    // LEXICAL-CORE ordering. The filters above already dropped junk /
    // uncovered / competing-region rows; order the survivors by SearXNG's raw
    // score (stable sort, so equal scores keep upstream order) and dedupe by
    // registrable domain, keeping the highest-scored page per domain. The
    // composite RRF/BM25/geo-score step was removed because it regresses the
    // baseline on our data — see module docs.
    cands.sort_by(|a, b| {
        let sa = a.score.unwrap_or(0.0);
        let sb = b.score.unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<&SearxngResult> = Vec::with_capacity(cands.len());
    for r in cands {
        let rd = registrable(url_of(r));
        if !seen.insert(rd) {
            continue;
        }
        out.push(r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(url: &str, title: &str, content: &str, positions: Vec<u32>) -> SearxngResult {
        SearxngResult {
            url: Some(url.into()),
            title: Some(title.into()),
            engine: Some("test".into()),
            content: Some(content.into()),
            score: Some(1.0),
            engines: Vec::new(),
            positions,
            category: Some("general".into()),
            template: None,
            published_date: None,
            img_src: None,
            thumbnail_src: None,
            img_format: None,
            resolution: None,
        }
    }

    #[test]
    fn domain_strips_www_and_port() {
        assert_eq!(domain("https://www.Example.com:8080/path"), "example.com");
        assert_eq!(domain("http://sub.example.org/x"), "sub.example.org");
    }

    #[test]
    fn registrable_takes_last_two_labels() {
        assert_eq!(
            registrable("https://dictionary.cambridge.org/x"),
            "cambridge.org"
        );
        assert_eq!(
            registrable("https://www.tripadvisor.com/y"),
            "tripadvisor.com"
        );
    }

    #[test]
    fn junk_dictionary_host_dropped() {
        let r = row(
            "https://www.merriam-webster.com/dictionary/best",
            "best Definition",
            "",
            vec![1],
        );
        assert!(is_junk(&r));
    }

    #[test]
    fn junk_bot_check_title_dropped() {
        let r = row("https://example.com/", "Just a moment...", "", vec![1]);
        assert!(is_junk(&r));
    }

    #[test]
    fn non_junk_real_result_kept() {
        let r = row(
            "https://www.tripadvisor.com/Restaurants-Belgrade.html",
            "THE 10 BEST Restaurants in Belgrade",
            "best restaurants in belgrade serbia",
            vec![1],
        );
        assert!(!is_junk(&r));
    }

    #[test]
    fn dedupe_by_registrable_domain() {
        let rows = vec![
            row("https://a.com/1", "alpha beta", "alpha beta", vec![1]),
            row("https://a.com/2", "alpha beta", "alpha beta", vec![2]),
            row("https://b.com/1", "alpha beta", "alpha beta", vec![3]),
        ];
        let out = rerank(&rows, "alpha beta");
        let doms: Vec<String> = out.iter().map(|r| registrable(url_of(r))).collect();
        assert_eq!(doms, vec!["a.com", "b.com"]);
    }

    #[test]
    fn degrade_never_returns_empty_when_coverage_fails() {
        // No row covers the important terms, but they're not junk → degrade.
        let rows = vec![
            row("https://a.com/1", "unrelated", "nothing matches", vec![1]),
            row(
                "https://b.com/1",
                "also unrelated",
                "still nothing",
                vec![2],
            ),
        ];
        let out = rerank(&rows, "quantum chromodynamics lattice");
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn empty_input_returns_empty() {
        let rows: Vec<SearxngResult> = Vec::new();
        assert!(rerank(&rows, "anything").is_empty());
    }

    #[test]
    fn junk_never_leaks_through_degrade() {
        // All non-junk rows fail coverage; degrade must still drop junk.
        let rows = vec![
            row(
                "https://www.merriam-webster.com/dictionary/best",
                "best Definition",
                "best",
                vec![1],
            ),
            row("https://real.com/1", "unrelated", "no match here", vec![2]),
        ];
        let out = rerank(&rows, "quantum chromodynamics");
        assert!(out.iter().all(|r| !is_junk(r)));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn relevance_gate_keeps_max_coverage_drops_zero_coverage() {
        // (a) full-coverage row kept; 0-coverage row dropped.
        let rows = vec![
            row(
                "https://a.com/1",
                "pizza in belgrade",
                "great pizza belgrade serbia",
                vec![1],
            ),
            row(
                "https://b.com/1",
                "completely unrelated topic",
                "nothing here at all",
                vec![2],
            ),
        ];
        let out = rerank_relevance(&rows, "best pizza in belgrade");
        let doms: Vec<String> = out.iter().map(|r| registrable(url_of(r))).collect();
        assert!(doms.contains(&"a.com".to_string()));
        assert!(!doms.contains(&"b.com".to_string()));
    }

    #[test]
    fn relevance_gate_evicts_below_max_coverage() {
        // Hard gate: only rows at the MAXIMUM important-term coverage survive.
        // A partial-coverage row (covers 2/3, missing one term — the wrong-
        // context homonym) is evicted along with the zero-coverage row. That
        // aggressive eviction is the feature's purpose: keep partial/wrong-
        // context matches out of the pool fed to the LLM.
        let rows = vec![
            // coverage 3/3 (rust, async, tokio) — the genuine full match.
            row(
                "https://full.com/1",
                "rust async tokio runtime",
                "a complete guide to rust async with tokio",
                vec![1],
            ),
            // coverage 2/3 (rust, async) — partial match, must be evicted.
            row(
                "https://partial.com/1",
                "rust async runtime guide",
                "deep dive into rust async programming",
                vec![2],
            ),
            // coverage 0/3 — must be dropped.
            row(
                "https://zero.com/1",
                "cooking recipes",
                "how to bake bread",
                vec![3],
            ),
        ];
        let out = rerank_relevance(&rows, "rust async tokio");
        let doms: Vec<String> = out.iter().map(|r| registrable(url_of(r))).collect();
        assert!(doms.contains(&"full.com".to_string()));
        assert!(
            !doms.contains(&"partial.com".to_string()),
            "partial-coverage row must be evicted by the hard max-coverage gate"
        );
        assert!(!doms.contains(&"zero.com".to_string()));
    }

    #[test]
    fn relevance_gate_noop_when_all_rows_equal_coverage() {
        // (c) all rows share the same coverage → gate keeps them all.
        let rows = vec![
            row(
                "https://a.com/1",
                "pizza belgrade",
                "pizza belgrade",
                vec![1],
            ),
            row(
                "https://b.com/1",
                "pizza belgrade",
                "pizza belgrade",
                vec![2],
            ),
            row(
                "https://c.com/1",
                "pizza belgrade",
                "pizza belgrade",
                vec![3],
            ),
        ];
        let out = rerank_relevance(&rows, "best pizza in belgrade");
        let doms: Vec<String> = out.iter().map(|r| registrable(url_of(r))).collect();
        assert_eq!(doms.len(), 3);
        assert!(doms.contains(&"a.com".to_string()));
        assert!(doms.contains(&"b.com".to_string()));
        assert!(doms.contains(&"c.com".to_string()));
    }

    #[test]
    fn relevance_gate_degrade_safe_with_no_important_terms() {
        // (d) a query with only stopwords yields no important terms → gate is
        // skipped and the non-empty pool is preserved.
        let rows = vec![
            row("https://a.com/1", "alpha", "alpha content", vec![1]),
            row("https://b.com/1", "beta", "beta content", vec![2]),
        ];
        let out = rerank_relevance(&rows, "the of in and a");
        assert!(!out.is_empty());
        assert_eq!(out.len(), 2);
    }

    // --- norm / fold_diacritic ---

    #[test]
    fn norm_lowercases_and_folds_common_latin_diacritics() {
        assert_eq!(norm("Beograd São Zürich"), "beograd sao zurich");
    }

    #[test]
    fn norm_leaves_unmapped_diacritics_unchanged() {
        // Only the corpus's common Latin accents are folded (see the module
        // comment); a character like "ø" has no mapping and passes through.
        assert_eq!(norm("Malmø"), "malmø");
    }

    #[test]
    fn norm_passes_non_latin_scripts_through() {
        assert_eq!(norm("東京 Tokyo"), "東京 tokyo");
    }

    #[test]
    fn norm_empty_string_is_empty() {
        assert_eq!(norm(""), "");
    }

    // --- toks ---

    #[test]
    fn toks_splits_on_punctuation_and_drops_empty_segments() {
        assert_eq!(toks("rust, async--tokio!!"), vec!["rust", "async", "tokio"]);
    }

    #[test]
    fn toks_lowercases_input() {
        assert_eq!(toks("RUST Async"), vec!["rust", "async"]);
    }

    // BUG: `toks` splits on `!c.is_ascii_alphanumeric()`, so any non-ASCII
    // character (every CJK codepoint) is treated as a separator rather than
    // as part of a token. A CJK-only string therefore tokenizes to nothing,
    // silently dropping all content instead of producing CJK tokens.
    #[test]
    fn toks_cjk_only_content_yields_no_tokens() {
        assert!(toks("日本語のタイトル").is_empty());
    }

    // BUG: same root cause as above — the CJK portion of a mixed string
    // vanishes entirely rather than surviving as its own token(s).
    #[test]
    fn toks_mixed_ascii_and_cjk_drops_the_cjk_portion() {
        assert_eq!(toks("hello 世界 world"), vec!["hello", "world"]);
    }

    #[test]
    fn toks_handles_a_very_long_string_without_panicking() {
        let long = "word ".repeat(5_000);
        let out = toks(&long);
        assert_eq!(out.len(), 5_000);
        assert!(out.iter().all(|t| t == "word"));
    }

    #[test]
    fn toks_keeps_mixed_alphanumeric_tokens_together() {
        assert_eq!(toks("a 3d model v2"), vec!["a", "3d", "model", "v2"]);
    }

    // --- domain ---

    #[test]
    fn domain_handles_userinfo_in_the_authority() {
        assert_eq!(
            domain("https://user:pass@Host.Example.com/x"),
            "host.example.com"
        );
    }

    #[test]
    fn domain_empty_for_a_url_without_a_double_slash() {
        assert_eq!(domain("not-a-url"), "");
    }

    #[test]
    fn domain_works_on_a_scheme_relative_url() {
        assert_eq!(domain("//Example.com/path"), "example.com");
    }

    #[test]
    fn domain_empty_path_after_host_is_fine() {
        assert_eq!(domain("https://example.com"), "example.com");
    }

    // --- registrable ---

    #[test]
    fn registrable_keeps_a_single_label_host_unchanged() {
        assert_eq!(registrable("http://localhost:8080/x"), "localhost");
    }

    #[test]
    fn registrable_uses_the_naive_last_two_labels_on_a_multi_label_suffix() {
        // Documented limitation (see the function's doc comment): no public
        // suffix list, so a `co.uk`-style host collapses to "co.uk", not the
        // real registrable domain. Locking in the current, intentional rule.
        assert_eq!(registrable("https://www.bbc.co.uk/news"), "co.uk");
    }

    // --- is_junk ---

    #[test]
    fn is_junk_matches_a_myshopify_suffix() {
        let r = row("https://mystore.myshopify.com/", "My Store", "", vec![1]);
        assert!(is_junk(&r));
    }

    #[test]
    fn is_junk_ignores_a_domain_that_merely_ends_with_the_shopify_suffix() {
        // `ends_with` is a raw string suffix check, not a subdomain boundary
        // check, so any host literally ending in "myshopify.com" is caught
        // even when it isn't on the Shopify platform. Documents the coarse
        // match rather than a per-label comparison.
        let r = row(
            "https://totallynotmyshopify.com/",
            "Totally Not Shopify",
            "",
            vec![1],
        );
        assert!(is_junk(&r));
    }

    #[test]
    fn is_junk_asset_leak_paths_are_dropped() {
        for path in [
            "https://example.com/mapfiles/marker.png",
            "https://example.com/apple-app-site-association/",
            "https://example.com/.well-known/security.txt",
        ] {
            let r = row(path, "Some Title", "some content", vec![1]);
            assert!(is_junk(&r), "{path} should be flagged as an asset leak");
        }
    }

    #[test]
    fn is_junk_dictionary_keyword_ignored_once_title_exceeds_six_tokens() {
        // The dictionary-title heuristic only fires on short (<=6 token)
        // titles; a longer title carrying "definition" as an incidental word
        // must not be flagged.
        let r = row(
            "https://real-blog.com/post",
            "A Complete Definition of Rust Ownership and Borrowing Rules",
            "an in-depth guide",
            vec![1],
        );
        assert!(!is_junk(&r));
    }

    #[test]
    fn is_junk_bot_check_title_match_is_case_insensitive() {
        let r = row("https://example.com/", "JUST A MOMENT...", "", vec![1]);
        assert!(is_junk(&r));
    }

    #[test]
    fn is_junk_false_for_a_url_missing_a_scheme() {
        let r = row("not-a-url", "A Real Result", "real content here", vec![1]);
        assert!(!is_junk(&r));
    }

    // --- covers / coverage_count ---

    #[test]
    fn covers_is_true_with_no_important_terms() {
        let r = row("https://a.com/", "anything", "anything", vec![1]);
        assert!(covers(&r, &HashSet::new()));
    }

    #[test]
    fn covers_boundary_at_exactly_min_coverage() {
        let important: HashSet<String> = ["pizza".to_string(), "belgrade".to_string()]
            .into_iter()
            .collect();
        // 1 of 2 terms = 0.5, and MIN_COVERAGE is inclusive.
        let r = row(
            "https://a.com/",
            "pizza reviews",
            "only pizza, no city",
            vec![1],
        );
        assert!(covers(&r, &important));
    }

    #[test]
    fn covers_false_just_below_min_coverage() {
        let important: HashSet<String> = ["a".to_string(), "b".to_string(), "c".to_string()]
            .into_iter()
            .collect();
        // 1 of 3 = 0.333, below the 0.5 threshold.
        let r = row("https://a.com/", "a only", "a only", vec![1]);
        assert!(!covers(&r, &important));
    }

    #[test]
    fn coverage_count_counts_distinct_terms_not_occurrences() {
        let important: HashSet<String> = ["pizza".to_string(), "belgrade".to_string()]
            .into_iter()
            .collect();
        let r = row(
            "https://a.com/",
            "pizza pizza pizza",
            "pizza everywhere, no city mentioned",
            vec![1],
        );
        assert_eq!(coverage_count(&r, &important), 1);
    }

    #[test]
    fn coverage_count_zero_with_no_important_terms() {
        let r = row("https://a.com/", "anything", "anything", vec![1]);
        assert_eq!(coverage_count(&r, &HashSet::new()), 0);
    }

    // --- geo_for / geo_competing ---

    #[test]
    fn geo_for_returns_empty_for_an_unrelated_query() {
        let (region, competing) = geo_for("best pizza recipe");
        assert!(region.is_empty());
        assert!(competing.is_empty());
    }

    // BUG: the GEO table's key for this entry is the literal token "belgrad"
    // (German spelling), and `geo_for` matches only via exact token equality
    // (`qn.contains(*key)`), with a hardcoded special case ONLY for "danang".
    // A query using the ordinary English spelling "belgrade" never produces
    // the token "belgrad", so the entry — whose whole stated purpose is
    // guarding "belgrade" pizza queries against the Istanbul-forest homonym
    // (see the module's `GEO` doc comment) — never actually fires for that
    // spelling.
    #[test]
    fn geo_for_belgrade_spelling_does_not_trigger_the_belgrad_entry() {
        let (region, competing) = geo_for("best pizza in belgrade");
        assert!(region.is_empty());
        assert!(competing.is_empty());
    }

    #[test]
    fn geo_for_matches_the_literal_belgrad_spelling() {
        let (region, competing) = geo_for("restaurants in belgrad");
        assert_eq!(region, &["belgrade", "beograd", "serbia"]);
        assert!(competing.contains(&"istanbul"));
    }

    #[test]
    fn geo_for_danang_matches_via_the_two_word_nang_token() {
        // "Da Nang" tokenizes to ["da", "nang"]; the explicit special case
        // for this entry matches on the "nang" token even without the exact
        // "danang" key present.
        let (region, _competing) = geo_for("things to do in da nang");
        assert_eq!(region, &["nang", "danang", "vietnam"]);
    }

    #[test]
    fn geo_competing_false_when_the_competing_list_is_empty() {
        let r = row("https://a.com/", "lisbon guide", "lisbon portugal", vec![1]);
        assert!(!geo_competing(&r, &[]));
    }

    #[test]
    fn geo_competing_detects_a_token_anywhere_in_title_content_or_url() {
        let in_title = row("https://a.com/", "istanbul forest walk", "trees", vec![1]);
        let in_content = row("https://a.com/", "a forest walk", "near istanbul", vec![1]);
        let in_url = row("https://istanbul-guide.com/", "a walk", "trees", vec![1]);
        for r in [in_title, in_content, in_url] {
            assert!(geo_competing(&r, &["istanbul"]));
        }
    }

    // --- rrf / minmax / doc_tokens / bm25_lite (disabled but retained) ---

    #[test]
    fn rrf_uses_the_single_vote_constant_with_no_positions() {
        let r = row("https://a.com/", "t", "c", vec![]);
        assert_eq!(rrf(&r), 1.0 / (K_RRF + 1.0));
    }

    #[test]
    fn rrf_sums_reciprocal_rank_over_multiple_positions() {
        let r = row("https://a.com/", "t", "c", vec![1, 2]);
        let expected = 1.0 / (K_RRF + 1.0) + 1.0 / (K_RRF + 2.0);
        assert!((rrf(&r) - expected).abs() < 1e-12);
    }

    #[test]
    fn minmax_collapsed_range_returns_constant_zero() {
        let f = minmax(&[5.0, 5.0, 5.0]);
        assert_eq!(f(5.0), 0.0);
        assert_eq!(f(0.0), 0.0);
    }

    #[test]
    fn minmax_normalizes_into_the_unit_range() {
        let f = minmax(&[0.0, 10.0]);
        assert_eq!(f(0.0), 0.0);
        assert_eq!(f(10.0), 1.0);
        assert_eq!(f(5.0), 0.5);
    }

    #[test]
    fn doc_tokens_weights_the_title_twice() {
        let r = row("https://a.com/", "rust guide", "async tokio", vec![1]);
        let d = doc_tokens(&r);
        assert_eq!(d.iter().filter(|t| t.as_str() == "rust").count(), 2);
        assert_eq!(d.iter().filter(|t| t.as_str() == "guide").count(), 2);
        assert_eq!(d.iter().filter(|t| t.as_str() == "async").count(), 1);
    }

    #[test]
    fn bm25_lite_zero_relevance_with_no_important_terms() {
        let a = row("https://a.com/", "rust async", "tokio runtime", vec![1]);
        let b = row("https://b.com/", "cooking", "recipes", vec![2]);
        let rows = [&a, &b];
        let scores = bm25_lite(&rows, &HashSet::new());
        assert_eq!(scores, vec![0.0, 0.0]);
    }

    // --- important_terms ---

    #[test]
    fn important_terms_strips_stopwords_case_insensitively() {
        let terms = important_terms("How Do You Make The Best Pizza");
        assert_eq!(
            terms,
            HashSet::from(["make".to_string(), "pizza".to_string()])
        );
    }

    #[test]
    fn important_terms_empty_when_the_whole_query_is_stopwords() {
        assert!(important_terms("the of in and a").is_empty());
    }

    // --- rerank / rerank_relevance: determinism, ties, edge inputs ---

    #[test]
    fn rerank_is_deterministic_across_repeated_calls() {
        let rows = vec![
            row("https://a.com/1", "alpha beta", "alpha beta", vec![1]),
            row("https://b.com/1", "alpha beta", "alpha beta", vec![2]),
            row("https://c.com/1", "alpha beta", "alpha beta", vec![3]),
        ];
        let first = rerank(&rows, "alpha beta");
        for _ in 0..5 {
            let again = rerank(&rows, "alpha beta");
            assert_eq!(
                again.iter().map(|r| url_of(r)).collect::<Vec<_>>(),
                first.iter().map(|r| url_of(r)).collect::<Vec<_>>(),
            );
        }
    }

    #[test]
    fn rerank_stable_sort_preserves_input_order_on_score_ties() {
        // All three rows share the default score of 1.0 from `row()`, so the
        // stable sort must keep them in their original input order.
        let rows = vec![
            row("https://a.com/1", "alpha beta", "alpha beta", vec![1]),
            row("https://b.com/1", "alpha beta", "alpha beta", vec![2]),
            row("https://c.com/1", "alpha beta", "alpha beta", vec![3]),
        ];
        let out = rerank(&rows, "alpha beta");
        assert_eq!(
            out.iter().map(|r| url_of(r)).collect::<Vec<_>>(),
            vec!["https://a.com/1", "https://b.com/1", "https://c.com/1"]
        );
    }

    #[test]
    fn rerank_single_item_pool_is_returned_unchanged() {
        let rows = vec![row("https://a.com/1", "alpha", "alpha content", vec![1])];
        let out = rerank(&rows, "alpha");
        assert_eq!(out.len(), 1);
        assert_eq!(url_of(out[0]), "https://a.com/1");
    }

    #[test]
    fn rerank_missing_score_ranks_below_a_positive_score() {
        let mut with_score = row("https://a.com/1", "alpha beta", "alpha beta", vec![1]);
        with_score.score = Some(0.9);
        let mut no_score = row("https://b.com/1", "alpha beta", "alpha beta", vec![2]);
        no_score.score = None;
        let rows = [with_score, no_score];
        let out = rerank(&rows, "alpha beta");
        assert_eq!(url_of(out[0]), "https://a.com/1");
        assert_eq!(url_of(out[1]), "https://b.com/1");
    }

    #[test]
    fn rerank_nan_score_does_not_panic_and_the_row_survives() {
        let mut nan_row = row("https://a.com/1", "alpha beta", "alpha beta", vec![1]);
        nan_row.score = Some(f64::NAN);
        let normal = row("https://b.com/1", "alpha beta", "alpha beta", vec![2]);
        let rows = [nan_row, normal];
        let out = rerank(&rows, "alpha beta");
        // `partial_cmp` on NaN returns None, which the sort comparator falls
        // back to `Equal` for — must not panic, and both distinct-domain rows
        // must survive regardless of the order NaN happens to land in.
        assert_eq!(out.len(), 2);
        let doms: HashSet<String> = out.iter().map(|r| registrable(url_of(r))).collect();
        assert_eq!(
            doms,
            HashSet::from(["a.com".to_string(), "b.com".to_string()])
        );
    }

    #[test]
    fn rerank_large_pool_sorts_descending_without_panicking() {
        let rows: Vec<SearxngResult> = (0..500)
            .map(|i| {
                let mut r = row(
                    &format!("https://site{i}.com/1"),
                    "alpha beta",
                    "alpha beta content",
                    vec![1],
                );
                r.score = Some(i as f64);
                r
            })
            .collect();
        let out = rerank(&rows, "alpha beta");
        assert_eq!(out.len(), 500);
        // Descending by score: site499 (score 499.0) must come first.
        assert_eq!(url_of(out[0]), "https://site499.com/1");
        assert_eq!(url_of(out[out.len() - 1]), "https://site0.com/1");
    }

    #[test]
    fn rerank_never_drops_a_distinct_domain_non_junk_covering_row() {
        let rows: Vec<SearxngResult> = (0..20)
            .map(|i| {
                row(
                    &format!("https://site{i}.com/1"),
                    "alpha beta guide",
                    "alpha beta content",
                    vec![1],
                )
            })
            .collect();
        let out = rerank(&rows, "alpha beta");
        assert_eq!(out.len(), 20);
    }

    #[test]
    fn rerank_cjk_query_does_not_panic_though_tokens_are_dropped() {
        // See the `toks` BUG notes above: a CJK query produces zero important
        // terms, so the coverage/geo guards become no-ops and every non-junk
        // row survives regardless of whether it actually matches the query.
        let rows = vec![
            row("https://a.com/1", "日本語のタイトル", "本文です", vec![1]),
            row(
                "https://b.com/1",
                "unrelated english title",
                "nothing to do with it",
                vec![2],
            ),
        ];
        let out = rerank(&rows, "日本語");
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn rerank_relevance_is_deterministic_across_repeated_calls() {
        let rows = vec![
            row(
                "https://a.com/1",
                "pizza belgrade",
                "pizza belgrade serbia",
                vec![1],
            ),
            row(
                "https://b.com/1",
                "pizza redmond",
                "pizza redmond washington",
                vec![2],
            ),
        ];
        let first = rerank_relevance(&rows, "best pizza in belgrade");
        for _ in 0..5 {
            let again = rerank_relevance(&rows, "best pizza in belgrade");
            assert_eq!(
                again.iter().map(|r| url_of(r)).collect::<Vec<_>>(),
                first.iter().map(|r| url_of(r)).collect::<Vec<_>>(),
            );
        }
    }

    #[test]
    fn rerank_relevance_single_item_pool_is_returned_unchanged() {
        let rows = vec![row(
            "https://a.com/1",
            "pizza belgrade",
            "pizza belgrade",
            vec![1],
        )];
        let out = rerank_relevance(&rows, "pizza belgrade");
        assert_eq!(out.len(), 1);
    }

    // BUG: the doc comment on [`rerank_relevance`] promises a coverage gate
    // with "one-term slack" — keep rows `>= max_cov - 1` once `max_cov >= 2`,
    // specifically so a row missing exactly one query term is not evicted.
    // The actual gate in `rerank_core` filters on `c == max_cov` (hard
    // equality), so a row one term short of the maximum IS evicted. This
    // locks in the current (stricter-than-documented) behavior.
    #[test]
    fn relevance_gate_hard_equality_contradicts_the_documented_one_term_slack() {
        let full = row("https://full.com/1", "a b c", "a b c", vec![1]); // coverage 3/3
        let one_short = row("https://short.com/1", "a b", "a b", vec![2]); // coverage 2/3
        let rows = [full, one_short];
        let out = rerank_relevance(&rows, "a b c");
        let doms: HashSet<String> = out.iter().map(|r| registrable(url_of(r))).collect();
        assert!(doms.contains("full.com"));
        // Per the doc comment, a row at max_cov - 1 (with max_cov >= 2)
        // should survive; the code evicts it instead.
        assert!(!doms.contains("short.com"));
    }
}
