use crw_core::error::CrwResult;
use scraper::{Html, Selector};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Hard cap on a single sitemap response body. The sitemap spec (sitemaps.org,
/// 2017 revision) raised the per-file limit to 50 MB uncompressed / 50,000
/// URLs, so anything below that risks silently dropping legit sitemaps from
/// large sites. We match the spec ceiling.
const MAX_SITEMAP_BYTES: usize = 50 * 1024 * 1024; // 50 MB

/// Per-fetch timeout for sitemap GET/HEAD. Without this a single slow sitemap
/// can starve the whole map operation budget.
const SITEMAP_FETCH_TIMEOUT_SECS: u64 = 15;

/// Hard ceiling on parallel sitemap GETs against a single host, regardless of
/// the configured `max_concurrency`. Sitemap responses are small and the
/// dominant cost is round-trip latency, so a small ceiling captures most of
/// the speedup without flooding the target.
const SITEMAP_FETCH_CONCURRENCY_CAP: usize = 8;

/// Result of parsing a sitemap document.
///
/// Sitemap-index entries (`<sitemap><loc>`) and urlset entries (`<url><loc>`)
/// are kept separate so callers can recurse into indexes without confusing
/// child sitemap URLs for page URLs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SitemapResult {
    pub page_urls: Vec<String>,
    pub child_sitemaps: Vec<String>,
}

impl SitemapResult {
    pub fn is_empty(&self) -> bool {
        self.page_urls.is_empty() && self.child_sitemaps.is_empty()
    }
}

/// Fetch a single sitemap and return parsed result. Returns empty on any
/// failure (timeout, non-2xx, oversized body, gzip error, HTML masquerade,
/// cross-origin redirect).
///
/// A cross-origin redirect (e.g. `example.com/sitemap.xml` → `evil.com/sm.xml`)
/// is rejected even though the redirect target is SSRF-safe, because the
/// sitemap-tree contract is "stay within the target origin". Without this
/// check, a same-origin seed could 302 us into parsing an attacker-controlled
/// sitemap body. We compare full origin tuples (scheme+host+port).
///
/// Errors are logged but not surfaced — callers iterate over multiple seeds
/// and a single bad sitemap should not abort discovery.
pub async fn fetch_sitemap(url: &str, client: &reqwest::Client) -> CrwResult<SitemapResult> {
    Ok(match fetch_sitemap_raw(url, client).await {
        SitemapOutcome::Parsed(r) => r,
        SitemapOutcome::Challenged | SitemapOutcome::Empty => SitemapResult::default(),
    })
}

/// Outcome of a single plain-HTTP sitemap fetch. `Challenged` is the signal the
/// tree walk uses to decide whether a JS-renderer escalation is worth trying.
enum SitemapOutcome {
    Parsed(SitemapResult),
    /// Anti-bot interstitial (Cloudflare "Just a moment", `cf-mitigated`, or a
    /// 403/503/429). A JS renderer that executes the challenge may recover it.
    Challenged,
    /// Nothing usable and not a recoverable block (404, timeout, off-site,
    /// HTML soft-404, genuinely empty). Escalation would not help.
    Empty,
}

async fn fetch_sitemap_raw(url: &str, client: &reqwest::Client) -> SitemapOutcome {
    let requested_site = match url::Url::parse(url).ok().as_ref().and_then(site_key) {
        Some(k) => k,
        None => {
            tracing::debug!("sitemap fetch skipped: cannot parse origin for {url}");
            return SitemapOutcome::Empty;
        }
    };

    let resp = match client
        .get(url)
        .timeout(std::time::Duration::from_secs(SITEMAP_FETCH_TIMEOUT_SECS))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("sitemap fetch error for {url}: {e}");
            return SitemapOutcome::Empty;
        }
    };

    let final_url = resp.url().clone();
    if final_url.as_str() != url {
        tracing::debug!("sitemap redirect: {} -> {}", url, final_url);
    }
    match site_key(&final_url) {
        Some(ref k) if *k == requested_site => {}
        _ => {
            tracing::warn!("sitemap {url} redirected off-site to {final_url}, dropping");
            return SitemapOutcome::Empty;
        }
    }
    let status = resp.status();
    if !status.is_success() {
        tracing::debug!("sitemap {url} returned {status}");
        // 403/503/429 from an edge WAF is the classic "blocked" signal — let the
        // caller try a JS renderer. Other non-2xx (404, 5xx app errors) are not
        // recoverable that way.
        return if matches!(status.as_u16(), 403 | 429 | 503) {
            SitemapOutcome::Challenged
        } else {
            SitemapOutcome::Empty
        };
    }

    let bytes = match read_body_capped(resp, MAX_SITEMAP_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("sitemap body read failed for {url}: {e}");
            return SitemapOutcome::Empty;
        }
    };

    // Magic bytes are authoritative. Reqwest's `gzip` feature already
    // transparently decodes responses with `Content-Encoding: gzip`, so a
    // `.xml.gz` URL served with that header arrives here as plain XML — the
    // magic bytes will be absent and we skip the decode. We still handle the
    // case where a server returns the raw gzip stream as the body without
    // Content-Encoding (some CDNs do this for `.gz` files).
    let magic_says_gz = bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b;
    let xml_bytes = if magic_says_gz {
        match decode_gzip_capped(&bytes, MAX_SITEMAP_BYTES) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("sitemap gzip decode failed for {url}: {e}");
                return SitemapOutcome::Empty;
            }
        }
    } else {
        bytes
    };

    let text = String::from_utf8_lossy(&xml_bytes);
    let head = sniff_head(&text);
    if has_challenge_markers(&head) {
        // 200 with a Cloudflare interstitial body (managed challenge served as
        // HTTP 200) — recoverable via a JS renderer.
        tracing::warn!("sitemap {url} body is an anti-bot challenge; may escalate");
        return SitemapOutcome::Challenged;
    }
    if looks_like_html(&head) {
        tracing::warn!("sitemap {url} body looks like HTML, not XML; ignoring");
        return SitemapOutcome::Empty;
    }
    let parsed = parse_sitemap(&text);
    // A 2xx, non-HTML body that yields no entries AND declares no sitemap root
    // is not a sitemap at all — a JSON error page, a stray text file, a soft-404
    // that skips the HTML doctype. `Parsed` is the "this document really is a
    // sitemap" signal (the tree walk reports it as one of the site's sitemaps),
    // so it must not cover those. Downstream behaviour is unchanged: both arms
    // produce an empty `SitemapResult`.
    if parsed.is_empty() && !declares_sitemap_root(&head) {
        tracing::debug!("sitemap {url} has no entries and no sitemap root; ignoring");
        return SitemapOutcome::Empty;
    }
    SitemapOutcome::Parsed(parsed)
}

/// True when the document's head declares a sitemap root element. Lets a valid
/// but currently empty `<urlset></urlset>` (a freshly built store) still count
/// as a real sitemap.
fn declares_sitemap_root(head: &str) -> bool {
    head.contains("<urlset") || head.contains("<sitemapindex")
}

/// Issue a HEAD request — used by the discover layer to skip body GETs on
/// fallback sitemap paths that are clearly 404. Returns false on any error
/// (HEAD often unsupported / cached as 4xx by CDNs).
pub async fn head_probe(url: &str, client: &reqwest::Client) -> bool {
    match client
        .head(url)
        .timeout(std::time::Duration::from_secs(SITEMAP_FETCH_TIMEOUT_SECS))
        .send()
        .await
    {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// Stream the body and abort if it would exceed `max` bytes.
async fn read_body_capped(resp: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    let (buf, truncated) = crw_core::body::read_capped(resp.bytes_stream(), max)
        .await
        .map_err(|e: reqwest::Error| e.to_string())?;
    if truncated {
        return Err(format!("body exceeds {max} bytes cap"));
    }
    Ok(buf)
}

/// Decode gzip with the same byte cap as the compressed body. A 10 MB gzip
/// payload can otherwise decompress to many GB ("gzip bomb"); without this
/// cap a single crafted sitemap.xml.gz would OOM the engine.
fn decode_gzip_capped(data: &[u8], max: usize) -> Result<Vec<u8>, String> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    let dec = GzDecoder::new(data);
    // `take(max + 1)` lets us detect overflow: if read_to_end fills exactly
    // `max + 1` bytes the decoded payload is over the cap.
    let mut limited = dec.take((max as u64) + 1);
    let mut out = Vec::new();
    limited.read_to_end(&mut out).map_err(|e| e.to_string())?;
    if out.len() > max {
        return Err(format!("decoded body exceeds {max} bytes cap"));
    }
    Ok(out)
}

/// Lowercased first ≤2048 bytes, split on a UTF-8 char boundary so a multi-byte
/// sequence straddling the 2048th byte can't panic.
fn sniff_head(xml: &str) -> String {
    let trimmed = xml.trim_start();
    let mut head_len = trimmed.len().min(2048);
    while !trimmed.is_char_boundary(head_len) {
        head_len -= 1;
    }
    trimmed[..head_len].to_lowercase()
}

/// Anti-bot interstitial markers (Cloudflare managed challenge / JS challenge).
fn has_challenge_markers(head: &str) -> bool {
    head.contains("just a moment")
        || head.contains("cf-mitigated")
        || head.contains("cf-chl-")
        || head.contains("attention required")
        || head.contains("/cdn-cgi/challenge-platform")
}

/// HTML masquerading as a sitemap (soft-404 pages served with HTTP 200).
fn looks_like_html(head: &str) -> bool {
    head.starts_with("<!doctype html") || head.starts_with("<html")
}

/// Parse a sitemap that a JS renderer fetched (challenge already solved). Chrome
/// wraps XML in `<div id="webkit-xml-viewer-source-xml">…</div>`, but the
/// `<url><loc>` / `<sitemap><loc>` nodes are real DOM elements so the normal
/// selectors still match. Unlike the plain path we must NOT reject on the
/// `<html>` prefix (the rendered document is legitimately HTML-wrapped); only a
/// still-present challenge marker means the renderer failed to clear the wall.
fn parse_rendered_sitemap(html: &str) -> SitemapResult {
    if has_challenge_markers(&sniff_head(html)) {
        return SitemapResult::default();
    }
    parse_sitemap(html)
}

/// Parse sitemap XML and split entries by kind.
///
/// `<url><loc>` → page URLs.
/// `<sitemap><loc>` → child sitemap URLs (a `<sitemapindex>` document).
/// Bare `<loc>` outside either parent is treated as a page URL fallback,
/// only used when no other matches were found.
pub fn parse_sitemap(xml: &str) -> SitemapResult {
    let document = Html::parse_document(xml);
    let mut result = SitemapResult::default();

    if let Ok(sel) = Selector::parse("url > loc") {
        for el in document.select(&sel) {
            let text: String = el.text().collect();
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                result.page_urls.push(trimmed.to_string());
            }
        }
    }

    if let Ok(sel) = Selector::parse("sitemap > loc") {
        for el in document.select(&sel) {
            let text: String = el.text().collect();
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                result.child_sitemaps.push(trimmed.to_string());
            }
        }
    }

    if result.page_urls.is_empty()
        && result.child_sitemaps.is_empty()
        && let Ok(sel) = Selector::parse("loc")
    {
        for el in document.select(&sel) {
            let text: String = el.text().collect();
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                result.page_urls.push(trimmed.to_string());
            }
        }
    }

    result
}

/// Renders a challenged sitemap URL through a JS renderer (executing a
/// Cloudflare/JS challenge) and returns the rendered HTML, or `None` if the
/// render failed. The caller (`discover_urls`) constructs this so this module
/// stays renderer-agnostic and unit-testable.
pub type SitemapRenderFn<'a> =
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync + 'a;

/// Escalation arm for anti-bot-gated sitemaps. A `SitemapOutcome::Challenged`
/// fetch is retried through `render`, bounded by a shared `budget` (chrome
/// renders cost ~100× a plain GET, so a deeply-gated site can't fan out
/// unbounded).
pub struct SitemapEscalator<'a> {
    render: &'a SitemapRenderFn<'a>,
    budget: AtomicUsize,
}

impl<'a> SitemapEscalator<'a> {
    pub fn new(render: &'a SitemapRenderFn<'a>, budget: usize) -> Self {
        Self {
            render,
            budget: AtomicUsize::new(budget),
        }
    }

    /// Claim a budget slot and render. Returns empty if the budget is exhausted,
    /// the render failed, or the rendered page is still a challenge.
    async fn try_render(&self, url: &str) -> SitemapResult {
        // Atomically claim one slot; bail when the budget is spent.
        let mut cur = self.budget.load(Ordering::Relaxed);
        loop {
            if cur == 0 {
                return SitemapResult::default();
            }
            match self.budget.compare_exchange_weak(
                cur,
                cur - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => cur = observed,
            }
        }
        tracing::info!("escalating challenged sitemap via renderer: {url}");
        match (self.render)(url.to_string()).await {
            Some(html) => parse_rendered_sitemap(&html),
            None => SitemapResult::default(),
        }
    }
}

/// Outcome of a sitemap-tree walk: the page URLs the tree declared, plus the
/// sitemap documents we actually fetched and parsed on the way (issue #440).
/// The sitemap files are useful output in their own right — `/map` surfaces
/// them so a caller does not have to re-derive robots.txt and the well-known
/// fallback paths itself.
#[derive(Debug, Clone, Default)]
pub struct SitemapWalk {
    pub pages: Vec<String>,
    /// Sitemap documents that were retrieved and recognised as sitemaps.
    /// Probed-but-404 fallback guesses are not listed. Parents precede their
    /// children, but siblings land in completion order (the level is fetched
    /// concurrently), so within one level the order is not stable across runs.
    /// The list is partial when the walk stops early on a cap or the deadline.
    pub sitemaps: Vec<String>,
}

/// BFS over a sitemap tree. Same-origin filter applies to both child sitemaps
/// and page URLs to prevent the engine from being abused as a sitemap-fetch
/// proxy (a crafted index could otherwise point us at arbitrary public hosts).
///
/// Each BFS level is fetched in parallel via `buffer_unordered(max_concurrency)`
/// — sequential fetching of a 10-child WordPress index would otherwise add
/// roughly 9× latency to discovery, but unbounded `join_all` would also burst
/// up to `max_sitemaps` concurrent same-host requests, ignoring the operator's
/// politeness setting.
#[allow(clippy::too_many_arguments)] // cohesive tuning knobs; a struct adds noise
pub async fn fetch_sitemap_tree(
    seeds: Vec<String>,
    target_origin: &url::Url,
    client: &reqwest::Client,
    max_depth: u32,
    max_sitemaps: usize,
    max_urls: usize,
    max_concurrency: usize,
    escalator: Option<&SitemapEscalator<'_>>,
    deadline: Option<std::time::Instant>,
) -> SitemapWalk {
    use futures::stream::{self, StreamExt};
    use std::collections::HashSet;

    let past_deadline = || deadline.is_some_and(|d| std::time::Instant::now() >= d);

    let target_key = match site_key(target_origin) {
        Some(k) => k,
        None => return SitemapWalk::default(),
    };
    // Sitemap fetches are light (10 MB cap, 15 s timeout), but we still bound
    // them so a sitemap-index with N children never exceeds the configured
    // crawler concurrency. 1 disables the WordPress speedup entirely; cap at
    // a small floor of 2 so politeness=1 still recurses without parallel hits.
    let concurrency = max_concurrency.clamp(1, SITEMAP_FETCH_CONCURRENCY_CAP);

    let mut visited: HashSet<String> = HashSet::new();
    let mut all_pages: HashSet<String> = HashSet::new();
    let mut found_sitemaps: Vec<String> = Vec::new();
    let mut current: Vec<String> = seeds
        .into_iter()
        .filter(|u| same_site(u, &target_key))
        .collect();
    let mut depth: u32 = 0;
    let mut total_fetched: usize = 0;

    while !current.is_empty() && depth <= max_depth && total_fetched < max_sitemaps {
        if past_deadline() {
            tracing::info!("sitemap_tree wall-clock budget spent; returning partial results");
            break;
        }
        let remaining_budget = max_sitemaps.saturating_sub(total_fetched);
        let batch: Vec<String> = current
            .drain(..)
            .filter(|u| visited.insert(u.clone()))
            .take(remaining_budget)
            .collect();

        if batch.is_empty() {
            break;
        }
        total_fetched += batch.len();

        let results: Vec<(String, SitemapResult, bool)> = stream::iter(batch)
            .map(|u| {
                let client = client.clone();
                async move {
                    // `parsed` records that the document itself was retrieved and
                    // really is a sitemap, independent of how many entries it
                    // holds — a valid but empty `<urlset/>` is still one of the
                    // site's sitemaps and must be reported as such.
                    let mut parsed = true;
                    let res = match fetch_sitemap_raw(&u, &client).await {
                        SitemapOutcome::Parsed(r) => r,
                        // Anti-bot wall: retry through the JS renderer if one was
                        // supplied (solves Cloudflare and yields the real XML).
                        // Skip the (expensive) render once the budget is spent so
                        // a deeply-gated index finishes fast with partial results
                        // instead of grinding every child to the timeout.
                        SitemapOutcome::Challenged => match escalator {
                            Some(esc) if deadline.is_none_or(|d| std::time::Instant::now() < d) => {
                                // The escalator collapses "rendered but empty" and
                                // "render failed" into the same empty result, so
                                // content is the only success signal available here.
                                let r = esc.try_render(&u).await;
                                parsed = !r.is_empty();
                                r
                            }
                            _ => {
                                parsed = false;
                                SitemapResult::default()
                            }
                        },
                        SitemapOutcome::Empty => {
                            parsed = false;
                            SitemapResult::default()
                        }
                    };
                    (u, res, parsed)
                }
            })
            .buffer_unordered(concurrency)
            .collect()
            .await;

        let mut next: Vec<String> = Vec::new();
        // Collect child sitemaps eagerly (BFS frontier for the next level).
        // Collect page URLs round-robin ACROSS the leaves in this level, not by
        // draining each leaf fully before the next. Without this, a site whose
        // index lists `artists` before `tabs` (e.g. songsterr: 75 artist leaves
        // of 5000 URLs each, then 854 tab leaves) would fill the entire
        // `max_urls` cap from the first leaf-group — returning 5000 artist-list
        // pages and zero song tabs. Interleaving makes a capped map representative
        // of the whole site instead of just its alphabetically-first section.
        let mut page_lists: Vec<std::vec::IntoIter<String>> = Vec::new();
        for (parent, res, parsed) in results {
            // Only documents we actually retrieved and recognised as sitemaps.
            // A 404, a block we could not clear, or an HTML guess must never be
            // reported as one of the site's sitemaps.
            if parsed {
                found_sitemaps.push(parent);
            }
            page_lists.push(res.page_urls.into_iter());
            for child in res.child_sitemaps {
                if same_site(&child, &target_key) && !visited.contains(&child) {
                    next.push(child);
                }
            }
        }
        'fill: loop {
            let mut progressed = false;
            for it in page_lists.iter_mut() {
                let Some(page) = it.next() else { continue };
                progressed = true;
                if same_site(&page, &target_key) {
                    all_pages.insert(page);
                    if all_pages.len() >= max_urls {
                        break 'fill;
                    }
                }
            }
            if !progressed {
                break;
            }
        }

        if all_pages.len() >= max_urls {
            tracing::info!(
                cap = max_urls,
                "sitemap_tree hit max_urls cap, stopping discovery"
            );
            break;
        }
        current = next;
        depth += 1;
    }
    if total_fetched >= max_sitemaps {
        tracing::info!(
            cap = max_sitemaps,
            fetched = total_fetched,
            "sitemap_tree hit max_sitemaps cap"
        );
    }

    SitemapWalk {
        pages: all_pages.into_iter().collect(),
        sitemaps: found_sitemaps,
    }
}

/// Site identity for sitemap scoping: the lowercased host with a single leading
/// `www.` removed. Scheme and port are intentionally ignored.
///
/// SSRF is *not* this function's job — it is enforced separately at the route
/// entry (`validate_safe_url_resolved`, blocks private IPs / metadata) and at
/// the reqwest level (`safe_redirect_policy`). Here the only contract is "stay
/// on the same site", and real-world sitemaps routinely cross http/https and
/// apex/www for one site: e.g. an https apex (`https://repertuarim.com`) whose
/// sitemap index lists `http://www.repertuarim.com/...` children. The old strict
/// (scheme, host, port) tuple silently dropped every such URL → empty map.
///
/// Only `www.` is collapsed; other subdomains stay distinct (`cdn.x.com`,
/// `blog.x.com` ≠ `x.com`), preserving subdomain isolation.
fn site_key(u: &url::Url) -> Option<String> {
    let scheme = u.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = u.host_str()?.to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    // Ignore scheme, but keep an *explicit non-default* port as part of the
    // identity: `:80`/`:443` collapse to the bare host (so http↔https match),
    // while `localhost:3000` vs `localhost:8080` stay distinct services.
    let default_port = if scheme == "https" { 443 } else { 80 };
    Some(match u.port() {
        Some(p) if p != default_port => format!("{host}:{p}"),
        _ => host.to_string(),
    })
}

fn same_site(u: &str, target: &str) -> bool {
    url::Url::parse(u)
        .ok()
        .as_ref()
        .and_then(site_key)
        .is_some_and(|k| k == target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urlset_into_page_urls() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url><loc>https://example.com/page1</loc></url>
  <url><loc>https://example.com/page2</loc></url>
</urlset>"#;
        let r = parse_sitemap(xml);
        assert_eq!(r.page_urls.len(), 2);
        assert!(r.child_sitemaps.is_empty());
    }

    #[test]
    fn parses_sitemap_index_into_child_sitemaps() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sitemap><loc>https://example.com/wp-sitemap-posts-1.xml</loc></sitemap>
  <sitemap><loc>https://example.com/wp-sitemap-posts-2.xml</loc></sitemap>
</sitemapindex>"#;
        let r = parse_sitemap(xml);
        assert!(r.page_urls.is_empty());
        assert_eq!(r.child_sitemaps.len(), 2);
    }

    #[test]
    fn html_pretending_to_be_sitemap_is_flagged() {
        // Soft-404 HTML on the plain path → looks_like_html (not a challenge).
        let html = "<!DOCTYPE html><html><body>Not Found</body></html>";
        let head = sniff_head(html);
        assert!(looks_like_html(&head));
        assert!(!has_challenge_markers(&head));
    }

    #[test]
    fn cloudflare_challenge_is_flagged_as_challenge() {
        let challenge =
            r#"<html><head><title>Just a moment...</title></head><body>cf-mitigated</body></html>"#;
        assert!(has_challenge_markers(&sniff_head(challenge)));
    }

    #[test]
    fn sniff_handles_utf8_split_at_boundary() {
        // Pad with ASCII so byte 2048 lands inside a multi-byte UTF-8 char.
        // Each `é` is 2 bytes; positioning one to span 2047..2049 forces the
        // sniff window to land mid-character. Must NOT panic.
        let mut s = String::new();
        s.push_str(&"a".repeat(2047));
        s.push('é');
        s.push_str("<urlset><url><loc>https://x.com/p</loc></url></urlset>");
        let head = sniff_head(&s); // must not panic
        assert!(!looks_like_html(&head) && !has_challenge_markers(&head));
        // The body has a urlset deeper than the sniff window, so the parser
        // still picks up the URL.
        let r = parse_sitemap(&s);
        assert!(r.page_urls.iter().any(|u| u.contains("/p")));
    }

    #[test]
    fn empty_xml_returns_empty_result() {
        let xml = "<?xml version=\"1.0\"?><urlset></urlset>";
        let r = parse_sitemap(xml);
        assert!(r.is_empty());
    }

    fn key(u: &str) -> String {
        site_key(&url::Url::parse(u).unwrap()).unwrap()
    }

    #[test]
    fn site_key_normalizes_case_www_scheme_and_port() {
        // scheme, port, case, and a leading www. all collapse to one identity.
        let base = key("https://example.com/x");
        assert_eq!(key("https://example.com:443/y"), base);
        assert_eq!(key("https://Example.COM/"), base);
        assert_eq!(key("http://example.com/x"), base, "scheme ignored");
        assert_eq!(
            key("http://example.com:80/x"),
            base,
            "default http port collapses"
        );
        assert_eq!(key("https://www.example.com/x"), base, "www. collapsed");
        // An explicit non-default port is a distinct service, not the same site.
        assert_ne!(
            key("https://example.com:8443/x"),
            base,
            "explicit port kept"
        );
    }

    #[test]
    fn same_site_collapses_www_and_scheme() {
        // The repertuarim.com case: apex https seed, sitemap lists http://www.
        let apex = key("https://repertuarim.com/");
        assert!(same_site(
            "http://www.repertuarim.com/akor/x-akor-1.html",
            &apex
        ));
        assert!(same_site(
            "https://www.repertuarim.com/maps/akor1.xml",
            &apex
        ));
        assert!(same_site("https://repertuarim.com/x", &apex));

        let www = key("https://www.example.com/");
        assert!(
            same_site("https://example.com/x", &www),
            "apex matches www target"
        );
        assert!(same_site("http://example.com/x", &www));
    }

    #[test]
    fn same_site_keeps_other_hosts_and_subdomains_distinct() {
        // Only www. is special-cased; everything else stays isolated.
        let apex = key("https://example.com/");
        assert!(!same_site("https://evil.com/x", &apex));
        assert!(!same_site("https://cdn.example.com/x", &apex));
        assert!(!same_site("https://blog.example.com/x", &apex));
        assert!(!same_site("https://example.com.evil.com/x", &apex));
    }

    #[test]
    fn same_site_blocks_non_http_schemes() {
        let target = key("https://example.com/");
        assert!(!same_site("ftp://example.com/x", &target));
        assert!(!same_site("file:///etc/passwd", &target));
    }

    #[test]
    fn decode_gzip_round_trips() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        let original =
            b"<?xml version=\"1.0\"?><urlset><url><loc>https://x.com/a</loc></url></urlset>";
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(original).unwrap();
        let gz = enc.finish().unwrap();
        let decoded = decode_gzip_capped(&gz, MAX_SITEMAP_BYTES).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn decode_gzip_rejects_bomb() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        // 2 MB of zeros compresses to a few KB, then expands back to 2 MB.
        let big = vec![0u8; 2 * 1024 * 1024];
        let mut enc = GzEncoder::new(Vec::new(), Compression::best());
        enc.write_all(&big).unwrap();
        let gz = enc.finish().unwrap();
        // Cap below decoded size → must error, not OOM.
        let err = decode_gzip_capped(&gz, 1024 * 1024).unwrap_err();
        assert!(err.contains("exceeds"));
    }

    #[tokio::test]
    async fn fetch_sitemap_handles_404() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&mock)
            .await;
        let client = reqwest::Client::new();
        let r = fetch_sitemap(&format!("{}/sitemap.xml", mock.uri()), &client)
            .await
            .unwrap();
        assert!(r.is_empty());
    }

    #[tokio::test]
    async fn fetch_sitemap_recursion_via_tree() {
        let mock = wiremock::MockServer::start().await;
        let host = mock.uri().replace("http://", "");

        // Index points at two children.
        let index_body = format!(
            r#"<?xml version="1.0"?><sitemapindex>
  <sitemap><loc>http://{host}/sm-1.xml</loc></sitemap>
  <sitemap><loc>http://{host}/sm-2.xml</loc></sitemap>
</sitemapindex>"#
        );
        let leaf_1 = format!(
            r#"<?xml version="1.0"?><urlset>
  <url><loc>http://{host}/page-a</loc></url>
  <url><loc>http://{host}/page-b</loc></url>
</urlset>"#
        );
        let leaf_2 = format!(
            r#"<?xml version="1.0"?><urlset>
  <url><loc>http://{host}/page-c</loc></url>
</urlset>"#
        );

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/sitemap.xml"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(index_body))
            .mount(&mock)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/sm-1.xml"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(leaf_1))
            .mount(&mock)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/sm-2.xml"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(leaf_2))
            .mount(&mock)
            .await;

        let target = url::Url::parse(&mock.uri()).unwrap();
        let client = reqwest::Client::new();
        let urls = fetch_sitemap_tree(
            vec![format!("{}/sitemap.xml", mock.uri())],
            &target,
            &client,
            3,
            25,
            5000,
            8,
            None,
            None,
        )
        .await
        .pages;

        let mut sorted = urls.clone();
        sorted.sort();
        assert_eq!(sorted.len(), 3, "expected 3 leaf page URLs, got {sorted:?}");
        assert!(sorted[0].ends_with("/page-a"));
        assert!(sorted[1].ends_with("/page-b"));
        assert!(sorted[2].ends_with("/page-c"));
    }

    /// Issue #440: the walk reports the sitemap documents it parsed (index and
    /// both children), and never a seed that answered 404.
    #[tokio::test]
    async fn fetch_sitemap_tree_reports_parsed_sitemap_documents() {
        let mock = wiremock::MockServer::start().await;
        let host = mock.uri().replace("http://", "");

        let index_body = format!(
            r#"<?xml version="1.0"?><sitemapindex>
  <sitemap><loc>http://{host}/product-sitemap.xml</loc></sitemap>
</sitemapindex>"#
        );
        let leaf = format!(
            r#"<?xml version="1.0"?><urlset>
  <url><loc>http://{host}/product/1</loc></url>
</urlset>"#
        );
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/sitemap_index.xml"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(index_body))
            .mount(&mock)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/product-sitemap.xml"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(leaf))
            .mount(&mock)
            .await;
        // A well-known fallback guess that does not exist must not be reported.
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/sitemap.xml"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&mock)
            .await;

        let target = url::Url::parse(&mock.uri()).unwrap();
        let client = reqwest::Client::new();
        let walk = fetch_sitemap_tree(
            vec![
                format!("{}/sitemap.xml", mock.uri()),
                format!("{}/sitemap_index.xml", mock.uri()),
            ],
            &target,
            &client,
            3,
            25,
            5000,
            8,
            None,
            None,
        )
        .await;

        let mut sitemaps = walk.sitemaps.clone();
        sitemaps.sort();
        assert_eq!(
            sitemaps,
            vec![
                format!("{}/product-sitemap.xml", mock.uri()),
                format!("{}/sitemap_index.xml", mock.uri()),
            ],
            "expected both parsed sitemaps and no 404 seed"
        );
        assert_eq!(walk.pages.len(), 1);
        assert!(walk.pages[0].ends_with("/product/1"));
    }

    /// A sitemap that is valid but currently lists nothing is still one of the
    /// site's sitemaps; a 2xx body that is not a sitemap at all is not.
    #[tokio::test]
    async fn fetch_sitemap_tree_reports_valid_empty_urlset_but_not_a_stray_body() {
        let mock = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/sitemap.xml"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"></urlset>"#,
            ))
            .mount(&mock)
            .await;
        // 200, not HTML, but not a sitemap either — must not be reported.
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/sitemap_index.xml"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_string(r#"{"error":"not found"}"#),
            )
            .mount(&mock)
            .await;

        let target = url::Url::parse(&mock.uri()).unwrap();
        let client = reqwest::Client::new();
        let walk = fetch_sitemap_tree(
            vec![
                format!("{}/sitemap.xml", mock.uri()),
                format!("{}/sitemap_index.xml", mock.uri()),
            ],
            &target,
            &client,
            3,
            25,
            5000,
            8,
            None,
            None,
        )
        .await;

        assert_eq!(
            walk.sitemaps,
            vec![format!("{}/sitemap.xml", mock.uri())],
            "valid empty urlset counts, a stray JSON body does not"
        );
        assert!(walk.pages.is_empty());
    }

    #[tokio::test]
    async fn fetch_sitemap_tree_filters_cross_origin_children() {
        let mock = wiremock::MockServer::start().await;
        let host = mock.uri().replace("http://", "");

        // Index points at one same-host child + one cross-origin child.
        let index_body = format!(
            r#"<?xml version="1.0"?><sitemapindex>
  <sitemap><loc>http://{host}/sm-good.xml</loc></sitemap>
  <sitemap><loc>https://evil.com/sitemap.xml</loc></sitemap>
</sitemapindex>"#
        );
        let leaf_good = format!(
            r#"<?xml version="1.0"?><urlset>
  <url><loc>http://{host}/ok</loc></url>
</urlset>"#
        );

        wiremock::Mock::given(wiremock::matchers::path("/index.xml"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(index_body))
            .mount(&mock)
            .await;
        wiremock::Mock::given(wiremock::matchers::path("/sm-good.xml"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(leaf_good))
            .mount(&mock)
            .await;

        let target = url::Url::parse(&mock.uri()).unwrap();
        let client = reqwest::Client::new();
        let urls = fetch_sitemap_tree(
            vec![format!("{}/index.xml", mock.uri())],
            &target,
            &client,
            3,
            25,
            5000,
            8,
            None,
            None,
        )
        .await
        .pages;

        // Cross-origin sitemap was filtered → only same-host pages returned.
        assert_eq!(urls.len(), 1);
        assert!(urls[0].ends_with("/ok"));
    }

    #[tokio::test]
    async fn fetch_sitemap_tree_breaks_cycle() {
        let mock = wiremock::MockServer::start().await;
        let host = mock.uri().replace("http://", "");

        // a -> b -> a cycle; visited set must terminate it.
        let a = format!(
            r#"<?xml version="1.0"?><sitemapindex>
  <sitemap><loc>http://{host}/b.xml</loc></sitemap>
</sitemapindex>"#
        );
        let b = format!(
            r#"<?xml version="1.0"?><sitemapindex>
  <sitemap><loc>http://{host}/a.xml</loc></sitemap>
</sitemapindex>"#
        );

        wiremock::Mock::given(wiremock::matchers::path("/a.xml"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(a))
            .mount(&mock)
            .await;
        wiremock::Mock::given(wiremock::matchers::path("/b.xml"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(b))
            .mount(&mock)
            .await;

        let target = url::Url::parse(&mock.uri()).unwrap();
        let client = reqwest::Client::new();
        let urls = fetch_sitemap_tree(
            vec![format!("{}/a.xml", mock.uri())],
            &target,
            &client,
            5,
            25,
            5000,
            8,
            None,
            None,
        )
        .await
        .pages;
        assert!(urls.is_empty());
    }

    #[test]
    fn parse_rendered_sitemap_handles_chrome_xml_viewer_wrapper() {
        // Exactly the shape Chrome returns for an XML URL once Cloudflare clears:
        // the real `<sitemap><loc>` nodes live inside a viewer wrapper div.
        let html = r#"<html><head></head><body>
            <div id="webkit-xml-viewer-source-xml"><sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
              <sitemap><loc>https://www.ultimate-guitar.com/sitemap1.xml</loc></sitemap>
              <sitemap><loc>https://www.ultimate-guitar.com/sitemap2.xml</loc></sitemap>
            </sitemapindex></div></body></html>"#;
        let r = parse_rendered_sitemap(html);
        assert_eq!(r.child_sitemaps.len(), 2);
        assert!(r.page_urls.is_empty());
    }

    #[test]
    fn parse_rendered_sitemap_rejects_unsolved_challenge() {
        // Renderer failed to clear the wall — markers still present.
        let html = r#"<html><head><title>Just a moment...</title></head>
            <body>cf-mitigated</body></html>"#;
        assert!(parse_rendered_sitemap(html).is_empty());
    }

    #[tokio::test]
    async fn fetch_sitemap_tree_escalates_challenged_sitemap() {
        // /sitemap.xml is served as a 403 wall; the escalator (standing in for a
        // JS renderer that solved the challenge) returns the real XML.
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/sitemap.xml"))
            .respond_with(wiremock::ResponseTemplate::new(403).set_body_string("Just a moment..."))
            .mount(&mock)
            .await;

        let host = mock.uri();
        let solved = format!(
            r#"<html><body><div id="webkit-xml-viewer-source-xml"><urlset>
              <url><loc>{host}/page-a</loc></url>
              <url><loc>{host}/page-b</loc></url>
            </urlset></div></body></html>"#
        );
        let render: Box<SitemapRenderFn> = Box::new(move |_u| {
            let solved = solved.clone();
            Box::pin(async move { Some(solved) })
        });
        let escalator = SitemapEscalator::new(&*render, 8);

        let target = url::Url::parse(&mock.uri()).unwrap();
        let client = reqwest::Client::new();
        let seeds = vec![format!("{}/sitemap.xml", mock.uri())];

        // Without an escalator the challenge is dropped → empty.
        let none = fetch_sitemap_tree(seeds.clone(), &target, &client, 3, 25, 5000, 8, None, None)
            .await
            .pages;
        assert!(
            none.is_empty(),
            "challenge must be dropped without escalator"
        );

        // With the escalator the solved XML's page URLs come through.
        let mut got = fetch_sitemap_tree(
            seeds,
            &target,
            &client,
            3,
            25,
            5000,
            8,
            Some(&escalator),
            None,
        )
        .await
        .pages;
        got.sort();
        assert_eq!(
            got,
            vec![format!("{host}/page-a"), format!("{host}/page-b")]
        );
    }

    #[tokio::test]
    async fn fetch_sitemap_rejects_cross_origin_redirect() {
        // A same-origin seed (mock_a) 302s to a different mock host (mock_b).
        // Even though the redirect target is SSRF-safe, fetch_sitemap must
        // refuse to parse the body — otherwise an attacker could host a
        // sitemap on a different origin and have us follow into it.
        let mock_a = wiremock::MockServer::start().await;
        let mock_b = wiremock::MockServer::start().await;

        let body = r#"<?xml version="1.0"?><urlset>
  <url><loc>https://evil.com/owned</loc></url>
</urlset>"#;

        wiremock::Mock::given(wiremock::matchers::path("/sitemap.xml"))
            .respond_with(
                wiremock::ResponseTemplate::new(302)
                    .insert_header("Location", format!("{}/sitemap.xml", mock_b.uri())),
            )
            .mount(&mock_a)
            .await;
        wiremock::Mock::given(wiremock::matchers::path("/sitemap.xml"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(body))
            .mount(&mock_b)
            .await;

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap();
        let result = fetch_sitemap(&format!("{}/sitemap.xml", mock_a.uri()), &client)
            .await
            .unwrap();
        assert!(
            result.is_empty(),
            "cross-origin redirect must yield empty result, got {result:?}"
        );
    }
}
