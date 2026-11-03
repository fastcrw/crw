use crw_core::types::ScrapedImage;
use once_cell::sync::Lazy;
use regex::Regex;
use scraper::{Html, Selector};
use std::collections::HashMap;

/// When a priority selector is "too broad" (>90% of body), drill down into it
/// to find a narrower content element.
fn find_content_within(parent_el: &scraper::ElementRef, parent_len: usize) -> Option<String> {
    let inner_selectors = [
        ".main-page-content",
        ".article-content",
        ".post-content",
        ".entry-content",
        ".content-body",
        ".article-body",
        "[itemprop=\"articleBody\"]",
        "[itemprop=\"text\"]",
        ".mw-parser-output",
        "#mw-content-text",
        "#content",
        ".content",
        "article", // nested article inside broad main
    ];

    let mut best: Option<(String, f64)> = None;
    for sel_str in &inner_selectors {
        if let Ok(sel) = Selector::parse(sel_str) {
            for el in parent_el.select(&sel) {
                let content = el.html();
                if content.len() < 200 {
                    continue;
                }
                // Skip if still too broad relative to parent
                if content.len() as f64 / parent_len as f64 > 0.85 {
                    continue;
                }
                let score = text_density(&content) * (content.len() as f64).ln();
                if best.as_ref().is_none_or(|(_, s)| score > *s) {
                    best = Some((content, score));
                }
            }
        }
    }
    best.map(|(c, _)| c)
}

/// Extract the "main content" element from HTML.
///
/// Uses text-density scoring across candidate selectors to pick the richest element.
/// Falls back to the `<body>` if no scored candidate is found.
pub fn extract_main_content(html: &str) -> String {
    let document = Html::parse_document(html);

    // Priority candidates in order: well-known semantic selectors first.
    let priority_selectors = ["article", "main", "[role=\"main\"]"];

    // Compute body length once for ratio checks below.
    let body_len = Selector::parse("body")
        .ok()
        .and_then(|sel| document.select(&sel).next())
        .map(|b| b.html().len())
        .unwrap_or(html.len());

    // Collect all candidates from priority selectors and score them.
    // Iterate in priority order so ties favor earlier selectors (article > main > role=main).
    let mut candidates: Vec<(scraper::ElementRef, String, f64, usize)> = Vec::new();
    for sel_str in &priority_selectors {
        if let Ok(sel) = Selector::parse(sel_str) {
            for el in document.select(&sel) {
                let content = el.html();
                if content.len() <= 200 {
                    continue;
                }
                let density = text_density(&content);
                if density <= 0.1 {
                    continue;
                }
                let text_len: usize = el.text().map(|t| t.len()).sum();
                if text_len == 0 {
                    continue;
                }
                let text_len_f = text_len as f64;

                let heading_count = ["h1", "h2", "h3", "h4", "h5", "h6"]
                    .iter()
                    .filter_map(|s| Selector::parse(s).ok())
                    .map(|s| el.select(&s).count())
                    .sum::<usize>();
                let paragraph_count = Selector::parse("p")
                    .ok()
                    .map(|s| el.select(&s).count())
                    .unwrap_or(0);
                let link_text_len: usize = Selector::parse("a")
                    .ok()
                    .map(|s| {
                        el.select(&s)
                            .map(|a| a.text().map(|t| t.len()).sum::<usize>())
                            .sum()
                    })
                    .unwrap_or(0);
                let link_density = link_text_len as f64 / text_len_f;

                let mut score = text_len_f * density
                    + (heading_count as f64) * 50.0
                    + (paragraph_count as f64) * 10.0
                    - link_density * text_len_f;

                // Penalty for filter/nav/sidebar markers in class or id.
                let attrs = format!(
                    "{} {}",
                    el.value().attr("class").unwrap_or(""),
                    el.value().attr("id").unwrap_or("")
                )
                .to_lowercase();
                const PENALTY_TOKENS: &[&str] =
                    &["filter", "facet", "sidebar", "nav", "menu", "navigation"];
                if PENALTY_TOKENS.iter().any(|t| attrs.contains(t)) {
                    score -= text_len_f * 0.7;
                }

                candidates.push((el, content, score, text_len));
            }
        }
    }

    if !candidates.is_empty() {
        // Find best by score; on tie, earlier (priority order) wins.
        let mut best_idx = 0;
        for i in 1..candidates.len() {
            if candidates[i].2 > candidates[best_idx].2 {
                best_idx = i;
            }
        }
        // Fallback guard: if best is much smaller than second-best by text length,
        // distrust and fall through to scored-selector path.
        let best_text_len = candidates[best_idx].3;
        let second_best_text_len = candidates
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != best_idx)
            .map(|(_, c)| c.3)
            .max()
            .unwrap_or(0);
        let trust_best = (second_best_text_len as f64) * 0.5 <= best_text_len as f64;

        if trust_best {
            let (el, content, _, _) = &candidates[best_idx];
            // If chosen element wraps nearly the entire document, drill down.
            if body_len > 0 && content.len() as f64 / body_len as f64 > 0.9 {
                if let Some(narrowed) = find_content_within(el, content.len()) {
                    return narrowed;
                }
                // Too broad and no narrower child — fall through to scoring.
            } else {
                return content.clone();
            }
        }
    }

    // Score all candidate selectors by text density and pick the best.
    let scored_selectors = [
        ".post-content",
        ".article-body",
        ".entry-content",
        ".article-content",
        ".post-body",
        ".story-body",
        ".content-body",
        "#main-content",
        "#article",
        "#content",
        ".content",
        ".main",
        "[itemprop=\"articleBody\"]",
        "[itemprop=\"text\"]",
        // MDN
        ".main-page-content",
        // StackOverflow
        ".js-post-body",
        ".s-prose",
        "#question",
        // Generic
        ".page-content",
        "#page-content",
        "[role=\"article\"]",
        // Wikipedia / MediaWiki
        ".mw-parser-output",
        "#mw-content-text",
        "#bodyContent",
        ".mw-body-content",
    ];

    let mut best: Option<(String, f64)> = None;
    for sel_str in &scored_selectors {
        if let Ok(sel) = Selector::parse(sel_str)
            && let Some(el) = document.select(&sel).next()
        {
            let content = el.html();
            if content.len() < 100 {
                continue;
            }
            // Skip selectors that wrap nearly the entire body (same as priority check).
            if body_len > 0 && content.len() as f64 / body_len as f64 > 0.9 {
                if let Some(narrowed) = find_content_within(&el, content.len()) {
                    return narrowed;
                }
                continue;
            }
            let score = text_density(&content) * (content.len() as f64).ln();
            if best.as_ref().is_none_or(|(_, s)| score > *s) {
                best = Some((content, score));
            }
        }
    }

    if let Some((content, _)) = best {
        return content;
    }

    // Last resort: return full body.
    if let Ok(sel) = Selector::parse("body")
        && let Some(body) = document.select(&sel).next()
    {
        return body.inner_html();
    }

    html.to_string()
}

/// Provenance of a successful main-content extraction.
#[derive(Debug, Clone)]
pub struct Provenance {
    pub kind: ProvenanceKind,
    pub candidate_features: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvenanceKind {
    /// Standard text-density / priority-selector pick.
    Primary,
    /// Picked element was a listing root; we detached repeating subtrees
    /// or descended into a non-listing child.
    ListingFallback,
    /// Listing detected but no usable body recovered.
    ListingRootRejected,
    /// Element lives inside a reference / bibliography section.
    ReferenceProtected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    /// Picked element was a listing and detach/descent both failed.
    ListingRootEmpty,
    /// No candidate cleared the minimum-body threshold.
    NoBodyAboveMinChars,
}

#[derive(Debug, Clone)]
pub enum ReadabilityOutcome {
    Selected {
        html: String,
        provenance: Provenance,
    },
    Rejected {
        reason: RejectReason,
    },
}

const LISTING_FALLBACK_MIN_CHARS: usize = 400;
const MAX_DESCENT_DEPTH: u8 = 3;

/// Provenance-aware variant of [`extract_main_content`].
///
/// First runs the legacy density-scored picker. If the chosen container
/// looks like a listing (cards, link grid), tries:
///
/// 1. detach repeating-shape subtrees in place;
/// 2. descend up to `MAX_DESCENT_DEPTH` looking for the largest
///    non-listing child with `>= LISTING_FALLBACK_MIN_CHARS` of text.
///
/// Returns `Rejected` only when both fallbacks fail to surface a body —
/// callers can then jump to the cleaned-HTML candidate.
pub fn extract_main_content_with_provenance(html: &str) -> ReadabilityOutcome {
    let primary = extract_main_content(html);
    if primary.trim().is_empty() {
        return ReadabilityOutcome::Rejected {
            reason: RejectReason::NoBodyAboveMinChars,
        };
    }

    let frag = Html::parse_fragment(&primary);
    let root_text_len = crate::dom_util::text_char_len(frag.root_element());
    // Readability often returns a wrapper (body/main/article) that has the
    // listing nested one or more levels deep. Walk the entire fragment tree
    // and trigger on the first descendant that matches the listing gate
    // — but only if it covers a meaningful share of the picked content
    // (≥50% of root text). Otherwise we'd treat sidebars / "more from"
    // rails as the page's primary intent.
    let listing_target = {
        let root = frag.root_element();
        find_listing_descendant(root).filter(|el| {
            if root_text_len == 0 {
                return false;
            }
            let target_text_len = crate::dom_util::text_char_len(*el);
            (target_text_len as f64) / (root_text_len as f64) >= 0.5
        })
    };
    if let Some(el) = listing_target {
        // Case B (listing root): try to descend into a non-listing child
        // with enough prose to stand on its own; otherwise reject and let
        // the caller fall through to the cleaned-HTML alternate, which
        // preserves card titles for downstream markdown conversion.
        if let Some(narrower) = walk_to_non_listing_descendant(el, MAX_DESCENT_DEPTH) {
            return ReadabilityOutcome::Selected {
                html: narrower,
                provenance: Provenance {
                    kind: ProvenanceKind::ListingFallback,
                    candidate_features: None,
                },
            };
        }
        return ReadabilityOutcome::Rejected {
            reason: RejectReason::ListingRootEmpty,
        };
    }

    ReadabilityOutcome::Selected {
        html: primary,
        provenance: Provenance {
            kind: ProvenanceKind::Primary,
            candidate_features: None,
        },
    }
}

fn find_listing_descendant<'a>(el: scraper::ElementRef<'a>) -> Option<scraper::ElementRef<'a>> {
    use crate::dom_util::{ElementChildren, has_paragraph_island, is_listing_container};
    // If any ancestor along the path has a paragraph island, the listing
    // is incidental (article with embedded card row) — leave it alone.
    if has_paragraph_island(el, LISTING_FALLBACK_MIN_CHARS) {
        return None;
    }
    if is_listing_container(el) {
        return Some(el);
    }
    for child in el.element_children() {
        if let Some(found) = find_listing_descendant(child) {
            return Some(found);
        }
    }
    None
}

fn walk_to_non_listing_descendant(el: scraper::ElementRef<'_>, max_depth: u8) -> Option<String> {
    use crate::dom_util::{ElementChildren, is_listing_container, text_char_len};
    if max_depth == 0 {
        return None;
    }
    let mut best: Option<(String, usize)> = None;
    for child in el.element_children() {
        if is_listing_container(child) {
            continue;
        }
        let chars = text_char_len(child);
        if chars < LISTING_FALLBACK_MIN_CHARS {
            continue;
        }
        let html = child.html();
        if best.as_ref().is_none_or(|(_, c)| chars > *c) {
            best = Some((html, chars));
        }
    }
    if let Some((h, _)) = best {
        return Some(h);
    }
    for child in el.element_children() {
        if let Some(v) = walk_to_non_listing_descendant(child, max_depth - 1) {
            return Some(v);
        }
    }
    None
}

/// Compute text-to-html ratio as a simple content density signal.
/// Returns a value in [0, 1]: higher = more text relative to markup.
fn text_density(html: &str) -> f64 {
    let doc = Html::parse_fragment(html);
    let text_len: usize = doc.root_element().text().map(|t| t.len()).sum();
    if html.is_empty() {
        return 0.0;
    }
    text_len as f64 / html.len() as f64
}

/// All extracted metadata from a page.
pub struct ExtractedMetadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub language: Option<String>,
    pub og_title: Option<String>,
    pub og_description: Option<String>,
    pub og_image: Option<String>,
    pub canonical_url: Option<String>,
    /// Every other `<meta name|property>` tag on the page, keyed by its raw
    /// name/property (e.g. `twitter:creator`, `author`). Values are the `content`
    /// attribute; a tag that repeats becomes an array. Keys already surfaced as
    /// a named field above (`title`, `description`) are excluded to avoid a
    /// duplicate key once flattened onto the metadata object.
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Extract metadata (title, description, OG tags, canonical) from HTML.
pub fn extract_metadata(html: &str) -> ExtractedMetadata {
    let document = Html::parse_document(html);

    let title = select_text(&document, "title");

    let description = select_attr(&document, r#"meta[name="description"]"#, "content");

    let og_title = select_attr(&document, r#"meta[property="og:title"]"#, "content");
    let og_description = select_attr(&document, r#"meta[property="og:description"]"#, "content");
    let og_image = select_attr(&document, r#"meta[property="og:image"]"#, "content");

    let canonical_url = select_attr(&document, r#"link[rel="canonical"]"#, "href");

    // Extract language from <html lang="..."> attribute.
    let language = select_attr(&document, "html", "lang");

    let extra = collect_meta_tags(&document);

    ExtractedMetadata {
        title,
        description,
        language,
        og_title,
        og_description,
        og_image,
        canonical_url,
        extra,
    }
}

/// Collect every `<meta name|property>` tag into a map, mirroring Firecrawl's
/// flat metadata. `name` wins over `property` when both are present. A tag that
/// appears more than once (e.g. `viewport`) becomes a JSON array; a single tag
/// stays a string. `title` / `description` are skipped — they already ship as
/// named fields and would collide once flattened onto the metadata object.
fn collect_meta_tags(document: &Html) -> std::collections::BTreeMap<String, serde_json::Value> {
    use serde_json::Value;
    use std::collections::BTreeMap;

    const SKIP: [&str; 2] = ["title", "description"];
    let mut raw: BTreeMap<String, Vec<String>> = BTreeMap::new();

    if let Ok(sel) = Selector::parse("meta") {
        for el in document.select(&sel) {
            let attrs = el.value();
            let Some(key) = attrs.attr("name").or_else(|| attrs.attr("property")) else {
                continue;
            };
            let key = key.trim();
            let Some(content) = attrs.attr("content") else {
                continue;
            };
            let content = content.trim();
            if key.is_empty() || content.is_empty() || SKIP.contains(&key) {
                continue;
            }
            raw.entry(key.to_string())
                .or_default()
                .push(content.to_string());
        }
    }

    raw.into_iter()
        .map(|(k, mut vals)| {
            let v = if vals.len() == 1 {
                Value::String(vals.pop().unwrap())
            } else {
                Value::Array(vals.into_iter().map(Value::String).collect())
            };
            (k, v)
        })
        .collect()
}

fn select_text(doc: &Html, selector: &str) -> Option<String> {
    Selector::parse(selector)
        .ok()
        .and_then(|sel| doc.select(&sel).next())
        .map(|el| el.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
}

fn select_attr(doc: &Html, selector: &str, attr: &str) -> Option<String> {
    Selector::parse(selector)
        .ok()
        .and_then(|sel| doc.select(&sel).next())
        // Trimmed like a browser does for URL attributes: a template that wraps
        // `href='\n  https://...\n  '` must not surface the padding (issue #664).
        .and_then(|el| el.value().attr(attr).map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
}

/// Extract all links from HTML.
pub fn extract_links(html: &str, base_url: &str) -> Vec<String> {
    let document = Html::parse_document(html);
    let sel = match Selector::parse("a[href]") {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let base = url::Url::parse(base_url).ok();

    document
        .select(&sel)
        .filter_map(|el| {
            let href = el.value().attr("href")?;
            if href.starts_with('#')
                || href.starts_with("javascript:")
                || href.starts_with("mailto:")
                || href.starts_with("data:")
                || href.starts_with("tel:")
                || href.starts_with("blob:")
            {
                return None;
            }
            if let Some(base) = &base {
                base.join(href).ok().map(|u| u.to_string())
            } else if href.starts_with("http") {
                Some(href.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// `background-image: url(...)` extractor. Mirrors Firecrawl's `URL_REGEX`
/// (`apps/api/native/src/html.rs`) verbatim, including its `[^'")]+` stop — a
/// `)` inside a `data:` SVG can truncate the match. Kept for byte-for-byte
/// parity with the v2 drop-in surface.
// ponytail: naive `[^'")]+`; a CSS-value parser is the upgrade path if a real
// page needs it, but that would diverge the v2 URL set from Firecrawl.
static BG_URL_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"url\(['"]?([^'")]+)['"]?\)"#).unwrap());

/// Extract candidate URL tokens from an HTML `srcset` per the WHATWG parse
/// algorithm's URL step: each candidate's URL is a leading run of NON-whitespace
/// characters, so an internal comma in a `data:` URI stays part of the URL
/// rather than splitting it. After the URL, an optional descriptor runs to the
/// next top-level comma (parens tracked for `calc()` widths). For ordinary
/// `a.jpg 480w, b.jpg 1080w` srcsets this yields exactly `["a.jpg", "b.jpg"]`,
/// identical to a naive comma split; it only differs on comma-bearing URLs.
fn srcset_url_tokens(srcset: &str) -> Vec<&str> {
    fn is_ws(c: u8) -> bool {
        matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
    }
    let b = srcset.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut urls = Vec::new();
    while i < n {
        while i < n && (is_ws(b[i]) || b[i] == b',') {
            i += 1;
        }
        if i >= n {
            break;
        }
        let start = i;
        while i < n && !is_ws(b[i]) {
            i += 1;
        }
        let mut url = &srcset[start..i];
        if url.ends_with(',') {
            // Trailing commas mean this candidate had no descriptor.
            url = url.trim_end_matches(',');
        } else {
            // Skip the descriptor up to the next top-level comma.
            let mut depth: i32 = 0;
            while i < n {
                match b[i] {
                    b'(' => depth += 1,
                    b')' => depth = depth.saturating_sub(1),
                    b',' if depth == 0 => break,
                    _ => {}
                }
                i += 1;
            }
        }
        if !url.is_empty() {
            urls.push(url);
        }
    }
    urls
}

/// Normalize an optional `alt`: trim and treat empty as absent.
fn norm_alt(alt: Option<&str>) -> Option<String> {
    alt.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Extract all images discovered on the page.
///
/// The **URL set mirrors Firecrawl's `_extract_images`** exactly (same sources,
/// resolution, filters, and srcset/background parsing — bugs included) so the v2
/// surface, which flattens these to a plain `Vec<String>`, stays a Firecrawl
/// drop-in from this single pass. The only native-`/v1` enrichment is the `alt`
/// field, which does not affect the URL set.
///
/// Deduplicated by URL in document order; a later duplicate carrying a non-empty
/// `alt` upgrades an earlier `None` (lazy-load and `<picture>`+`<img>` commonly
/// put the good `alt` on the second sighting).
pub fn extract_images(html: &str, base_url: &str) -> Vec<ScrapedImage> {
    let document = Html::parse_document(html);

    // Join base: honor `<base href>` when present, else the scrape source URL.
    // A `<base href>` may itself be relative (`/cdn/`) or absolute; resolve it
    // against the document URL (matching Firecrawl's `new URL(baseHref, baseUrl)`
    // fallback) so relative bases aren't silently dropped. A malformed base
    // degrades to `None` (absolute-only) rather than panicking.
    let doc_base = url::Url::parse(base_url).ok();
    // `<base href>` join base: relative or absolute, resolved against the doc URL;
    // `doc_base` itself is kept for protocol-relative (`//`) page-scheme joins.
    let base = select_attr(&document, "base[href]", "href")
        .and_then(|h| doc_base.as_ref().and_then(|b| b.join(&h).ok()))
        .or_else(|| doc_base.clone());

    // Resolve a raw src, mirroring Firecrawl's `resolve_image_url` branch-for-
    // branch so the v2 URL set stays a drop-in:
    //   data:/blob:    -> verbatim
    //   http(s):// abs -> verbatim (Firecrawl does NOT canonicalize absolutes)
    //   //host/x       -> inherit the PAGE scheme (join against the doc URL)
    //   relative       -> join against `<base href>` (falls back to the doc URL)
    // Then Firecrawl's final filter: drop `javascript:` (case-insensitive) and
    // any non-`data:`/`blob:` result that won't `Url::parse`.
    let resolve = |src: &str| -> Option<String> {
        // Deliberate, recall-neutral divergence from Firecrawl on degenerate
        // input: trim whitespace and skip an empty `src`. Firecrawl uses the raw
        // value, so its native resolver turns `src=""` into `base_href.join("")`
        // = the PAGE URL and emits it as an "image" (junk no drop-in client
        // wants), and keeps whitespace-padded URLs verbatim. We never drop a real
        // image here, so v2 recall is unaffected; we only omit that junk.
        let src = src.trim();
        if src.is_empty() {
            return None;
        }
        // Kept verbatim (Firecrawl does not canonicalize these). The
        // `http(s)://` prefix check is case-sensitive, exactly like Firecrawl —
        // an uppercase scheme (`HTTPS://`) intentionally falls through to
        // `join`, matching Firecrawl's `resolve_image_url`.
        let candidate = if src.starts_with("data:")
            || src.starts_with("blob:")
            || src.starts_with("http://")
            || src.starts_with("https://")
        {
            src.to_string()
        } else if src.starts_with("//") {
            // Protocol-relative: inherit the PAGE scheme (join against doc URL).
            doc_base.as_ref()?.join(src).ok()?.to_string()
        } else {
            base.as_ref()?.join(src).ok()?.to_string()
        };
        if candidate.to_ascii_lowercase().starts_with("javascript:") {
            return None;
        }
        if !candidate.starts_with("data:")
            && !candidate.starts_with("blob:")
            && url::Url::parse(&candidate).is_err()
        {
            return None;
        }
        Some(candidate)
    };

    // Dedup by URL in traversal order via a url->index map (O(1) per push, so a
    // page with many repeated URLs stays linear). Traversal order is the fixed
    // source-category order below (img, then picture, meta, icons, poster,
    // background) and DOM order within each — deterministic, matching Firecrawl.
    // A later duplicate carrying a non-empty `alt` upgrades an earlier `None`.
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut images: Vec<ScrapedImage> = Vec::new();
    let mut push = |url: String, alt: Option<String>| match index.get(&url) {
        None => {
            index.insert(url.clone(), images.len());
            images.push(ScrapedImage { url, alt });
        }
        Some(&i) => {
            if let Some(new_alt) = alt
                && images[i].alt.is_none()
            {
                images[i].alt = Some(new_alt);
            }
        }
    };

    // Extract candidate URLs from a `srcset`, resolved. Uses the WHATWG URL
    // step (`srcset_url_tokens`) rather than a naive `split(',')`: identical to
    // the naive result for ordinary `url 480w, url 1080w` srcsets, but a comma
    // INSIDE a `data:` URI no longer splits it into phantom fragments (a real
    // lazy-load placeholder pattern — see the smashingmagazine.com regression
    // test). Recall-neutral: it never drops a real image, only avoids junk.
    let srcset_urls = |srcset: &str| -> Vec<String> {
        srcset_url_tokens(srcset)
            .into_iter()
            .filter_map(&resolve)
            .collect()
    };

    // 1. <img src|data-src|srcset> — carries the img's alt.
    if let Ok(sel) = Selector::parse("img") {
        for el in document.select(&sel) {
            let alt = norm_alt(el.value().attr("alt"));
            if let Some(src) = el.value().attr("src")
                && let Some(url) = resolve(src)
            {
                push(url, alt.clone());
            }
            if let Some(src) = el.value().attr("data-src")
                && let Some(url) = resolve(src)
            {
                push(url, alt.clone());
            }
            if let Some(srcset) = el.value().attr("srcset") {
                for url in srcset_urls(srcset) {
                    push(url, alt.clone());
                }
            }
        }
    }

    // 2. <picture><source srcset> — no alt.
    if let Ok(sel) = Selector::parse("picture source") {
        for el in document.select(&sel) {
            if let Some(srcset) = el.value().attr("srcset") {
                for url in srcset_urls(srcset) {
                    push(url, None);
                }
            }
        }
    }

    // 3. OG / Twitter / itemprop meta images (read `content`) — no alt.
    if let Ok(sel) = Selector::parse(
        r#"meta[property="og:image"], meta[property="og:image:url"], meta[property="og:image:secure_url"], meta[name="twitter:image"], meta[name="twitter:image:src"], meta[itemprop="image"]"#,
    ) {
        for el in document.select(&sel) {
            if let Some(content) = el.value().attr("content")
                && let Some(url) = resolve(content)
            {
                push(url, None);
            }
        }
    }

    // 4. Icon / image_src links (read `href`, substring `*=` like Firecrawl) — no alt.
    if let Ok(sel) = Selector::parse(
        r#"link[rel*="icon"], link[rel*="apple-touch-icon"], link[rel*="image_src"]"#,
    ) {
        for el in document.select(&sel) {
            if let Some(href) = el.value().attr("href")
                && let Some(url) = resolve(href)
            {
                push(url, None);
            }
        }
    }

    // 5. <video poster> — no alt.
    if let Ok(sel) = Selector::parse("video[poster]") {
        for el in document.select(&sel) {
            if let Some(poster) = el.value().attr("poster")
                && let Some(url) = resolve(poster)
            {
                push(url, None);
            }
        }
    }

    // 6. Inline background-image styles — no alt.
    if let Ok(sel) = Selector::parse(r#"[style*="background"]"#) {
        for el in document.select(&sel) {
            if let Some(style) = el.value().attr("style") {
                for cap in BG_URL_REGEX.captures_iter(style) {
                    if let Some(m) = cap.get(1)
                        && let Some(url) = resolve(m.as_str())
                    {
                        push(url, None);
                    }
                }
            }
        }
    }

    images
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_article_content() {
        let html = r#"<html><body><nav>Nav</nav><article><p>Main content</p></article><footer>Foot</footer></body></html>"#;
        let content = extract_main_content(html);
        assert!(content.contains("Main content"));
    }

    #[test]
    fn srcset_url_tokens_ordinary() {
        assert_eq!(
            srcset_url_tokens("a.jpg 480w, b.jpg 1080w, c.jpg 2x"),
            vec!["a.jpg", "b.jpg", "c.jpg"]
        );
    }

    #[test]
    fn srcset_url_tokens_no_descriptors() {
        // Trailing-comma candidates (no descriptor) still split correctly.
        assert_eq!(srcset_url_tokens("a.jpg, b.jpg"), vec!["a.jpg", "b.jpg"]);
    }

    #[test]
    fn srcset_url_tokens_data_uri_comma_kept() {
        // The comma inside the data: URI does NOT split the token.
        assert_eq!(
            srcset_url_tokens("data:image/avif;base64,AAAA== 1x, /real.jpg 2x"),
            vec!["data:image/avif;base64,AAAA==", "/real.jpg"]
        );
    }

    #[test]
    fn srcset_url_tokens_paren_descriptor() {
        // A descriptor containing a comma inside parens isn't a candidate split.
        assert_eq!(
            srcset_url_tokens("a.jpg 100w, b.jpg calc(50vw - 10px)"),
            vec!["a.jpg", "b.jpg"]
        );
    }

    #[test]
    fn srcset_url_tokens_empty() {
        assert!(srcset_url_tokens("").is_empty());
        assert!(srcset_url_tokens("   ").is_empty());
    }

    #[test]
    fn extracts_title_and_description() {
        let html = r#"<html><head><title>Test Page</title><meta name="description" content="A test"></head><body></body></html>"#;
        let meta = extract_metadata(html);
        assert_eq!(meta.title.unwrap(), "Test Page");
        assert_eq!(meta.description.unwrap(), "A test");
    }

    #[test]
    fn collects_arbitrary_meta_tags() {
        use serde_json::Value;
        let html = r#"<html><head>
            <title>T</title>
            <meta name="description" content="D">
            <meta name="twitter:creator" content="@behramcelen">
            <meta property="og:type" content="blog">
            <meta name="viewport" content="a">
            <meta name="viewport" content="b">
            <meta name="empty" content="">
        </head><body></body></html>"#;
        let meta = extract_metadata(html);
        // Arbitrary name/property tags surface verbatim.
        assert_eq!(
            meta.extra.get("twitter:creator"),
            Some(&Value::String("@behramcelen".into()))
        );
        assert_eq!(
            meta.extra.get("og:type"),
            Some(&Value::String("blog".into()))
        );
        // Repeated tag becomes an array.
        assert_eq!(
            meta.extra.get("viewport"),
            Some(&Value::Array(vec![
                Value::String("a".into()),
                Value::String("b".into())
            ]))
        );
        // title/description are named fields — excluded to avoid flatten collision.
        assert!(!meta.extra.contains_key("description"));
        // Empty content is dropped.
        assert!(!meta.extra.contains_key("empty"));
    }

    #[test]
    fn extracts_og_metadata() {
        let html = r#"<html><head>
            <meta property="og:title" content="OG Title">
            <meta property="og:description" content="OG Desc">
            <meta property="og:image" content="https://img.com/pic.jpg">
            <link rel="canonical" href="https://example.com/canonical">
        </head><body></body></html>"#;
        let meta = extract_metadata(html);
        assert_eq!(meta.og_title.unwrap(), "OG Title");
        assert_eq!(meta.og_description.unwrap(), "OG Desc");
        assert_eq!(meta.og_image.unwrap(), "https://img.com/pic.jpg");
        assert_eq!(meta.canonical_url.unwrap(), "https://example.com/canonical");
    }

    #[test]
    fn skips_broad_article_picks_mw_parser_output() {
        // Simulate Wikipedia structure: <article> wraps everything,
        // but .mw-parser-output is the real content.
        let filler = "x".repeat(500);
        let html = format!(
            r#"<html><body>
            <article>
              <div id="mw-navigation">{filler}</div>
              <div id="content" role="main">
                <div id="bodyContent">
                  <div id="mw-content-text">
                    <div class="mw-parser-output">
                      <p>This is the real Wikipedia article content about web scraping. {filler}</p>
                    </div>
                  </div>
                </div>
              </div>
              <div class="catlinks">{filler}</div>
            </article>
            </body></html>"#
        );
        let content = extract_main_content(&html);
        assert!(
            content.contains("real Wikipedia article content"),
            "Should extract .mw-parser-output content"
        );
        // Should NOT contain the navigation or catlinks filler
        assert!(
            !content.contains("mw-navigation"),
            "Should not include navigation div"
        );
    }

    #[test]
    fn extracts_links() {
        let html = r##"<html><body><a href="/page1">P1</a><a href="https://other.com">O</a><a href="#top">T</a></body></html>"##;
        let links = extract_links(html, "https://example.com");
        assert_eq!(links.len(), 2);
        assert!(links.contains(&"https://example.com/page1".to_string()));
        assert!(
            links.contains(&"https://other.com".to_string())
                || links.contains(&"https://other.com/".to_string())
        );
    }

    // ── extract_main_content: page shapes ──────────────────────────────

    #[test]
    fn blog_shape_prefers_post_content_class_when_no_semantic_tags() {
        let html = r#"<html><body>
            <nav>Home About Contact Blog Archive Categories Tags Search Login</nav>
            <div class="post-content"><p>This is the real blog post body, long enough to score well as the winning candidate in the density scan.</p></div>
        </body></html>"#;
        let content = extract_main_content(html);
        assert!(content.contains("real blog post body"));
    }

    #[test]
    fn docs_shape_prefers_content_id_over_unlisted_sidebar_class() {
        let html = r#"<html><body>
            <div class="sidebar-nav">SIDENAV_MARKER install quickstart api reference changelog faq</div>
            <div id="content"><p>Documentation body explaining how to install and configure the CLI tool in detail.</p></div>
        </body></html>"#;
        let content = extract_main_content(html);
        assert!(content.contains("Documentation body"));
        assert!(!content.contains("SIDENAV_MARKER"));
    }

    #[test]
    fn forum_shape_extracts_stackoverflow_post_body() {
        let html = r#"<html><body>
            <div class="js-post-body"><p>Question body: why does my Rust borrow checker complain here in this specific case?</p></div>
            <div class="related-questions">REL_Q_MARKER similar question one similar question two similar question three</div>
        </body></html>"#;
        let content = extract_main_content(html);
        assert!(content.contains("borrow checker"));
        assert!(!content.contains("REL_Q_MARKER"));
    }

    #[test]
    fn product_page_shape_extracts_content_body_class() {
        let html = r#"<html><body>
            <div class="related-products">REL_PROD_MARKER item one item two item three item four</div>
            <div class="content-body"><p>Full product description: durable stainless steel water bottle with vacuum insulation.</p></div>
        </body></html>"#;
        let content = extract_main_content(html);
        assert!(content.contains("stainless steel water bottle"));
        assert!(!content.contains("REL_PROD_MARKER"));
    }

    #[test]
    fn nested_article_keeps_outer_article_content() {
        let html = r#"<html><body><article>
            <p>OUTER_START: this is the primary article body with plenty of real prose to dominate the score.</p>
            <article><p>Related teaser, much shorter.</p></article>
            <p>OUTER_END: closing paragraph of the primary article body, also long enough to add real weight.</p>
        </article></body></html>"#;
        let content = extract_main_content(html);
        assert!(content.contains("OUTER_START"));
        assert!(content.contains("OUTER_END"));
    }

    #[test]
    fn find_content_within_narrows_main_that_wraps_whole_body() {
        let filler = "filler word ".repeat(80);
        let html = format!(
            r#"<html><body><main>
                <div class="site-header-filler">{filler}</div>
                <div class="content-body"><p>Real docs content here about installing the CLI tool from source. {filler}</p></div>
                <div class="site-footer-filler">{filler}</div>
            </main></body></html>"#
        );
        let content = extract_main_content(&html);
        assert!(content.contains("installing the CLI tool"));
        assert!(!content.contains("site-header-filler"));
        assert!(!content.contains("site-footer-filler"));
    }

    #[test]
    fn sidebar_penalty_token_loses_to_smaller_unpenalized_article() {
        let article_text = "Real article prose. ".repeat(40);
        let sidebar_text = "Sidebar filler text. ".repeat(45);
        let html = format!(
            r#"<html><body>
                <article><p>ARTICLE_MARKER {article_text}</p></article>
                <main class="sidebar-nav-widget"><p>SIDEBAR_MARKER {sidebar_text}</p></main>
            </body></html>"#
        );
        let content = extract_main_content(&html);
        assert!(
            content.contains("ARTICLE_MARKER"),
            "penalized sidebar-classed main must not beat the plain article: {content}"
        );
        assert!(!content.contains("SIDEBAR_MARKER"));
    }

    #[test]
    fn single_div_content_no_semantic_wrapper() {
        let html = r#"<html><body>
            <div class="content"><p>All the page's real content lives in this one plain div, with no article or main wrapper at all.</p></div>
        </body></html>"#;
        let content = extract_main_content(html);
        assert!(content.contains("one plain div"));
    }

    #[test]
    fn falls_back_to_body_when_no_candidate_matches() {
        let html = r#"<html><body><section class="unlisted-wrapper"><p>Just a plain unmarked section with some content.</p></section></body></html>"#;
        let content = extract_main_content(html);
        assert!(content.contains("plain unmarked section"));
    }

    #[test]
    fn empty_html_does_not_panic_and_returns_empty() {
        let content = extract_main_content("");
        assert!(content.trim().is_empty());
    }

    #[test]
    fn truncated_unclosed_html_does_not_panic() {
        let html = r#"<html><body><article><p>Unterminated paragraph and tag <div class="broken"#;
        let content = extract_main_content(html);
        // Just must not panic; html5ever recovers something (possibly empty).
        let _ = content;
    }

    #[test]
    fn deeply_nested_markup_does_not_panic() {
        let mut html = String::from("<html><body><article>");
        for _ in 0..500 {
            html.push_str("<div>");
        }
        html.push_str("<p>Deeply nested real content.</p>");
        for _ in 0..500 {
            html.push_str("</div>");
        }
        html.push_str("</article></body></html>");
        let content = extract_main_content(&html);
        assert!(content.contains("Deeply nested real content"));
    }

    #[test]
    fn rtl_arabic_text_preserved() {
        let html = r#"<html><body><article><p>مرحبا بالعالم، هذه مقالة تجريبية طويلة تحتوي على نص عربي من اليمين إلى اليسار للتحقق من عدم كسر المستخرج أثناء المعالجة.</p></article></body></html>"#;
        let content = extract_main_content(html);
        assert!(content.contains("مرحبا بالعالم"));
    }

    #[test]
    fn emoji_and_unicode_do_not_panic() {
        let html = r#"<html><body><article><p>Launch day 🚀🔥✨ celebration post with plenty of real prose surrounding the emoji so the density scan has something to chew on.</p></article></body></html>"#;
        let content = extract_main_content(html);
        assert!(content.contains("🚀🔥✨"));
    }

    // ── extract_main_content_with_provenance ───────────────────────────

    #[test]
    fn provenance_selected_primary_for_normal_article() {
        let html = r#"<html><body><article><p>Real content here, more than enough characters to clear the minimum-body threshold easily.</p></article></body></html>"#;
        match extract_main_content_with_provenance(html) {
            ReadabilityOutcome::Selected { html, provenance } => {
                assert!(html.contains("Real content here"));
                assert_eq!(provenance.kind, ProvenanceKind::Primary);
            }
            ReadabilityOutcome::Rejected { reason } => {
                panic!("expected Selected, got Rejected({reason:?})")
            }
        }
    }

    #[test]
    fn provenance_rejected_for_empty_body() {
        let html = "<html><body></body></html>";
        match extract_main_content_with_provenance(html) {
            ReadabilityOutcome::Rejected { reason } => {
                assert_eq!(reason, RejectReason::NoBodyAboveMinChars);
            }
            ReadabilityOutcome::Selected { .. } => panic!("expected Rejected for empty body"),
        }
    }

    #[test]
    fn provenance_rejected_for_whitespace_only_body() {
        let html = "<html><body>   \n\t   </body></html>";
        match extract_main_content_with_provenance(html) {
            ReadabilityOutcome::Rejected { reason } => {
                assert_eq!(reason, RejectReason::NoBodyAboveMinChars);
            }
            ReadabilityOutcome::Selected { .. } => {
                panic!("expected Rejected for whitespace-only body")
            }
        }
    }

    // ── extract_metadata ────────────────────────────────────────────────

    #[test]
    fn metadata_missing_head_returns_all_none() {
        let html = "<html><body><p>No head at all.</p></body></html>";
        let meta = extract_metadata(html);
        assert!(meta.title.is_none());
        assert!(meta.description.is_none());
        assert!(meta.og_title.is_none());
        assert!(meta.canonical_url.is_none());
    }

    #[test]
    fn metadata_malformed_html_does_not_panic() {
        let html = r#"<html><head><title>Unterminated<meta name="description" content="broken"#;
        let meta = extract_metadata(html);
        // html5ever recovers a document; just must not panic.
        let _ = meta.title;
    }

    #[test]
    fn metadata_lang_attribute_extracted() {
        let html = r#"<html lang="tr"><head><title>T</title></head><body></body></html>"#;
        let meta = extract_metadata(html);
        assert_eq!(meta.language.as_deref(), Some("tr"));
    }

    #[test]
    fn metadata_name_wins_over_property_on_same_tag() {
        use serde_json::Value;
        let html = r#"<html><head><meta name="foo" property="bar" content="X"></head><body></body></html>"#;
        let meta = extract_metadata(html);
        assert_eq!(meta.extra.get("foo"), Some(&Value::String("X".into())));
        assert!(!meta.extra.contains_key("bar"));
    }

    #[test]
    fn metadata_unicode_title_and_description() {
        let html = r#"<html><head><title>日本語のタイトル</title><meta name="description" content="Ürünlerimiz hakkında bilgi"></head><body></body></html>"#;
        let meta = extract_metadata(html);
        assert_eq!(meta.title.as_deref(), Some("日本語のタイトル"));
        assert_eq!(
            meta.description.as_deref(),
            Some("Ürünlerimiz hakkında bilgi")
        );
    }

    #[test]
    fn metadata_html_entities_in_description_are_predecoded_by_parser() {
        let html = r#"<html><head><meta name="description" content="Cats &amp; Dogs"></head><body></body></html>"#;
        let meta = extract_metadata(html);
        assert_eq!(meta.description.as_deref(), Some("Cats & Dogs"));
    }

    #[test]
    fn metadata_relative_canonical_url_kept_raw() {
        let html =
            r#"<html><head><link rel="canonical" href="/page/42"></head><body></body></html>"#;
        let meta = extract_metadata(html);
        assert_eq!(meta.canonical_url.as_deref(), Some("/page/42"));
    }

    /// Issue #664: a templated canonical href wrapped in newlines/indent.
    #[test]
    fn metadata_canonical_url_whitespace_is_trimmed() {
        let html = "<html><head><link rel='canonical' href='\n          https://example.com/c/tvs\n        '>\
                    <meta property=\"og:title\" content=\"  Padded  \"></head><body></body></html>";
        let meta = extract_metadata(html);
        assert_eq!(
            meta.canonical_url.as_deref(),
            Some("https://example.com/c/tvs")
        );
        assert_eq!(meta.og_title.as_deref(), Some("Padded"));
    }

    #[test]
    fn metadata_collect_trims_whitespace_and_skips_empty_key() {
        let html = r#"<html><head>
            <meta name="  spaced  " content="  padded value  ">
            <meta name="" content="no key">
        </head><body></body></html>"#;
        let meta = extract_metadata(html);
        assert!(meta.extra.contains_key("spaced"));
        assert_eq!(meta.extra.len(), 1);
    }

    // ── extract_links ────────────────────────────────────────────────────

    #[test]
    fn links_filters_non_navigable_schemes_and_fragments() {
        let html = r##"<html><body>
            <a href="javascript:void(0)">JS</a>
            <a href="mailto:a@b.com">Mail</a>
            <a href="data:text/plain,hi">Data</a>
            <a href="tel:+15551234">Tel</a>
            <a href="blob:https://example.com/uuid">Blob</a>
            <a href="#section">Fragment</a>
            <a href="/real">Real</a>
        </body></html>"##;
        let links = extract_links(html, "https://example.com");
        assert_eq!(links, vec!["https://example.com/real".to_string()]);
    }

    #[test]
    fn links_resolves_relative_paths_against_base() {
        let html = r#"<html><body><a href="../up/one">Up</a></body></html>"#;
        let links = extract_links(html, "https://example.com/a/b/");
        assert_eq!(links, vec!["https://example.com/a/up/one".to_string()]);
    }

    #[test]
    fn links_duplicate_hrefs_are_not_deduped() {
        let html = r#"<html><body><a href="/x">A</a><a href="/x">B</a></body></html>"#;
        let links = extract_links(html, "https://example.com");
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn links_malformed_base_url_falls_back_to_absolute_only() {
        let html =
            r#"<html><body><a href="http://ok.com/x">A</a><a href="/relative">B</a></body></html>"#;
        let links = extract_links(html, "not a valid url");
        assert_eq!(links, vec!["http://ok.com/x".to_string()]);
    }

    #[test]
    fn links_malformed_html_still_extracts() {
        let html = r#"<html><body><a href="/one">One<a href="/two">Two"#;
        let links = extract_links(html, "https://example.com");
        assert!(links.contains(&"https://example.com/one".to_string()));
        assert!(links.contains(&"https://example.com/two".to_string()));
    }

    // ── extract_images ───────────────────────────────────────────────────

    #[test]
    fn images_src_and_alt_extracted() {
        let html = r#"<html><body><img src="/a.jpg" alt="A photo"></body></html>"#;
        let images = extract_images(html, "https://example.com");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].url, "https://example.com/a.jpg");
        assert_eq!(images[0].alt.as_deref(), Some("A photo"));
    }

    #[test]
    fn images_data_src_and_src_dedup_with_alt_upgrade() {
        let html = r#"<html><body><img src="/a.jpg" data-src="/a.jpg"><img src="/a.jpg" alt="Late alt"></body></html>"#;
        let images = extract_images(html, "https://example.com");
        assert_eq!(images.len(), 1, "same URL from src/data-src must dedup");
        assert_eq!(images[0].alt.as_deref(), Some("Late alt"));
    }

    #[test]
    fn images_srcset_multiple_candidates_extracted() {
        let html = r#"<html><body><img srcset="/small.jpg 480w, /big.jpg 1080w" alt="Responsive"></body></html>"#;
        let images = extract_images(html, "https://example.com");
        let urls: Vec<&str> = images.iter().map(|i| i.url.as_str()).collect();
        assert!(urls.contains(&"https://example.com/small.jpg"));
        assert!(urls.contains(&"https://example.com/big.jpg"));
    }

    #[test]
    fn images_picture_source_has_no_alt() {
        let html = r#"<html><body><picture><source srcset="/p.jpg 1x"><img src="/fallback.jpg" alt="Fallback"></picture></body></html>"#;
        let images = extract_images(html, "https://example.com");
        let source_img = images
            .iter()
            .find(|i| i.url.ends_with("p.jpg"))
            .expect("picture source image missing");
        assert!(source_img.alt.is_none());
    }

    #[test]
    fn images_og_twitter_itemprop_meta_extracted() {
        let html = r#"<html><head>
            <meta property="og:image" content="https://cdn.com/og.jpg">
            <meta name="twitter:image" content="https://cdn.com/tw.jpg">
            <meta itemprop="image" content="https://cdn.com/ip.jpg">
        </head><body></body></html>"#;
        let images = extract_images(html, "https://example.com");
        let urls: Vec<&str> = images.iter().map(|i| i.url.as_str()).collect();
        assert!(urls.contains(&"https://cdn.com/og.jpg"));
        assert!(urls.contains(&"https://cdn.com/tw.jpg"));
        assert!(urls.contains(&"https://cdn.com/ip.jpg"));
    }

    #[test]
    fn images_icon_links_extracted_by_rel_substring() {
        let html = r#"<html><head>
            <link rel="shortcut icon" href="/favicon.ico">
            <link rel="apple-touch-icon" href="/apple.png">
        </head><body></body></html>"#;
        let images = extract_images(html, "https://example.com");
        let urls: Vec<&str> = images.iter().map(|i| i.url.as_str()).collect();
        assert!(urls.contains(&"https://example.com/favicon.ico"));
        assert!(urls.contains(&"https://example.com/apple.png"));
    }

    #[test]
    fn images_video_poster_extracted() {
        let html = r#"<html><body><video poster="/poster.jpg"></video></body></html>"#;
        let images = extract_images(html, "https://example.com");
        assert!(images.iter().any(|i| i.url.ends_with("poster.jpg")));
    }

    #[test]
    fn images_inline_background_style_extracted() {
        let html =
            r#"<html><body><div style="background-image: url('/bg.jpg')"></div></body></html>"#;
        let images = extract_images(html, "https://example.com");
        assert!(images.iter().any(|i| i.url.ends_with("bg.jpg")));
    }

    #[test]
    fn images_background_style_multiple_urls_all_extracted() {
        let html = r#"<html><body>
            <div style="background: url(/bg1.jpg)"></div>
            <div style="background-image: url(&quot;/bg2.jpg&quot;)"></div>
        </body></html>"#;
        let images = extract_images(html, "https://example.com");
        assert!(images.iter().any(|i| i.url.ends_with("bg1.jpg")));
        assert!(images.iter().any(|i| i.url.ends_with("bg2.jpg")));
    }

    #[test]
    fn images_base_href_relative_resolved_against_doc_url() {
        let html = r#"<html><head><base href="/cdn/"></head><body><img src="a.jpg"></body></html>"#;
        let images = extract_images(html, "https://example.com/page");
        assert_eq!(images[0].url, "https://example.com/cdn/a.jpg");
    }

    #[test]
    fn images_base_href_absolute_used_directly() {
        let html = r#"<html><head><base href="https://cdn.example.com/"></head><body><img src="a.jpg"></body></html>"#;
        let images = extract_images(html, "https://example.com/page");
        assert_eq!(images[0].url, "https://cdn.example.com/a.jpg");
    }

    #[test]
    fn images_protocol_relative_src_inherits_page_scheme() {
        let html = r#"<html><body><img src="//images.example.com/a.jpg"></body></html>"#;
        let images = extract_images(html, "https://example.com/page");
        assert_eq!(images[0].url, "https://images.example.com/a.jpg");
    }

    #[test]
    fn images_javascript_scheme_src_is_dropped() {
        let html = r#"<html><body><img src="javascript:alert(1)"></body></html>"#;
        let images = extract_images(html, "https://example.com");
        assert!(images.is_empty());
    }

    #[test]
    fn images_data_and_blob_uris_kept_verbatim() {
        let html = r#"<html><body>
            <img src="data:image/png;base64,AAAA">
            <img src="blob:https://example.com/uuid-1">
        </body></html>"#;
        let images = extract_images(html, "https://example.com");
        assert!(images.iter().any(|i| i.url == "data:image/png;base64,AAAA"));
        assert!(
            images
                .iter()
                .any(|i| i.url == "blob:https://example.com/uuid-1")
        );
    }

    #[test]
    fn images_malformed_relative_src_without_base_is_dropped() {
        let html = r#"<html><body><img src="relative.jpg"></body></html>"#;
        let images = extract_images(html, "not a valid url");
        assert!(images.is_empty());
    }

    #[test]
    fn images_empty_src_is_skipped() {
        let html = r#"<html><body><img src=""><img src="/real.jpg"></body></html>"#;
        let images = extract_images(html, "https://example.com");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].url, "https://example.com/real.jpg");
    }

    #[test]
    fn images_whitespace_padded_src_is_trimmed() {
        let html = "<html><body><img src=\"  /padded.jpg  \"></body></html>";
        let images = extract_images(html, "https://example.com");
        assert_eq!(images[0].url, "https://example.com/padded.jpg");
    }

    #[test]
    fn images_uppercase_https_scheme_falls_through_join_and_normalizes() {
        let html = r#"<html><body><img src="HTTPS://Example.com/x.jpg"></body></html>"#;
        let images = extract_images(html, "https://example.com");
        // Case-sensitive verbatim-check misses "HTTPS://", so it falls through
        // to `base.join(src)`, which parses+normalizes the absolute URL.
        assert_eq!(images[0].url, "https://example.com/x.jpg");
    }

    #[test]
    fn images_does_not_panic_on_deeply_nested_markup() {
        let mut html = String::from("<html><body>");
        for _ in 0..300 {
            html.push_str("<div>");
        }
        html.push_str(r#"<img src="/deep.jpg">"#);
        for _ in 0..300 {
            html.push_str("</div>");
        }
        html.push_str("</body></html>");
        let images = extract_images(&html, "https://example.com");
        assert!(images.iter().any(|i| i.url.ends_with("deep.jpg")));
    }

    // ── srcset_url_tokens: extra edge cases ─────────────────────────────

    #[test]
    fn srcset_url_tokens_leading_trailing_whitespace() {
        assert_eq!(
            srcset_url_tokens("   /a.jpg 1x  ,  /b.jpg 2x   "),
            vec!["/a.jpg", "/b.jpg"]
        );
    }

    #[test]
    fn srcset_url_tokens_tab_and_newline_separators() {
        assert_eq!(
            srcset_url_tokens("/a.jpg 1x,\n\t/b.jpg 2x"),
            vec!["/a.jpg", "/b.jpg"]
        );
    }

    #[test]
    fn srcset_url_tokens_single_url_no_descriptor() {
        assert_eq!(srcset_url_tokens("/only.jpg"), vec!["/only.jpg"]);
    }

    // ── norm_alt / text_density (private helpers) ───────────────────────

    #[test]
    fn norm_alt_trims_and_empty_becomes_none() {
        assert_eq!(norm_alt(Some("  hello  ")), Some("hello".to_string()));
        assert_eq!(norm_alt(Some("")), None);
        assert_eq!(norm_alt(Some("   ")), None);
        assert_eq!(norm_alt(None), None);
    }

    #[test]
    fn text_density_empty_html_is_zero() {
        assert_eq!(text_density(""), 0.0);
    }

    #[test]
    fn text_density_pure_text_fragment_is_positive() {
        let d = text_density("<p>hello world</p>");
        assert!(d > 0.0 && d <= 1.0, "density out of range: {d}");
    }
}
