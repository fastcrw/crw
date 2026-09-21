use crw_core::config::LlmConfig;
use crw_core::error::CrwResult;
use crw_core::types::{
    CrawlRequest, CrawlState, CrawlStatus, LlmUsage, ScrapeData, resolve_pinned_renderer,
};
use crw_extract::readability::extract_links;
use crw_renderer::FallbackRenderer;
use futures::StreamExt;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::robots::RobotsTxt;
use crate::single::derive_target_warning;
use crate::url_filter::normalize_url;

/// Default URL discovery limit when a caller doesn't specify one.
pub const DEFAULT_MAX_DISCOVERED_URLS: usize = 5000;
/// Hard ceiling on `DiscoverOptions::max_urls` — caps memory even when a caller
/// asks for "everything" (a site like songsterr.com exposes ~4.3M sitemap URLs;
/// holding them all is a few hundred MB, which is the most we allow per call).
pub const MAX_DISCOVERED_URLS_CEILING: usize = 5_000_000;

/// Options for running a BFS crawl job.
pub struct CrawlOptions<'a> {
    pub id: Uuid,
    pub req: CrawlRequest,
    pub renderer: Arc<FallbackRenderer>,
    pub max_concurrency: usize,
    pub respect_robots: bool,
    pub requests_per_second: f64,
    pub user_agent: &'a str,
    pub state_tx: tokio::sync::watch::Sender<CrawlState>,
    pub llm_config: Option<&'a LlmConfig>,
    /// Proxy URL for the crawler's reqwest client (robots.txt fetching).
    /// Supports HTTP, HTTPS, and SOCKS5
    /// (e.g. `http://proxy:8080` or `socks5://user:pass@proxy:1080`).
    pub proxy: Option<String>,
    /// Jitter factor for rate limiting (0.0–1.0). 0.2 = ±20% of sleep duration.
    pub jitter_factor: f64,
    /// Per-page deadline budget in milliseconds. Each URL fetched in the
    /// crawl gets a fresh `Deadline` of this length.
    pub deadline_ms_per_page: u64,
    /// Cap on concurrent in-flight requests per eTLD+1 host. `1` enforces
    /// strict politeness; raise via config when scraping owned infrastructure.
    pub per_host_max_concurrent: u32,
    /// From `ExtractionConfig::normalize_tables`. Threaded here so a crawl and
    /// a single scrape of the same URL produce the same markdown; leaving it
    /// hardcoded would silently diverge the two paths once the flag is flipped.
    pub normalize_tables: bool,
    /// From `ExtractionConfig::http_retry_threshold_bytes`. Same reason as
    /// `normalize_tables`: `classify_block` takes it, and a crawl that judged
    /// blocks differently from a single scrape of the same URL would bill the
    /// customer for a wall on one surface and refund it on the other.
    pub http_retry_threshold_bytes: usize,
}

/// Validate that a URL is safe to fetch (scheme + host check).
fn is_safe_url(url: &url::Url) -> bool {
    crw_core::url_safety::validate_safe_url(url).is_ok()
}

/// Send a failed crawl state.
fn send_failed(id: Uuid, state_tx: &tokio::sync::watch::Sender<CrawlState>, error: String) {
    let _ = state_tx.send(CrawlState {
        id,
        success: false,
        status: CrawlStatus::Failed,
        total: 0,
        completed: 0,
        blocked: 0,
        data: vec![],
        error: Some(error),
    });
}

/// Extract same-origin links from a page and enqueue new ones for crawling.
fn enqueue_discovered_links(
    html: &str,
    page_url: &str,
    origin: &str,
    max_pages: usize,
    visited: &mut HashSet<String>,
    queue: &mut VecDeque<(String, u32)>,
    depth: u32,
) {
    let page_links = extract_links(html, page_url);
    for link in page_links {
        if let Ok(link_url) = url::Url::parse(&link) {
            if !is_safe_url(&link_url) {
                continue;
            }
            // Full origin, port included. `map` next door already compares
            // this way (`discover_urls`). Building it as scheme + host dropped
            // the port, so a seed on `https://example.com/` treated
            // `https://example.com:8443/` as the same site and crawled it. That
            // was merely over-broad while the crawl sent no headers of its own;
            // with `CrawlRequest::headers` it would replay the caller's
            // credentials to a different service on the same host.
            if link_url.origin().ascii_serialization() != origin {
                continue;
            }
            // Dedup on the canonical key, but enqueue the link as the page
            // wrote it. Enqueueing the key instead meant the crawl fetched a
            // lowercased URL: on a case-sensitive origin `/docs/Guide` went out
            // as `/docs/guide` and 404'd, `?id=AbC` lost its token, and the
            // document's `source_url` reported a URL that was never requested.
            let key = normalize_url(&link);
            if !visited.contains(&key) && visited.len() < max_pages {
                visited.insert(key);
                // The fragment is never sent on the wire, so strip it here
                // rather than asking the renderer to.
                let mut fetch_url = link_url.clone();
                fetch_url.set_fragment(None);
                queue.push_back((fetch_url.into(), depth + 1));
            }
        }
    }
}

/// Build the placeholder document a crawl or batch returns for a URL it could not read.
///
/// Carries only the URL, the status (0 when there was no response at all) and
/// the reason, stamped through the same `block` field the scrape and batch
/// paths already use, so every surface that already understands "this document
/// is not a page you asked for" understands this one too, and the caller's
/// `completed - blocked` billing keeps it free.
pub fn failed_page(url: &str, status_code: u16, reason: String) -> ScrapeData {
    ScrapeData {
        metadata: crw_core::types::PageMetadata {
            source_url: url.to_string(),
            status_code,
            ..Default::default()
        },
        block: Some(crw_core::types::BlockOutcome {
            vendor: crw_core::types::HTTP_ERROR_VENDOR.to_string(),
            reason,
        }),
        ..Default::default()
    }
}

/// Record a failed page and publish progress, mirroring the success path's
/// bookkeeping so `completed`, `blocked` and `total` never disagree.
fn push_failed_page(
    data: ScrapeData,
    id: Uuid,
    state_tx: &tokio::sync::watch::Sender<CrawlState>,
    results: &mut Vec<ScrapeData>,
    blocked: &mut u32,
    total: u32,
) {
    results.push(data);
    *blocked += 1;
    // Progress carries no data: the full array ships once, in Completed.
    let _ = state_tx.send(CrawlState {
        id,
        success: true,
        status: CrawlStatus::InProgress,
        total,
        completed: results.len() as u32,
        blocked: *blocked,
        data: Vec::new(),
        error: None,
    });
}

/// Run a BFS crawl starting from a URL.
pub async fn run_crawl(opts: CrawlOptions<'_>) {
    // Propagate crawl-level country to every page-fetch through the renderer
    // stack via the same task-local used by `single::scrape_url`.
    let country = opts.req.country.clone();
    crw_renderer::REQUEST_COUNTRY
        .scope(country, run_crawl_inner(opts))
        .await
}

async fn run_crawl_inner(opts: CrawlOptions<'_>) {
    let CrawlOptions {
        id,
        req,
        renderer,
        max_concurrency,
        respect_robots,
        requests_per_second,
        user_agent,
        state_tx,
        llm_config,
        proxy,
        jitter_factor: _,
        deadline_ms_per_page,
        per_host_max_concurrent,
        normalize_tables,
        http_retry_threshold_bytes,
    } = opts;

    let max_depth = req.max_depth.unwrap_or(2).min(10);
    let max_pages = req.max_pages.unwrap_or(100).min(1000) as usize;

    // Per-crawl BYOP proxy pool (req.proxy_list, req.proxy_rotation). Takes
    // precedence over the server-config rotator. A malformed entry fails the
    // whole job — never a silent direct connection (real-IP leak).
    let byop_rotator = match crw_core::ProxyRotator::build(
        &req.proxy_list,
        None,
        req.proxy_rotation.unwrap_or_default(),
    ) {
        Ok(r) => r.map(std::sync::Arc::new),
        Err(e) => {
            send_failed(id, &state_tx, format!("invalid proxy_list: {e}"));
            return;
        }
    };

    // Apply "pinned implies JS" once per crawl, mirroring single.rs. The rule
    // lives in `RequestedRenderer::implies_js` (the single source both entry
    // points consume): browser pins coerce a JS render, the wire-level
    // impersonated pin and an explicit Auto do not.
    let pinned_renderer = resolve_pinned_renderer(req.renderer);
    let effective_render_js =
        if req.renderer.is_some_and(|r| r.implies_js()) && req.render_js.is_none() {
            Some(true)
        } else {
            req.render_js
        };

    let base_url = match url::Url::parse(&req.url) {
        Ok(u) => {
            if let Err(e) = crw_core::url_safety::validate_safe_url_resolved(&u).await {
                send_failed(id, &state_tx, e);
                return;
            }
            u
        }
        Err(e) => {
            send_failed(id, &state_tx, format!("Invalid URL: {e}"));
            return;
        }
    };

    if base_url.host_str().is_none() {
        send_failed(id, &state_tx, "URL has no host".into());
        return;
    }
    let origin = base_url.origin().ascii_serialization();

    // Robots/sitemap egress must match the page egress: prefer the per-crawl
    // BYOP pool, then the config rotator, then the legacy single `proxy`. Never
    // a silent direct connection when a proxy is configured (real-IP leak).
    let robots_proxy: Option<String> = match &byop_rotator {
        Some(b) => Some(b.pick(base_url.host_str()).raw().to_string()),
        None => renderer
            .pick_proxy_for_url(&origin)
            .map(|e| e.raw().to_string())
            .or_else(|| proxy.clone()),
    }
    // An empty/whitespace single `proxy` means "no proxy configured" (e.g. a
    // present-but-empty CRW_CRAWLER__PROXY or a CLI `--proxy ""`, which bypasses
    // config normalization). Treat it as a direct connection rather than handing
    // "" to reqwest::Proxy::all, which rejects it with "builder error"
    // (issue #154). A genuinely malformed non-empty value still fails closed below.
    .filter(|p| !p.trim().is_empty());
    // Without these a seed host that blackholes `/robots.txt` hangs this fetch
    // forever: the job holds one of the process-wide crawl permits and stays
    // `InProgress` indefinitely, and non-terminal jobs are never TTL-evicted.
    let mut client_builder = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(ROBOTS_FETCH_TIMEOUT)
        .connect_timeout(ROBOTS_CONNECT_TIMEOUT)
        .redirect(crw_core::url_safety::safe_redirect_policy());
    if let Some(ref proxy_url) = robots_proxy {
        match reqwest::Proxy::all(proxy_url) {
            Ok(p) => client_builder = client_builder.proxy(p),
            Err(e) => {
                send_failed(
                    id,
                    &state_tx,
                    format!(
                        "invalid crawl proxy URL '{}': {}",
                        crw_core::redact_proxy_url(proxy_url),
                        crw_core::error::reqwest_message(e)
                    ),
                );
                return;
            }
        }
    }
    let client = client_builder
        .build()
        .expect("reqwest client build should not fail");

    // Fails open. An unreadable robots.txt is not a licence to fail the job:
    // the crawl proceeds with no rules, so an origin that 503s the file cannot
    // stop a crawl outright.
    let robots =
        if respect_robots {
            RobotsTxt::fetch(&origin, &client).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "robots.txt unavailable, crawling without its rules");
            RobotsTxt::default()
        })
        } else {
            RobotsTxt::default()
        };

    let semaphore = Arc::new(Semaphore::new(max_concurrency));
    // Key the rate limiter by eTLD+1 so subdomains under the same registered
    // domain (e.g. news.example.com + blog.example.com) share a single
    // limiter rather than each getting their own — otherwise we'd hammer the
    // origin's actual infrastructure at N×rps.
    // Per-host rate limit and concurrency cap are owned by FallbackRenderer
    // via crw_renderer::host_limiter; no need to construct them here.
    let _ = (requests_per_second, per_host_max_concurrent);
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    let mut results: Vec<ScrapeData> = Vec::new();
    // How many completed pages came back a wall or an origin error page. The
    // caller bills off `completed`, so it needs this to bill `completed - blocked`
    // without walking the paginated array.
    let mut blocked: u32 = 0;

    queue.push_back((req.url.clone(), 0));
    visited.insert(normalize_url(&req.url));

    while let Some((url, depth)) = queue.pop_front() {
        if results.len() >= max_pages {
            break;
        }

        // Match on path AND query. `is_allowed(parsed.path())` drops the query,
        // which silently allows everything a site keyed its rules on. The
        // exact failure `is_url_allowed`'s doc comment describes, and which
        // `discover_urls` already avoids. This is the surface that fetches at
        // volume, so it was the one that mattered.
        if let Ok(parsed) = url::Url::parse(&url)
            && !robots.is_url_allowed(&parsed)
        {
            tracing::debug!(url, "Blocked by robots.txt");
            continue;
        }

        let _permit = match semaphore.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                tracing::error!("Semaphore closed unexpectedly");
                break;
            }
        };
        // Per-host rate limit + concurrency cap is now owned by
        // FallbackRenderer (see crw_renderer::host_limiter). Acquiring here
        // would double-acquire the same global semaphore and deadlock with
        // the renderer's acquire when per_host_max_concurrent = 1.

        let page_deadline = crw_core::Deadline::from_request_ms(deadline_ms_per_page);
        // Per-page proxy selection (sticky-per-host by default) so each crawled
        // host egresses through its assigned proxy across both the HTTP and
        // JS/CDP paths. BYOP pool (if supplied) takes precedence over config.
        // Scoped per page since hosts vary across the crawl.
        let resolved_proxy = match &byop_rotator {
            Some(b) => {
                let host = url::Url::parse(&url)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_string));
                Some(std::sync::Arc::new(b.pick(host.as_deref()).clone()))
            }
            None => renderer.pick_proxy_for_url(&url),
        };
        let fetch_fut = renderer.fetch(
            &url,
            &req.headers,
            effective_render_js,
            req.wait_for,
            pinned_renderer,
            page_deadline,
        );
        let mut fetch_result = match crw_renderer::REQUEST_PROXY
            .scope(resolved_proxy, fetch_fut)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(url, error = %e, "Crawl: failed to fetch page");
                push_failed_page(
                    failed_page(&url, 0, e.to_string()),
                    id,
                    &state_tx,
                    &mut results,
                    &mut blocked,
                    visited.len() as u32,
                );
                continue;
            }
        };

        // The CDN answered for a dead origin, so this page has no content and no
        // links worth following: its body is the CDN's error page. It is
        // recorded as a blocked page rather than a crawled one: a crawl of a
        // site whose origin is down was reporting `completed: 1` with
        // Cloudflare's apology as the page. `blocked` keeps it out of the bill
        // (the caller charges `completed - blocked`) while the caller can still
        // see which URL failed and why.
        if crate::single::is_cdn_origin_error(fetch_result.status_code) {
            tracing::warn!(
                url,
                status = fetch_result.status_code,
                "Crawl: CDN could not reach the origin"
            );
            push_failed_page(
                failed_page(
                    &url,
                    fetch_result.status_code,
                    "CDN could not reach origin".to_string(),
                ),
                id,
                &state_tx,
                &mut results,
                &mut blocked,
                visited.len() as u32,
            );
            continue;
        }

        // Extract links for further crawling.
        if depth < max_depth {
            enqueue_discovered_links(
                &fetch_result.html,
                &url,
                &origin,
                max_pages,
                &mut visited,
                &mut queue,
                depth,
            );
        }

        let warning = derive_target_warning(&fetch_result);
        // PDF branch: convert document bytes to markdown instead of running the
        // HTML pipeline (crawl auto-parses PDFs — there is no per-page parsers
        // override on CrawlRequest). Mirrors `single.rs::scrape_url_inner`.
        let mut data = if fetch_result.content_type.as_deref() == Some("application/pdf")
            && fetch_result.raw_bytes.is_some()
        {
            let bytes = fetch_result.raw_bytes.take().unwrap();
            let pdf_req = crw_core::types::ScrapeRequest {
                formats: req.formats.clone(),
                ..Default::default()
            };
            let source = crate::pdf::PdfSource {
                source_url: fetch_result.url.clone(),
                status_code: fetch_result.status_code,
                elapsed_ms: fetch_result.elapsed_ms,
                source_filename: None,
            };
            match crate::pdf::convert_pdf_bytes(bytes, &pdf_req, source).await {
                Ok(data) => data,
                Err(err) => {
                    tracing::warn!(url, error = %err, "Crawl: PDF conversion failed");
                    push_failed_page(
                        failed_page(
                            &url,
                            fetch_result.status_code,
                            format!("PDF conversion failed: {err}"),
                        ),
                        id,
                        &state_tx,
                        &mut results,
                        &mut blocked,
                        visited.len() as u32,
                    );
                    continue;
                }
            }
        } else {
            // Off-reactor, parallelism-bounded extraction (see
            // `crate::extract_pool`). The owned input lets the CPU-bound
            // `extract()` run on the blocking pool without starving the crawl's
            // async fan-out.
            match crate::extract_pool::extract_offloaded(crw_extract::OwnedExtractInput {
                raw_html: fetch_result.html.clone(),
                content_type: fetch_result.content_type.clone(),
                source_url: fetch_result.url.clone(),
                status_code: fetch_result.status_code,
                rendered_with: fetch_result.rendered_with.clone(),
                elapsed_ms: fetch_result.elapsed_ms,
                render_decision: fetch_result.render_decision.clone(),
                credit_cost: fetch_result.credit_cost,
                warnings: fetch_result.warnings.clone(),
                formats: req.formats.clone(),
                only_main_content: req.only_main_content,
                include_tags: Vec::new(),
                exclude_tags: Vec::new(),
                css_selector: None,
                xpath: None,
                chunk_strategy: None,
                query: None,
                filter_mode: None,
                top_k: None,
                domain_selectors: None,
                captured_responses: fetch_result.captured_responses.clone(),
                debug: false,
                debug_sink: None,
                normalize_tables,
            })
            .await
            {
                Ok(data) => data,
                Err(err) => {
                    tracing::warn!(url, error = %err, "Crawl: extraction failed");
                    push_failed_page(
                        failed_page(
                            &url,
                            fetch_result.status_code,
                            format!("extraction failed: {err}"),
                        ),
                        id,
                        &state_tx,
                        &mut results,
                        &mut blocked,
                        visited.len() as u32,
                    );
                    continue;
                }
            }
        };
        data.warning = warning;
        // Surface content type on each discovered page so the SaaS monitor
        // reconciler can hash binary/non-text pages instead of diffing them.
        // The actual change-tracking diff for crawl pages runs SaaS-side via
        // POST /v1/change-tracking/diff, not inline here.
        data.content_type = fetch_result.content_type.clone();

        // Same verdict a single scrape gets. Without this a crawl shipped the
        // Cloudflare wall as the page's markdown, `success: true`, billed — the
        // classifier was simply never called on this path.
        //
        // With one exception: `structural_failure` is not applied here. It means
        // "we got a 200 and produced nothing", and `single::scrape_url` only
        // reaches that verdict after a JS escalation has already failed to
        // recover the page. A crawl does one fetch and no escalation, so the same
        // verdict here would fail JS-hydrated pages that `/v1/scrape` recovers —
        // the cross-surface divergence this change exists to remove, inverted.
        //
        // The renderer may also have reached a verdict of its own, for a body it
        // was about to hand back as a fallback when that body is itself a wall.
        // A NAMED verdict from the classifier still wins (the routing registry
        // learns vendors from it); the carried one fills in when the classifier
        // has nothing. The `structural_failure` exclusion above applies only to
        // the classifier's leg, which is what produces it.
        data.block = crate::single::classify_block(
            fetch_result.status_code,
            fetch_result.content_type.as_deref(),
            &fetch_result.html,
            data.markdown.as_deref(),
            fetch_result.screenshot.is_some(),
            http_retry_threshold_bytes,
            &fetch_result.url,
            fetch_result.final_url.as_deref(),
        )
        .filter(|b| b.vendor != crw_core::types::STRUCTURAL_FAILURE_VENDOR)
        .or_else(|| fetch_result.block.clone());
        // An origin error page is not the page that was asked for either. One
        // helper, so `/v1/scrape` and a crawl of the same URL agree. It is
        // stamped onto `block` because a crawl returns documents, not an
        // envelope with an error code — and the caller's billing filters on
        // exactly this field.
        let is_wall = data.block.is_some();
        if !is_wall && let Some(reason) = data.http_error() {
            data.block = Some(crw_core::types::BlockOutcome {
                vendor: crw_core::types::HTTP_ERROR_VENDOR.to_string(),
                reason,
            });
        }
        if data.block.is_some() {
            // Only a wall loses its body, matching `/v1/scrape`: there the
            // challenge shell is dropped and the origin's error page is kept for
            // the caller to read. Either way the page is marked and not billed.
            if is_wall {
                data.clear_body();
            }
            blocked += 1;
        }

        // Never send a refused page to the model. A wall dodges this by accident
        // (`clear_body()` nulled the markdown); an origin error page keeps its
        // body on purpose, so without the check a crawl with a `jsonSchema` over
        // a 404-heavy site makes one real provider call per error page — pages
        // that are then counted `blocked` and not billed, so we pay and nobody
        // is charged.
        if let (Some(schema), Some(llm)) = (&req.json_schema, llm_config)
            && data.block.is_none()
            && let Some(md) = &data.markdown
        {
            match crw_extract::structured::extract_structured_with_usage(
                md,
                Some(schema),
                None,
                llm,
                None,
            )
            .await
            {
                Ok(result) => {
                    data.json = Some(result.value);
                    // One leg per page today, but the same trap as the other
                    // call sites: accumulate so a second leg here cannot go
                    // unbilled the way it did in single.rs and pdf.rs.
                    LlmUsage::accumulate(&mut data.llm_usage, result.usage);
                }
                Err(e) => {
                    tracing::warn!(url = url.as_str(), "Crawl LLM extraction failed: {e}")
                }
            }
        }

        results.push(data);

        // Send progress with empty data to avoid O(N²) cloning.
        // Full data is sent only in the final Completed state.
        let _ = state_tx.send(CrawlState {
            id,
            success: true,
            status: CrawlStatus::InProgress,
            total: visited.len() as u32,
            completed: results.len() as u32,
            blocked,
            data: vec![],
            error: None,
        });
    }

    let _ = state_tx.send(CrawlState {
        id,
        success: true,
        status: CrawlStatus::Completed,
        total: visited.len() as u32,
        completed: results.len() as u32,
        blocked,
        data: results,
        error: None,
    });
}

/// Options for URL discovery (map endpoint).
pub struct DiscoverOptions<'a> {
    pub base_url: &'a str,
    pub max_depth: u32,
    pub use_sitemap: bool,
    /// Run a short-budget BFS crawl after the sitemap phase to fill gaps.
    /// When false, returns sitemap-only results.
    pub crawl_fallback: bool,
    pub renderer: &'a Arc<FallbackRenderer>,
    pub max_concurrency: usize,
    pub requests_per_second: f64,
    pub user_agent: &'a str,
    /// Proxy URL for the discovery client.
    /// Supports HTTP, HTTPS, and SOCKS5
    /// (e.g. `http://proxy:8080` or `socks5://user:pass@proxy:1080`).
    pub proxy: Option<String>,
    /// Per-page deadline budget in milliseconds.
    pub deadline_ms_per_page: u64,
    /// Per-eTLD+1 concurrency cap. See `CrawlOptions::per_host_max_concurrent`.
    pub per_host_max_concurrent: u32,
    /// Optional /map URL filter (Tier A drop + Tier B strip). When `None`,
    /// behaviour is the legacy `normalize_url` pass-through.
    pub url_filter: Option<Arc<crate::url_filter::UrlFilterCfg>>,
    /// Max URLs to discover. Clamped to `[1, MAX_DISCOVERED_URLS_CEILING]`.
    /// Use `DEFAULT_MAX_DISCOVERED_URLS` for the historical behaviour.
    pub max_urls: usize,
    /// Wall-clock deadline for the WHOLE discovery call.
    ///
    /// Every phase (robots, sitemap probes, sitemap tree, each BFS page) is
    /// clamped by whatever is left of this, and discovery returns the URLs it
    /// has when it runs out. Callers must set it *inside* their own request
    /// timeout: if the caller's `tokio::time::timeout` fires first it drops this
    /// future and every discovered URL is lost — which is exactly the
    /// zero-result 504 this field exists to prevent.
    pub overall_deadline: std::time::Instant,
    /// Skip links that robots.txt disallows. Fetching only; a disallowed link is
    /// still *reported* if we discovered it, because robots governs fetching,
    /// not listing.
    pub respect_robots: bool,
}

/// Result of [`discover_urls`]. URLs in `urls` have already passed the
/// /map filter; `dropped_action_count` and `stripped_tracking_count` are
/// the per-request stats the API response surfaces back to callers.
#[derive(Debug, Clone, Default)]
pub struct DiscoverResult {
    pub urls: Vec<String>,
    pub dropped_action_count: usize,
    pub stripped_tracking_count: usize,
    /// Sitemap documents fetched and parsed during discovery (issue #440).
    /// Empty when `use_sitemap` is off or the site has no reachable sitemap.
    pub sitemaps: Vec<String>,
}

/// If the sitemap phase yields at least this many URLs and `crawl_fallback`
/// is true, the BFS phase still runs but with a much smaller time budget
/// (we have plenty already; spending the full timeout on slow HTML fetches
/// would burn time for marginal gain).
/// Read timeout for the one-shot `robots.txt` fetch. In `run_crawl` this is the
/// only bound on that request: unlike `discover_urls`, `CrawlOptions` carries no
/// overall deadline, and `deadline_ms_per_page` never reaches this call.
const ROBOTS_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Connect timeout for the `robots.txt` fetch.
const ROBOTS_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const DISCOVERY_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const DISCOVERY_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

const SITEMAP_SUFFICIENT_THRESHOLD: usize = 50;
/// Hard ceiling on the BFS crawl phase when sitemap was sufficient.
const BFS_SHORT_BUDGET_SECS: u64 = 30;
/// Recursion depth for the sitemap tree fetch. The per-call fetch *count* is
/// derived from `max_urls` (`sitemap_max_fetches`) so a large `<sitemapindex>`
/// (e.g. ultimate-guitar.com: 84 children; songsterr.com: 854) isn't truncated.
const SITEMAP_MAX_DEPTH: u32 = 3;
/// Per-render deadline for the sitemap anti-bot escalation arm. A Cloudflare
/// managed challenge needs a few seconds of JS execution to clear.
const SITEMAP_ESCALATE_DEADLINE_MS: u64 = 30_000;
/// Hard cap on renderer escalations per map call. Chrome renders cost ~100× a
/// plain GET, so a fully-gated site (every child sitemap behind Cloudflare)
/// can't fan out unbounded; we recover the first N sections within the timeout.
const SITEMAP_ESCALATE_BUDGET: usize = 64;
/// Wall-clock budget for the sitemap phase when escalation is active. Bounds the
/// total time spent solving challenges so the map returns partial results within
/// the caller's request timeout instead of being killed with nothing.
const SITEMAP_PHASE_BUDGET_SECS: u64 = 75;

/// Margin held back so discovery finishes *before* the caller's backstop timeout.
///
/// The backstop drops the future and loses everything; the inner deadline returns
/// partial results. They must not be the same instant, or the race decides whether
/// the caller gets its URLs or a 504.
const DISCOVERY_BACKSTOP_MARGIN: std::time::Duration = std::time::Duration::from_secs(2);

/// The `overall_deadline` to hand [`DiscoverOptions`] for a caller-supplied request
/// timeout. Shared by every caller (v1/v2 map, MCP, CLI) so they cannot drift.
pub fn discovery_deadline(request_timeout: std::time::Duration) -> std::time::Instant {
    let budget = request_timeout
        .saturating_sub(DISCOVERY_BACKSTOP_MARGIN)
        .max(std::time::Duration::from_millis(500));
    std::time::Instant::now() + budget
}

/// Time left before `deadline`, or `None` if it has already passed.
///
/// Every awaited phase of discovery clamps itself with this. Clamping only some
/// of them leaves the total-loss bug open in the ones that aren't: the caller's
/// outer timeout fires, the future is dropped, and every URL found so far is
/// thrown away.
fn remaining_budget(deadline: std::time::Instant) -> Option<std::time::Duration> {
    deadline
        .checked_duration_since(std::time::Instant::now())
        .filter(|d| !d.is_zero())
}

/// Discover URLs from a site (map endpoint).
pub async fn discover_urls(opts: DiscoverOptions<'_>) -> CrwResult<DiscoverResult> {
    let DiscoverOptions {
        base_url,
        max_depth,
        use_sitemap,
        crawl_fallback,
        renderer,
        max_concurrency,
        requests_per_second,
        user_agent,
        proxy,
        deadline_ms_per_page,
        per_host_max_concurrent,
        url_filter,
        max_urls,
        overall_deadline,
        respect_robots,
    } = opts;
    // `0` is the documented "unbounded" sentinel (MCP/Firecrawl) → the ceiling.
    let max_urls = if max_urls == 0 {
        MAX_DISCOVERED_URLS_CEILING
    } else {
        max_urls.clamp(1, MAX_DISCOVERED_URLS_CEILING)
    };
    // Each leaf sitemap holds up to ~50k URLs (spec) but commonly ~5k. To reach
    // a large `max_urls` the tree walk must be allowed to fetch enough children:
    // a 4.3M-URL site (songsterr) needs ~900 leaf fetches. Scale the fetch budget
    // with the requested limit, with a small floor so the default stays cheap.
    let sitemap_max_fetches = (max_urls / 1000).clamp(200, 50_000);
    let mut dropped_action_count: usize = 0;
    let mut stripped_tracking_count: usize = 0;
    // Helper closures around the optional filter — None ⇒ legacy pass-through
    // (`normalize_url`). Both increment the stats counters so the API can
    // surface them in the response.
    let filter_raw = |raw: &str, dropped: &mut usize, stripped: &mut usize| -> Option<String> {
        match url_filter.as_deref() {
            Some(cfg) => match crate::url_filter::filter_and_normalize_raw(raw, cfg) {
                Some(s) => {
                    if raw.contains('?')
                        && let Ok(p) = url::Url::parse(raw)
                        && s != normalize_url(raw)
                        && p.query().is_some()
                    {
                        *stripped += 1;
                    }
                    Some(s)
                }
                None => {
                    *dropped += 1;
                    None
                }
            },
            None => Some(normalize_url(raw)),
        }
    };
    let filter_parsed = |parsed: &url::Url,
                         raw: &str,
                         dropped: &mut usize,
                         stripped: &mut usize|
     -> Option<String> {
        match url_filter.as_deref() {
            Some(cfg) => match crate::url_filter::filter_and_normalize_parsed(parsed, raw, cfg) {
                Some(s) => {
                    if parsed.query().is_some() && s != normalize_url(raw) {
                        *stripped += 1;
                    }
                    Some(s)
                }
                None => {
                    *dropped += 1;
                    None
                }
            },
            None => Some(normalize_url(raw)),
        }
    };
    let parsed = url::Url::parse(base_url)
        .map_err(|e| crw_core::error::CrwError::InvalidRequest(format!("Invalid URL: {e}")))?;

    // The seed's SSRF/DNS resolution is an awaited phase like robots, the sitemap
    // walk and every per-page fetch, so it honours the overall deadline too.
    // Left unclamped it was the one hole in the "every phase is bounded"
    // guarantee: a stalled resolver here would run past the route backstop, get
    // the whole future dropped, and reproduce the 504-with-zero-URLs this change
    // exists to remove — and callers without their own pre-check (the CLI `map`)
    // have nothing else guarding it. A timeout here is fail-closed (an SSRF check
    // cannot be skipped), so it surfaces as a clean error the caller maps, never a
    // silent bypass.
    let seed_budget =
        remaining_budget(overall_deadline).ok_or(crw_core::error::CrwError::Timeout(0))?;
    match tokio::time::timeout(
        seed_budget,
        crw_core::url_safety::validate_safe_url_resolved(&parsed),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(crw_core::error::CrwError::InvalidRequest(e)),
        Err(_) => {
            return Err(crw_core::error::CrwError::Timeout(
                seed_budget.as_millis() as u64
            ));
        }
    }

    // Use the URL's full origin (scheme + host + explicit port). Dropping the
    // port here would silently break sitemap discovery on any non-default-port
    // host, because `fetch_sitemap_tree` filters seeds against the target
    // origin tuple including port.
    if parsed.host_str().is_none() {
        return Err(crw_core::error::CrwError::InvalidRequest(
            "URL has no host".into(),
        ));
    }
    let origin = parsed.origin().ascii_serialization();

    // robots/sitemap egress must match page egress: prefer the config rotator
    // (proxy_list), then the legacy single `proxy`. Fail closed on a bad proxy —
    // never a silent direct connection (real-IP leak).
    let discover_proxy: Option<String> = renderer
        .pick_proxy_for_url(&origin)
        .map(|e| e.raw().to_string())
        .or_else(|| proxy.clone())
        // Empty/whitespace single `proxy` = no proxy configured; never hand "" to
        // reqwest::Proxy::all (it errors with "builder error" — issue #154). A
        // malformed non-empty value still fails closed below. Covers the CLI
        // `--proxy ""` path, which bypasses config-level normalization.
        .filter(|p| !p.trim().is_empty());
    let mut discover_client_builder = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(DISCOVERY_FETCH_TIMEOUT)
        .connect_timeout(DISCOVERY_CONNECT_TIMEOUT)
        .redirect(crw_core::url_safety::safe_redirect_policy());
    if let Some(ref proxy_url) = discover_proxy {
        let p = reqwest::Proxy::all(proxy_url).map_err(|e| {
            // NEVER interpolate `proxy_url` itself: it carries `user:pass@`.
            let redacted = crw_core::redact_proxy_url(proxy_url);
            crw_core::error::CrwError::InvalidRequest(format!(
                "invalid proxy URL '{redacted}': {}",
                crw_core::error::reqwest_message(e)
            ))
        })?;
        discover_client_builder = discover_client_builder.proxy(p);
    }
    let client = discover_client_builder.build().map_err(|e| {
        crw_core::error::CrwError::Internal(format!(
            "http client build: {}",
            crw_core::error::reqwest_message(e)
        ))
    })?;

    // The base URL is always part of the result, so seed it up front and let it
    // count against `max_urls`. Appending it at the very end instead would push the
    // result to `max_urls + 1` whenever the cap was already full and the base was
    // not among the discovered URLs — a caller asking for 10 would get 11.
    let mut all_urls: HashSet<String> = HashSet::new();
    all_urls.insert(base_url.to_string());
    let mut sitemaps: Vec<String> = Vec::new();

    // robots.txt is fetched OUTSIDE the `use_sitemap` block: the BFS phase needs
    // its Disallow rules even when sitemap discovery is switched off. (It used to
    // live inside, which silently disabled robots for `use_sitemap: false`.)
    //
    // The client's own 15s timeout is longer than a short caller timeout, so it
    // is additionally clamped by the overall deadline — otherwise a slow
    // robots.txt alone could burn the whole budget and lose every result.
    // Half the remaining budget, not all of it: a robots.txt that hangs used to
    // be handed the whole deadline, so the seed and the sitemap probes got
    // nothing and discovery came back with zero URLs. Half leaves the work its
    // share. Like `run_crawl` this fails open, because an origin that cannot
    // serve the file has not thereby forbidden anything.
    let robots = match remaining_budget(overall_deadline) {
        Some(budget) => {
            match tokio::time::timeout(budget / 2, RobotsTxt::fetch(&origin, &client)).await {
                Ok(Ok(robots)) => robots,
                Ok(Err(error)) => {
                    tracing::warn!(error = %error, "robots.txt unavailable, discovering without its rules");
                    RobotsTxt::default()
                }
                Err(_) => {
                    tracing::warn!("robots.txt fetch timed out, discovering without its rules");
                    RobotsTxt::default()
                }
            }
        }
        None => RobotsTxt::default(),
    };

    if use_sitemap {
        let seeds: Vec<String> = if !robots.sitemaps.is_empty() {
            robots.sitemaps.clone()
        } else {
            // robots.txt declared nothing — try the well-known fallback paths.
            // /sitemap.xml first because it's the standard entry point and most
            // CMSes (incl. WordPress 5.5+) 301/302 from there to their canonical
            // sitemap index. HEAD probe filters obvious 404s before bodies fetch.
            // /sitemap.xml is the spec-required canonical path — always try
            // it via GET regardless of the HEAD probe (some CDNs return
            // 405/403 to HEAD but answer GET fine, and we don't want to lose
            // the canonical sitemap just because another fallback responded
            // 200 to HEAD). The other fallbacks are CMS-specific guesses; the
            // HEAD probe avoids paying a body GET on obvious 404s for those.
            let canonical = format!("{origin}/sitemap.xml");
            let probe_candidates = [
                format!("{origin}/wp-sitemap.xml"),
                format!("{origin}/sitemap_index.xml"),
                format!("{origin}/sitemap-index.xml"),
            ];
            let probes = probe_candidates.iter().map(|u| {
                let client = client.clone();
                let u = u.clone();
                async move { (u.clone(), crate::sitemap::head_probe(&u, &client).await) }
            });
            // Clamped by the overall deadline: the probes inherit the discovery
            // client's 15s timeout, which outlives a short caller budget. Unbounded,
            // they could run past the deadline and let the route's backstop drop the
            // future — losing every URL found so far, the exact bug we are fixing.
            // On expiry we just skip the CMS-specific guesses; the canonical
            // /sitemap.xml seed is still tried.
            let probe_results = match remaining_budget(overall_deadline) {
                Some(budget) => tokio::time::timeout(budget, futures::future::join_all(probes))
                    .await
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            let mut seeds: Vec<String> = vec![canonical];
            seeds.extend(
                probe_results
                    .into_iter()
                    .filter_map(|(u, ok)| ok.then_some(u)),
            );
            seeds
        };

        // Use the operator's per-host politeness cap for sitemap fanout, not
        // the global `max_concurrency` — a sitemap-index with many children
        // is still N requests against the SAME host, and respecting the
        // configured per-host limit is what an operator who set it expects.
        // `fetch_sitemap_tree` clamps to a small ceiling internally.
        // Anti-bot escalation arm: a sitemap behind a Cloudflare/JS challenge
        // is re-fetched through the renderer (which executes the challenge and
        // returns the real XML). Only wired when a JS renderer is available —
        // otherwise a re-fetch would just hit the same wall. Egress (proxy) is
        // resolved per-URL, identical to the BFS page-fetch path below.
        let escalate_render = move |u: String| -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Option<String>> + Send>,
        > {
            let renderer = renderer.clone();
            Box::pin(async move {
                let empty: HashMap<String, String> = HashMap::new();
                let deadline = crw_core::Deadline::from_request_ms(SITEMAP_ESCALATE_DEADLINE_MS);
                let resolved_proxy = renderer.pick_proxy_for_url(&u);
                let fut = renderer.fetch(&u, &empty, Some(true), None, None, deadline);
                match crw_renderer::REQUEST_PROXY.scope(resolved_proxy, fut).await {
                    Ok(r) => Some(r.html),
                    Err(e) => {
                        tracing::debug!("sitemap escalation render failed for {u}: {e}");
                        None
                    }
                }
            })
        };
        let escalator = renderer.js_capable().then(|| {
            crate::sitemap::SitemapEscalator::new(&escalate_render, SITEMAP_ESCALATE_BUDGET)
        });
        // Wall-clock budget for the sitemap phase — only meaningful when the
        // escalation arm is live (plain-HTTP sitemaps finish in well under it).
        // Without it a fully-gated multi-child index (e.g. UG's 84 children, each
        // behind its own Cloudflare solve) would grind every child to the outer
        // request timeout and return nothing; with it the walk stops and returns
        // whatever it recovered.
        // Always clamped by the overall deadline. SITEMAP_PHASE_BUDGET_SECS (75s)
        // is a hardcoded constant, so on its own it happily outlives a caller who
        // asked for `timeout=10` — the outer timeout would then fire mid-walk and
        // discard everything. Take whichever comes first, and arm it even without
        // an escalator so the plain-HTTP walk is bounded too.
        let sitemap_deadline = Some(
            (std::time::Instant::now() + std::time::Duration::from_secs(SITEMAP_PHASE_BUDGET_SECS))
                .min(overall_deadline),
        );

        // Hard-bounded by the overall deadline, on top of the inner `sitemap_deadline`.
        //
        // The inner deadline is only checked BETWEEN batches: a batch that starts a
        // millisecond before it can still run for a full client timeout (15s), which
        // blows straight through the backstop margin. The route's outer timeout would
        // then fire, drop this future, and throw away every URL — the exact
        // zero-result 504 this whole change exists to prevent. The inner deadline
        // still does the real work (it returns partial sitemap results); this timeout
        // is the guarantee that the phase can never outlive its budget.
        let walk = match remaining_budget(overall_deadline) {
            Some(budget) => tokio::time::timeout(
                budget,
                crate::sitemap::fetch_sitemap_tree(
                    seeds,
                    &parsed,
                    &client,
                    SITEMAP_MAX_DEPTH,
                    sitemap_max_fetches,
                    max_urls,
                    per_host_max_concurrent as usize,
                    escalator.as_ref(),
                    sitemap_deadline,
                ),
            )
            .await
            .unwrap_or_default(),
            None => crate::sitemap::SitemapWalk::default(),
        };
        sitemaps = walk.sitemaps;
        for u in walk.pages {
            if all_urls.len() >= max_urls {
                break;
            }
            if let Some(n) = filter_raw(&u, &mut dropped_action_count, &mut stripped_tracking_count)
            {
                all_urls.insert(n);
            }
        }
    }

    // Sitemap-only mode (or sitemap was empty + crawl_fallback is off).
    if !crawl_fallback {
        let mut urls: Vec<String> = all_urls.into_iter().collect();
        urls.sort();
        return Ok(DiscoverResult {
            urls,
            dropped_action_count,
            stripped_tracking_count,
            sitemaps,
        });
    }

    // The BFS phase is ALWAYS bounded. It used to get a deadline only when the
    // sitemap had already produced >= 50 URLs; a site with no sitemap (e.g. Hacker
    // News) therefore ran the BFS with no budget at all until the outer request
    // timeout killed the future and threw away every URL it had found — a 504 with
    // zero results. A rich sitemap still shortens the budget (we have plenty
    // already); everything else runs to the overall deadline and returns partials.
    let bfs_deadline_at = if all_urls.len() >= SITEMAP_SUFFICIENT_THRESHOLD {
        tracing::info!(
            sitemap_urls = all_urls.len(),
            "sitemap sufficient, BFS will run with short budget"
        );
        (std::time::Instant::now() + std::time::Duration::from_secs(BFS_SHORT_BUDGET_SECS))
            .min(overall_deadline)
    } else {
        overall_deadline
    };

    let max_depth = max_depth.min(10);
    // Per-host limiter is owned by FallbackRenderer (see run_crawl).
    let _ = (requests_per_second, per_host_max_concurrent);
    let mut visited: HashSet<String> = HashSet::new();

    visited.insert(normalize_url(base_url));
    let mut frontier: Vec<String> = vec![base_url.to_string()];

    // BFS runs level by level, fetching each level CONCURRENTLY.
    //
    // The previous loop built a `Semaphore`, acquired a permit, then awaited the
    // fetch inline in the same iteration and dropped the permit at the end of it.
    // Nothing was ever spawned, so real concurrency was 1 and `max_concurrency`
    // was dead weight — every URL was fetched strictly one after another.
    //
    // Level-by-level keeps `visited` single-owner (mutated only between levels,
    // never across tasks), so there is no shared-mutex churn and no chance of the
    // same URL being fetched twice. Note the real ceiling here is the per-host
    // limiter inside `FallbackRenderer::fetch`, not `max_concurrency`: map only
    // follows same-origin links, so every fetch contends for the same host's
    // permits. Politeness is unchanged by design.
    //
    // A level's fetches are STREAMED, not collected: results are merged as each
    // page lands and the level is abandoned the moment `max_urls` is reached.
    // Waiting for a whole batch would make every batch cost as much as its slowest
    // page — and pages are wildly uneven. On Hacker News a `/item?id=` page takes
    // ~6.7s while the list pages that actually yield most of the links take ~0.7s,
    // so batching made map ~6x slower for the same output. Streaming lets the cheap
    // pages satisfy the cap while the expensive ones are still in flight; dropping
    // the stream then cancels them.
    'bfs: for depth in 0..=max_depth {
        if frontier.is_empty() {
            break;
        }
        let mut next_frontier: Vec<String> = Vec::new();
        let level_urls = std::mem::take(&mut frontier);

        {
            let mut in_flight = futures::stream::iter(level_urls.into_iter().map(|url| {
                let renderer = renderer.clone();
                async move {
                    // A page must not start if the budget is already gone, and its own
                    // deadline is clamped to what is left of the overall budget —
                    // otherwise a page begun just before the deadline runs for its full
                    // per-page allowance and overruns the caller's timeout, losing
                    // everything to the outer backstop.
                    let budget = remaining_budget(bfs_deadline_at)?;

                    let parsed = url::Url::parse(&url).ok()?;
                    // The SSRF check resolves DNS, so it is an awaited phase like any
                    // other and must live inside the budget. Slow resolution would
                    // otherwise burn time the per-page deadline below never accounts
                    // for, pushing the request past the caller's backstop.
                    let safe = tokio::time::timeout(
                        budget,
                        crw_core::url_safety::validate_safe_url_resolved(&parsed),
                    )
                    .await;
                    if !matches!(safe, Ok(Ok(()))) {
                        return None;
                    }

                    // Re-read the budget: DNS resolution may have eaten into it.
                    let budget = remaining_budget(bfs_deadline_at)?;
                    let page_ms = (budget.as_millis() as u64).min(deadline_ms_per_page);
                    let discover_deadline = crw_core::Deadline::from_request_ms(page_ms);
                    // Honor the config rotator so discovery page fetches egress through
                    // the proxy (sticky-per-host), not direct. Resolved into REQUEST_PROXY.
                    let resolved_proxy = renderer.pick_proxy_for_url(&url);
                    let empty_headers: HashMap<String, String> = HashMap::new();
                    // render_js = None (auto), not Some(false): SPAs (Angular/React/Vue)
                    // serve an empty app shell over plain HTTP, so HTTP-only discovery
                    // finds zero navigational links and /map returns only the seed URL
                    // (issue #166). Auto lets the renderer escalate thin/SPA shells to a
                    // JS render (staying HTTP-only for static sites), like /scrape.
                    let fetch_fut =
                        renderer.fetch(&url, &empty_headers, None, None, None, discover_deadline);
                    let result = crw_renderer::REQUEST_PROXY
                        .scope(resolved_proxy, fetch_fut)
                        .await
                        .ok()?;
                    Some((url, result.html))
                }
            }))
            .buffer_unordered(max_concurrency.max(1));

            // Merge each page's links as it lands. `visited` and the filter counters
            // have a single owner here, and `urls.sort()` below keeps the output
            // deterministic regardless of the order pages completed in.
            while let Some(page) = in_flight.next().await {
                if all_urls.len() >= max_urls || std::time::Instant::now() >= bfs_deadline_at {
                    break;
                }
                let Some((url, html)) = page else { continue };
                for link in extract_links(&html, &url) {
                    let Ok(link_url) = url::Url::parse(&link) else {
                        continue;
                    };
                    if !is_safe_url(&link_url) {
                        continue;
                    }
                    // Compare full origin (scheme + host + explicit port) so a
                    // non-default-port target (e.g. https://example.com:8443) doesn't
                    // reject its own same-origin links.
                    if link_url.origin().ascii_serialization() != origin {
                        continue;
                    }
                    let Some(normalized) = filter_parsed(
                        &link_url,
                        &link,
                        &mut dropped_action_count,
                        &mut stripped_tracking_count,
                    ) else {
                        continue;
                    };
                    if visited.contains(&normalized) {
                        continue;
                    }
                    visited.insert(normalized.clone());
                    // The URL is REPORTED even if robots disallows it: robots.txt
                    // governs fetching, not listing. Only the re-fetch below is gated.
                    all_urls.insert(normalized.clone());
                    if all_urls.len() >= max_urls {
                        break;
                    }
                    if depth >= max_depth {
                        continue;
                    }
                    // robots gate applies to FETCHING only. Hacker News disallows
                    // `/hide?`, `/vote?`, `/reply?` etc; we were rendering each of those
                    // through a full Chrome ladder, ~10-20s a piece, which is most of
                    // how the 120s budget got burned.
                    if respect_robots
                        && let Ok(next_url) = url::Url::parse(&normalized)
                        && !robots.is_url_allowed(&next_url)
                    {
                        continue;
                    }
                    next_frontier.push(normalized);
                }
            }
        }
        frontier = next_frontier;

        if all_urls.len() >= max_urls || std::time::Instant::now() >= bfs_deadline_at {
            tracing::info!(
                discovered = all_urls.len(),
                "BFS stopping: URL cap or budget reached; returning what we have"
            );
            break 'bfs;
        }
    }

    let mut urls: Vec<String> = all_urls.into_iter().collect();
    urls.sort();
    Ok(DiscoverResult {
        urls,
        dropped_action_count,
        stripped_tracking_count,
        sitemaps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_url_strips_fragment() {
        assert_eq!(
            normalize_url("https://example.com/page#section"),
            "https://example.com/page"
        );
    }

    #[test]
    fn normalize_url_strips_trailing_slash() {
        assert_eq!(
            normalize_url("https://example.com/page/"),
            "https://example.com/page"
        );
    }

    /// A page the crawl could not read must come back marked, not silently
    /// dropped and not billable: `block` is what every surface (v1, v2, the
    /// SaaS biller's `completed - blocked`) already reads to tell a real page
    /// from a refused one.
    #[test]
    fn failed_page_is_marked_blocked_and_carries_the_reason() {
        let d = failed_page("https://example.com/dead", 0, "connect timeout".into());
        assert_eq!(d.metadata.source_url, "https://example.com/dead");
        // No response at all, so there is no status to report.
        assert_eq!(d.metadata.status_code, 0);
        assert!(d.markdown.is_none() && d.html.is_none());
        let block = d.block.expect("a failed page must be marked");
        assert_eq!(block.vendor, crw_core::types::HTTP_ERROR_VENDOR);
        assert_eq!(block.reason, "connect timeout");
        // Not priced by the engine; the caller excludes blocked pages anyway.
        assert_eq!(d.credit_cost, 0);
    }

    /// The CDN-origin-error case keeps the status it answered with, so a caller
    /// can tell a 523 apart from a transport failure.
    #[test]
    fn failed_page_keeps_an_upstream_status_code() {
        let d = failed_page(
            "https://example.com/x",
            523,
            "CDN could not reach origin".into(),
        );
        assert_eq!(d.metadata.status_code, 523);
        assert_eq!(
            d.block.map(|b| b.reason).as_deref(),
            Some("CDN could not reach origin")
        );
    }

    #[test]
    fn normalize_url_folds_scheme_and_host_only() {
        // Scheme and host are case-insensitive per RFC 3986 §6.2.2.1 and are
        // folded; the path is not, so its case survives into the key.
        assert_eq!(
            normalize_url("HTTPS://EXAMPLE.COM/Page"),
            "https://example.com/Page"
        );
    }

    #[test]
    fn normalize_url_path_case_distinguishes_two_pages() {
        // The whole point of not folding the path: these are two documents on
        // any case-sensitive origin and must not share a frontier slot.
        assert_ne!(
            normalize_url("https://example.com/Guide"),
            normalize_url("https://example.com/guide")
        );
    }

    #[test]
    fn normalize_url_combined() {
        assert_eq!(
            normalize_url("https://Example.Com/Path/#fragment"),
            "https://example.com/Path"
        );
    }

    #[test]
    fn normalize_url_no_changes_needed() {
        assert_eq!(
            normalize_url("https://example.com/page"),
            "https://example.com/page"
        );
    }

    #[test]
    fn is_safe_url_http() {
        assert!(is_safe_url(&url::Url::parse("http://example.com").unwrap()));
    }

    #[test]
    fn is_safe_url_https() {
        assert!(is_safe_url(
            &url::Url::parse("https://example.com").unwrap()
        ));
    }

    #[test]
    fn is_safe_url_ftp_blocked() {
        assert!(!is_safe_url(&url::Url::parse("ftp://example.com").unwrap()));
    }

    #[test]
    fn is_safe_url_file_blocked() {
        assert!(!is_safe_url(
            &url::Url::parse("file:///etc/passwd").unwrap()
        ));
    }

    #[test]
    fn is_safe_url_data_blocked() {
        assert!(!is_safe_url(
            &url::Url::parse("data:text/html,<h1>x</h1>").unwrap()
        ));
    }

    #[test]
    fn is_safe_url_localhost_blocked() {
        assert!(!is_safe_url(
            &url::Url::parse("http://localhost:8080").unwrap()
        ));
        assert!(!is_safe_url(&url::Url::parse("http://127.0.0.1").unwrap()));
    }

    #[test]
    fn is_safe_url_private_ip_blocked() {
        assert!(!is_safe_url(&url::Url::parse("http://10.0.0.1").unwrap()));
        assert!(!is_safe_url(
            &url::Url::parse("http://192.168.1.1").unwrap()
        ));
        assert!(!is_safe_url(
            &url::Url::parse("http://169.254.169.254").unwrap()
        ));
    }

    /// Simulate the clamping logic from run_crawl
    fn clamp_depth(max_depth: Option<u32>) -> u32 {
        max_depth.unwrap_or(2).min(10)
    }

    fn clamp_pages(max_pages: Option<u32>) -> usize {
        max_pages.unwrap_or(100).min(1000) as usize
    }

    #[test]
    fn crawl_max_depth_capped_at_10() {
        assert_eq!(clamp_depth(Some(100)), 10);
    }

    #[test]
    fn crawl_max_pages_capped_at_1000() {
        assert_eq!(clamp_pages(Some(5000)), 1000);
    }

    #[test]
    fn crawl_default_depth() {
        assert_eq!(clamp_depth(None), 2);
    }

    #[test]
    fn crawl_default_pages() {
        assert_eq!(clamp_pages(None), 100);
    }

    // ─────────────────────────── normalize_url (additional) ───────────────────────────

    #[test]
    fn normalize_url_empty_string() {
        assert_eq!(normalize_url(""), "");
    }

    #[test]
    fn normalize_url_only_fragment_is_empty() {
        assert_eq!(normalize_url("#section"), "");
    }

    #[test]
    fn normalize_url_takes_text_before_first_hash_only() {
        // split('#').next() stops at the FIRST '#', so anything after a
        // second '#' is discarded along with the first fragment.
        assert_eq!(normalize_url("https://e.test/a#b#c"), "https://e.test/a");
    }

    #[test]
    fn normalize_url_root_path_trailing_slash_stripped() {
        assert_eq!(normalize_url("https://example.com/"), "https://example.com");
    }

    #[test]
    fn normalize_url_multiple_trailing_slashes_all_stripped() {
        assert_eq!(
            normalize_url("https://example.com/path///"),
            "https://example.com/path"
        );
    }

    #[test]
    fn normalize_url_query_string_preserved_byte_exact() {
        // Query values carry IDs, base64 and signatures. Folding them broke
        // every origin that treats them as opaque tokens.
        assert_eq!(
            normalize_url("https://example.com/page?Q=Hello"),
            "https://example.com/page?Q=Hello"
        );
    }

    #[test]
    fn normalize_url_query_and_fragment_both_handled() {
        assert_eq!(
            normalize_url("https://example.com/page?q=1#frag"),
            "https://example.com/page?q=1"
        );
    }

    #[test]
    fn normalize_url_relative_path_no_scheme() {
        assert_eq!(normalize_url("/just/a/path/"), "/just/a/path");
    }

    #[test]
    fn normalize_url_unicode_path_case_preserved() {
        // No percent-encoding either: the key is the caller's bytes with only
        // the authority folded.
        assert_eq!(
            normalize_url("https://example.com/CAFÉ"),
            "https://example.com/CAFÉ"
        );
    }

    #[test]
    fn normalize_url_keeps_userinfo_default_port_and_dot_segments() {
        // Pinning what this function deliberately does NOT do. Routing the key
        // through `Url::parse` would drop the credentials and the `:443`, and
        // collapse `/a/../b`, four silent changes to a value `/map` returns.
        assert_eq!(
            normalize_url("https://u:p@example.com:443/a/../b"),
            "https://u:p@example.com:443/a/../b"
        );
    }

    #[test]
    fn normalize_url_trailing_slash_before_a_query_is_untouched() {
        // `trim_end_matches` works on the whole string, so a slash that is not
        // last is not a trailing slash. Trimming the PATH component instead
        // would turn `/a/?q=1` into `/a?q=1`. On the `/map` path that is a
        // URL the origin may well 301 or 404, which is the bug this change
        // exists to remove, not to relocate.
        assert_eq!(
            normalize_url("https://example.com/a/?q=1"),
            "https://example.com/a/?q=1"
        );
    }

    #[test]
    fn normalize_url_relative_path_case_preserved() {
        // The no-scheme arm. The pre-existing relative-path test uses an
        // already-lowercase path, so it proves nothing about the fold.
        assert_eq!(normalize_url("/Just/A/Path/"), "/Just/A/Path");
    }

    #[test]
    fn normalize_url_authority_without_a_path_is_folded() {
        assert_eq!(normalize_url("HTTPS://EXAMPLE.COM"), "https://example.com");
    }

    #[test]
    fn normalize_url_authority_is_folded_when_a_query_follows_directly() {
        assert_eq!(
            normalize_url("HTTPS://EXAMPLE.COM?Q=1"),
            "https://example.com?Q=1"
        );
    }

    #[test]
    fn normalize_url_very_long_string_no_panic() {
        let long = format!("https://example.com/{}", "a".repeat(5000));
        let out = normalize_url(&long);
        assert!(out.starts_with("https://example.com/"));
    }

    #[test]
    fn normalize_url_whitespace_only() {
        assert_eq!(normalize_url("   "), "   ");
    }

    #[test]
    fn normalize_url_is_idempotent() {
        let once = normalize_url("HTTPS://Example.COM/Path/#frag");
        let twice = normalize_url(&once);
        assert_eq!(once, twice);
    }

    // ─────────────────────────── is_safe_url (additional) ───────────────────────────

    #[test]
    fn is_safe_url_ipv6_loopback_blocked() {
        assert!(!is_safe_url(&url::Url::parse("http://[::1]/").unwrap()));
    }

    #[test]
    fn is_safe_url_ipv6_ipv4_mapped_loopback_blocked() {
        assert!(!is_safe_url(
            &url::Url::parse("http://[::ffff:127.0.0.1]/").unwrap()
        ));
    }

    #[test]
    fn is_safe_url_ipv6_link_local_blocked() {
        assert!(!is_safe_url(&url::Url::parse("http://[fe80::1]/").unwrap()));
    }

    #[test]
    fn is_safe_url_ipv6_public_allowed() {
        assert!(is_safe_url(
            &url::Url::parse("http://[2001:4860:4860::8888]/").unwrap()
        ));
    }

    #[test]
    fn is_safe_url_unspecified_ip_blocked() {
        assert!(!is_safe_url(&url::Url::parse("http://0.0.0.0").unwrap()));
    }

    #[test]
    fn is_safe_url_172_range_boundary_low_allowed() {
        // 172.15.0.0/16 is public; the private range starts at 172.16.
        assert!(is_safe_url(
            &url::Url::parse("http://172.15.255.255").unwrap()
        ));
    }

    #[test]
    fn is_safe_url_172_range_lower_bound_blocked() {
        assert!(!is_safe_url(&url::Url::parse("http://172.16.0.0").unwrap()));
    }

    #[test]
    fn is_safe_url_172_range_upper_bound_blocked() {
        assert!(!is_safe_url(
            &url::Url::parse("http://172.31.255.255").unwrap()
        ));
    }

    #[test]
    fn is_safe_url_172_range_above_boundary_allowed() {
        assert!(is_safe_url(&url::Url::parse("http://172.32.0.0").unwrap()));
    }

    #[test]
    fn is_safe_url_public_ip_allowed() {
        assert!(is_safe_url(&url::Url::parse("http://8.8.8.8").unwrap()));
    }

    #[test]
    fn is_safe_url_null_byte_encoded_blocked() {
        assert!(!is_safe_url(
            &url::Url::parse("http://example.com/%00").unwrap()
        ));
    }

    #[test]
    fn is_safe_url_exceeds_max_length_blocked() {
        let long_path = "a".repeat(2100);
        let url = format!("http://example.com/{long_path}");
        assert!(!is_safe_url(&url::Url::parse(&url).unwrap()));
    }

    #[test]
    fn is_safe_url_punycode_domain_allowed() {
        assert!(is_safe_url(
            &url::Url::parse("http://xn--e1aybc.xn--p1ai").unwrap()
        ));
    }

    #[test]
    fn is_safe_url_non_default_port_public_host_allowed() {
        assert!(is_safe_url(
            &url::Url::parse("http://example.com:8443").unwrap()
        ));
    }

    #[test]
    fn is_safe_url_uppercase_scheme_still_evaluated() {
        assert!(is_safe_url(&url::Url::parse("HTTP://EXAMPLE.COM").unwrap()));
    }

    #[test]
    fn is_safe_url_metadata_google_internal_blocked() {
        assert!(!is_safe_url(
            &url::Url::parse("http://metadata.google.internal").unwrap()
        ));
    }

    #[test]
    fn is_safe_url_wildcard_dns_nip_io_blocked() {
        assert!(!is_safe_url(
            &url::Url::parse("http://169.254.169.254.nip.io").unwrap()
        ));
    }

    // ─────────────────────────── clamp_depth / clamp_pages (additional) ───────────────────────────

    #[test]
    fn crawl_max_depth_exact_boundary_allowed() {
        assert_eq!(clamp_depth(Some(10)), 10);
    }

    #[test]
    fn crawl_max_depth_just_below_boundary() {
        assert_eq!(clamp_depth(Some(9)), 9);
    }

    #[test]
    fn crawl_max_depth_zero() {
        assert_eq!(clamp_depth(Some(0)), 0);
    }

    #[test]
    fn crawl_max_depth_u32_max_clamped() {
        assert_eq!(clamp_depth(Some(u32::MAX)), 10);
    }

    #[test]
    fn crawl_max_pages_exact_boundary_allowed() {
        assert_eq!(clamp_pages(Some(1000)), 1000);
    }

    #[test]
    fn crawl_max_pages_just_below_boundary() {
        assert_eq!(clamp_pages(Some(999)), 999);
    }

    #[test]
    fn crawl_max_pages_zero() {
        assert_eq!(clamp_pages(Some(0)), 0);
    }

    #[test]
    fn crawl_max_pages_u32_max_clamped() {
        assert_eq!(clamp_pages(Some(u32::MAX)), 1000);
    }

    // ─────────────────────────── remaining_budget / discovery_deadline ───────────────────────────

    #[test]
    fn remaining_budget_future_deadline_returns_some() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let budget = remaining_budget(deadline);
        assert!(budget.is_some());
        assert!(budget.unwrap() <= std::time::Duration::from_secs(5));
    }

    #[test]
    fn remaining_budget_past_deadline_returns_none() {
        let deadline = std::time::Instant::now() - std::time::Duration::from_secs(1);
        assert!(remaining_budget(deadline).is_none());
    }

    #[test]
    fn remaining_budget_deadline_right_now_returns_none() {
        // checked_duration_since(now) for a deadline that is "now" (or a
        // hair in the past by the time it's evaluated) is zero-or-none,
        // and the `.filter(|d| !d.is_zero())` rejects both.
        let deadline = std::time::Instant::now();
        assert!(remaining_budget(deadline).is_none());
    }

    #[test]
    fn discovery_deadline_subtracts_backstop_margin() {
        let before = std::time::Instant::now();
        let deadline = discovery_deadline(std::time::Duration::from_secs(10));
        let budget = deadline.duration_since(before);
        // Expect ~8s (10s request timeout minus the 2s margin), allow
        // generous slack for test-runner scheduling jitter.
        assert!(
            budget >= std::time::Duration::from_millis(7500)
                && budget <= std::time::Duration::from_millis(8500),
            "budget was {budget:?}"
        );
    }

    #[test]
    fn discovery_deadline_floors_at_500ms_when_timeout_smaller_than_margin() {
        let before = std::time::Instant::now();
        let deadline = discovery_deadline(std::time::Duration::from_secs(1));
        let budget = deadline.duration_since(before);
        assert!(
            budget >= std::time::Duration::from_millis(400)
                && budget <= std::time::Duration::from_millis(700),
            "budget was {budget:?}"
        );
    }

    #[test]
    fn discovery_deadline_zero_timeout_floors_at_500ms() {
        let before = std::time::Instant::now();
        let deadline = discovery_deadline(std::time::Duration::ZERO);
        let budget = deadline.duration_since(before);
        assert!(
            budget >= std::time::Duration::from_millis(400)
                && budget <= std::time::Duration::from_millis(700),
            "budget was {budget:?}"
        );
    }

    #[test]
    fn discovery_deadline_large_timeout_still_subtracts_margin() {
        let before = std::time::Instant::now();
        let deadline = discovery_deadline(std::time::Duration::from_secs(3600));
        let budget = deadline.duration_since(before);
        assert!(
            budget >= std::time::Duration::from_secs(3597)
                && budget <= std::time::Duration::from_secs(3599),
            "budget was {budget:?}"
        );
    }

    // ─────────────────────────── enqueue_discovered_links ───────────────────────────

    fn links_html(hrefs: &[&str]) -> String {
        let anchors: String = hrefs
            .iter()
            .map(|h| format!(r#"<a href="{h}">link</a>"#))
            .collect();
        format!("<html><body>{anchors}</body></html>")
    }

    #[test]
    fn enqueue_basic_same_origin_links() {
        let html = links_html(&["/a", "/b"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        visited.insert(normalize_url("https://example.com/"));
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue.len(), 2);
        assert!(visited.contains("https://example.com/a"));
        assert!(visited.contains("https://example.com/b"));
    }

    #[test]
    fn enqueue_preserves_document_order() {
        let html = links_html(&["/first", "/second", "/third"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        let order: Vec<String> = queue.iter().map(|(u, _)| u.clone()).collect();
        assert_eq!(
            order,
            vec![
                "https://example.com/first".to_string(),
                "https://example.com/second".to_string(),
                "https://example.com/third".to_string(),
            ]
        );
    }

    #[test]
    fn enqueue_cross_origin_links_filtered_out() {
        let html = links_html(&["https://other.com/page", "/same-origin"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].0, "https://example.com/same-origin");
    }

    #[test]
    fn enqueue_duplicate_links_on_same_page_deduped() {
        let html = links_html(&["/dup", "/dup", "/dup"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn enqueue_already_visited_url_not_requeued() {
        let html = links_html(&["/seen"]);
        let mut visited = HashSet::new();
        visited.insert(normalize_url("https://example.com/seen"));
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn enqueue_cycle_back_to_start_page_not_requeued() {
        // Simulates a page whose links include the crawl's own start URL —
        // a classic cycle. `visited` already holds the start URL (seeded by
        // `run_crawl_inner` before the loop begins), so it must not re-enter
        // the queue.
        let html = links_html(&["/", "/other"]);
        let mut visited = HashSet::new();
        visited.insert(normalize_url("https://example.com/"));
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/page",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            1,
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].0, "https://example.com/other");
    }

    #[test]
    fn enqueue_max_pages_boundary_stops_enqueueing() {
        let html = links_html(&["/a", "/b", "/c"]);
        let mut visited: HashSet<String> =
            ["https://example.com".to_string()].into_iter().collect();
        let mut queue = VecDeque::new();
        // visited already has 2 entries; cap is 2, so nothing new fits.
        visited.insert("https://example.com/existing".to_string());
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            2,
            &mut visited,
            &mut queue,
            0,
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn enqueue_max_pages_exact_boundary_allows_last_slot() {
        let html = links_html(&["/a", "/b"]);
        let mut visited: HashSet<String> =
            ["https://example.com".to_string()].into_iter().collect();
        let mut queue = VecDeque::new();
        // max_pages=2, visited already has 1 entry, so exactly one more fits.
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            2,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn enqueue_relative_links_resolved_against_page_url() {
        let html = links_html(&["about"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/blog/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].0, "https://example.com/blog/about");
    }

    #[test]
    fn enqueue_fragment_stripped_from_queued_url() {
        let html = links_html(&["/page#section"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue[0].0, "https://example.com/page");
    }

    #[test]
    fn enqueue_javascript_mailto_tel_links_ignored() {
        let html = links_html(&[
            "javascript:void(0)",
            "mailto:a@example.com",
            "tel:+1234567890",
            "/real-page",
        ]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].0, "https://example.com/real-page");
    }

    /// A different port is a different origin, matching `discover_urls`'s BFS
    /// phase. This used to be pinned the other way and labelled a known bug:
    /// harmless while the crawl sent no caller headers, a credential-scope hole
    /// the moment `CrawlRequest::headers` existed, since an admin console on
    /// `:9999` would receive the seed's `Authorization`.
    #[test]
    fn enqueue_same_host_different_port_is_a_different_origin() {
        let html = links_html(&["https://example.com:9999/admin"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert!(queue.is_empty(), "a different port must not be followed");
    }

    /// The default port is not a different origin: `Url::origin()` omits it, so
    /// an explicit `:443` link from a plain `https://` seed still gets crawled.
    #[test]
    fn enqueue_explicit_default_port_is_still_same_origin() {
        let html = links_html(&["https://example.com:443/page"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn enqueue_non_ascii_href_no_panic() {
        let html = links_html(&["/café/naïve"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn enqueue_empty_html_no_links() {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            "",
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn enqueue_malformed_html_no_panic() {
        let html = "<html><body><a href=/broken<a href='/other'>text</a>";
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        // Must not panic regardless of what scraper's lenient parser recovers.
        enqueue_discovered_links(
            html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
    }

    #[test]
    fn enqueue_anchor_without_href_ignored() {
        let html = r#"<html><body><a name="top">no href</a><a href="/ok">ok</a></body></html>"#;
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].0, "https://example.com/ok");
    }

    #[test]
    fn enqueue_host_case_variation_links_deduped_via_normalize() {
        // Host case IS folded, so these two hrefs are one page.
        let html = links_html(&["https://EXAMPLE.com/page", "https://example.com/page"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue.len(), 1, "host case is folded, so this is one page");
    }

    #[test]
    fn enqueue_keeps_path_case_of_the_link_as_written() {
        // The regression this file exists to prevent: the crawl used to enqueue
        // its own dedup key, so a mixed-case path went out lowercased and 404'd
        // on any case-sensitive origin.
        let html = links_html(&["/docs/Guide"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue[0].0, "https://example.com/docs/Guide");
    }

    #[test]
    fn enqueue_keeps_query_token_case_of_the_link_as_written() {
        let html = links_html(&["/item?id=AbC123"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue[0].0, "https://example.com/item?id=AbC123");
    }

    #[test]
    fn enqueue_path_case_variants_are_two_distinct_pages() {
        let html = links_html(&["/Guide", "/guide"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue.len(), 2, "case-sensitive paths are distinct pages");
    }

    #[test]
    fn enqueue_trailing_slash_variant_is_deduped_but_fetched_as_written() {
        // `/a/` and `/a` share a dedup key, and whichever link the page wrote
        // first is what goes on the wire, not the stripped key.
        let html = links_html(&["/a/", "/a"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].0, "https://example.com/a/");
    }

    #[test]
    fn enqueue_stops_mid_page_once_cap_reached() {
        let html = links_html(&["/a", "/b", "/c", "/d", "/e"]);
        let mut visited: HashSet<String> =
            ["https://example.com".to_string()].into_iter().collect();
        let mut queue = VecDeque::new();
        // Cap allows 3 more beyond the seed.
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            4,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue.len(), 3);
    }

    #[test]
    fn enqueue_depth_is_incremented_by_one() {
        let html = links_html(&["/a"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            3,
        );
        assert_eq!(queue[0].1, 4);
    }

    #[test]
    fn enqueue_depth_zero_becomes_one() {
        let html = links_html(&["/a"]);
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        enqueue_discovered_links(
            &html,
            "https://example.com/",
            "https://example.com",
            100,
            &mut visited,
            &mut queue,
            0,
        );
        assert_eq!(queue[0].1, 1);
    }
}
