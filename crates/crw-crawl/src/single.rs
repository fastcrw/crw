use crw_core::Deadline;
use crw_core::config::{BUILTIN_UA_POOL, ExtractionConfig, LlmConfig};
use crw_core::error::CrwResult;
use crw_core::types::{
    BlockOutcome, ChangeTrackingMode, FetchResult, OutputFormat, RequestedRenderer,
    STRUCTURAL_FAILURE_VENDOR, ScrapeData, ScrapeRequest, resolve_pinned_renderer,
    resolve_render_js,
};
use crw_renderer::FallbackRenderer;
use crw_renderer::http_only::HttpFetcher;
use crw_renderer::traits::PageFetcher;

use crate::page_cache;
use regex::Regex;
use std::sync::LazyLock;
use std::sync::{Arc, Mutex};

/// Resolve the effective proxy for a request, honoring precedence
/// `req.proxy_list > req.proxy > server config`. The single resolved entry is
/// scoped into `REQUEST_PROXY` so BOTH the HTTP and JS/CDP paths egress through
/// it (no second pick, no path-specific resolution). A malformed BYOP proxy is
/// an `InvalidRequest` error — never a silent direct connection.
fn resolve_request_proxy(
    req: &ScrapeRequest,
    renderer: &FallbackRenderer,
) -> CrwResult<Option<Arc<crw_core::ProxyEntry>>> {
    if !req.proxy_list.is_empty() || req.proxy.is_some() {
        let byop = crw_core::ProxyRotator::build(
            &req.proxy_list,
            req.proxy.as_deref(),
            req.proxy_rotation.unwrap_or_default(),
        )
        .map_err(crw_core::error::CrwError::InvalidRequest)?;
        if let Some(byop) = byop {
            let host = url::Url::parse(&req.url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string));
            return Ok(Some(Arc::new(byop.pick(host.as_deref()).clone())));
        }
    }
    Ok(renderer.pick_proxy_for_url(&req.url))
}

/// Scrape a single URL: fetch → extract → (optional) LLM structured extraction.
///
/// - `user_agent`: base user-agent string from global config.
/// - `default_stealth`: whether stealth headers are active by global config.
/// - `render_js_default`: global `[renderer] render_js_default` config; used only
///   for the `needs_temp_fetcher` HTTP-only gating. The shared renderer applies
///   the same default internally, so we don't forward it to the renderer call.
#[allow(clippy::too_many_arguments)]
pub async fn scrape_url(
    req: &ScrapeRequest,
    renderer: &Arc<FallbackRenderer>,
    llm_config: Option<&LlmConfig>,
    extraction_cfg: &ExtractionConfig,
    user_agent: &str,
    default_stealth: bool,
    render_js_default: Option<bool>,
    deadline: Deadline,
) -> CrwResult<ScrapeData> {
    // Propagate per-request country + the single resolved proxy into the
    // renderer stack via task-locals. `REQUEST_COUNTRY` drives DataImpulse
    // credential country; `REQUEST_PROXY` carries the resolved proxy (BYOP >
    // config) so BOTH the HTTP and JS/CDP paths egress through the same entry.
    let resolved_proxy = resolve_request_proxy(req, renderer)?;
    // `REQUEST_SCREENSHOT` carries the (out-of-band) screenshot params into the
    // renderer stack so the CDP path can capture without trait-signature churn
    // (mirrors REQUEST_PROXY). `Some` only when `formats` asked for it.
    let screenshot_req =
        req.formats
            .contains(&OutputFormat::Screenshot)
            .then_some(crw_renderer::ScreenshotReq {
                full_page: req.screenshot_full_page,
            });
    let result = crw_renderer::REQUEST_COUNTRY
        .scope(req.country.clone(), async move {
            crw_renderer::REQUEST_PROXY
                .scope(resolved_proxy, async move {
                    crw_renderer::REQUEST_SCREENSHOT
                        .scope(screenshot_req, async move {
                            scrape_url_inner(
                                req,
                                renderer,
                                llm_config,
                                extraction_cfg,
                                user_agent,
                                default_stealth,
                                render_js_default,
                                deadline,
                            )
                            .await
                        })
                        .await
                })
                .await
        })
        .await;
    // Single choke point for every scrape (single/crawl/batch all route here):
    // fingerprint the canonical markdown so clients can dedup/cache and evidence
    // offsets can be tied to an exact source revision. Computed here (not in
    // crw-extract) because crw-diff is a crw-crawl dep and MUST NOT be a
    // crw-extract one (the diff engine stays free of the extractor).
    result.map(|mut data| {
        if let Some(md) = data.markdown.as_deref() {
            data.source_hash = Some(crw_diff::snapshot::hash_markdown(md));
        }
        data
    })
}

/// Reject the faults in a scrape template that no fetch can repair, before any
/// network work. Shared by the single scrape and the batch route, so a bad
/// template is one 400 on both surfaces rather than one placeholder document
/// per URL labelled as a block.
pub fn validate_scrape_template(req: &ScrapeRequest) -> CrwResult<()> {
    if req.actions.is_some() {
        return Err(crw_core::error::CrwError::InvalidRequest(
            "The 'actions' parameter is not yet supported. Use cssSelector or xpath for element targeting.".into()
        ));
    }
    // A screenshot is captured via CDP and cannot be produced on the HTTP-only
    // path. An explicit `renderJs:false` + `screenshot` is contradictory: reject
    // it rather than silently return a null screenshot.
    if req.formats.contains(&OutputFormat::Screenshot) && req.render_js == Some(false) {
        return Err(crw_core::error::CrwError::InvalidRequest(
            "screenshot format requires JS rendering; remove renderJs:false (or omit it)".into(),
        ));
    }
    // The impersonated tier executes no JS and has no DOM to capture, so an
    // explicit pin on it is contradictory with both JS-rendering requests.
    // Mirrors the renderJs:false + screenshot rejection just above.
    if req.renderer == Some(RequestedRenderer::ImpersonatedHttp) {
        if req.render_js == Some(true) {
            return Err(crw_core::error::CrwError::InvalidRequest(
                "renderer 'impersonated-http' never executes JS; remove renderJs:true (or omit it)"
                    .into(),
            ));
        }
        if req.formats.contains(&OutputFormat::Screenshot) {
            return Err(crw_core::error::CrwError::InvalidRequest(
                "renderer 'impersonated-http' has no DOM to capture; remove the screenshot format"
                    .into(),
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn scrape_url_inner(
    req: &ScrapeRequest,
    renderer: &Arc<FallbackRenderer>,
    llm_config: Option<&LlmConfig>,
    extraction_cfg: &ExtractionConfig,
    user_agent: &str,
    default_stealth: bool,
    render_js_default: Option<bool>,
    deadline: Deadline,
) -> CrwResult<ScrapeData> {
    validate_scrape_template(req)?;

    // Determine whether stealth headers should be injected for this request.
    let inject_stealth = req.stealth.unwrap_or(default_stealth);

    let pinned = resolve_pinned_renderer(req.renderer);

    // "Pinned implies JS": if the user named a non-auto BROWSER renderer but
    // didn't set renderJs, force JS so auto-gating doesn't silently bypass
    // the pin. The impersonated-http tier is the one wire-level pin that
    // never executes JS (`RequestedRenderer::implies_js`), so it is exempt:
    // the pin chooses a transport, and a renderJs coercion would divert it
    // into the forced-JS arm it cannot serve.
    let pin_implies_js = req.renderer.is_some_and(|r| r.implies_js());
    let effective_render_js_request = if pin_implies_js && req.render_js.is_none() {
        Some(true)
    } else {
        req.render_js
    };

    // Resolve the effective render_js decision (per-request overrides global default).
    // Used for the temp-fetcher HTTP-only gate below so a user with
    // render_js_default=true and a per-request proxy still reaches the JS renderer.
    let effective_render_js = resolve_render_js(effective_render_js_request, render_js_default);

    // For the default/auto case the renderer forces the CDP path for a
    // screenshot (see FallbackRenderer::fetch), and the temp HTTP fetcher below
    // is skipped so the screenshot is never dropped.
    let wants_screenshot = req.formats.contains(&OutputFormat::Screenshot);

    // Validate pinned renderer is available: fail fast with a 400 instead of
    // letting the request reach the dispatcher with a hard-pin to a missing
    // pool. Skip validation when renderJs:false is honored for a BROWSER pin
    // (HTTP-only ignores that pin). An impersonated-http pin is validated
    // regardless of the resolved renderJs: it is a transport choice, and a
    // global render_js_default=false must not silently drop it to plain HTTP.
    if let Some(name) = pinned
        && (effective_render_js != Some(false) || !pin_implies_js)
    {
        let available = renderer.available_renderer_names();
        if !available.contains(&name) {
            return Err(crw_core::error::CrwError::InvalidRequest(format!(
                "renderer '{}' not available; configured renderers: [{}]. \
                 Update server config or omit the 'renderer' field.",
                name,
                available.join(", ")
            )));
        }
    }

    // Use a temporary fetcher ONLY when per-request stealth differs from the
    // shared renderer's config. Proxy egress (config rotation + BYOP) is carried
    // by the REQUEST_PROXY task-local (resolved once in `scrape_url`) and is
    // honored by the shared renderer's HTTP and CDP paths, so it does not need a
    // temp fetcher.
    let needs_temp_fetcher = req.stealth.is_some_and(|s| s != default_stealth);

    // ---- page cache ----
    //
    // Reuse a page we already fetched instead of fetching it again. What is
    // cached is the FETCH, never the extraction, so a second pass with a
    // different schema is a hit and still runs its own extraction. The
    // published contract (`docs/docs/scraping.md`) is one hour by default,
    // clamped to 24 hours, `maxAge: 0` always fetches.
    //
    // Placed after the pinned-renderer availability check above: a pin this
    // deployment cannot serve must still 400 rather than be answered from
    // cache.
    let cache_max_age = page_cache::effective_max_age(req.max_age);
    let cacheable = !cache_max_age.is_zero()
        // The capture belongs to the request that asked for it, and it forces
        // the CDP path.
        && !wants_screenshot
        // Change tracking diffs a LIVE fetch against the caller's previous
        // snapshot. Replaying a page would report `unchanged` for a page that
        // did change: a silent wrong answer, so it is keyed on the format the
        // diff actually runs on (see the changeTracking phase below).
        && !req.formats.contains(&OutputFormat::ChangeTracking)
        // Caller egress. A caller proxy is theirs, and a rotating one is meant
        // to vary.
        && req.proxy.is_none()
        && req.proxy_list.is_empty()
        && req.proxy_rotation.is_none()
        // Caller headers are where a cookie or an authorization token lives, so
        // a header-bearing fetch is personal to that caller: it is never stored
        // and never read. Keying on them would technically separate callers,
        // but it would still hold a logged-in page in memory for an hour after
        // the session that fetched it was revoked, and it would put the token
        // itself in the key. This is what keeps a cached page to what any
        // anonymous caller would have received.
        && req.headers.is_empty()
        // The stealth-override arm draws a fresh random user agent per request
        // (below), so its fetch is not reproducible from the request alone.
        && !needs_temp_fetcher;
    // A fetch that escalated to a browser holds a rendered DOM, which is not
    // what a caller reading the HTTP source would have got. Requests that can
    // escalate and requests that cannot therefore keep separate entries. This
    // mirrors `escalation_eligible` below, minus the content type, which is not
    // known until the page is in hand.
    let may_escalate = effective_render_js != Some(false)
        && !needs_temp_fetcher
        && !renderer.js_renderer_names().is_empty()
        && req.formats.contains(&OutputFormat::Markdown);
    let cache_key = cacheable.then(|| {
        // The resolved proxy reaches this function only through the task-local
        // `scrape_url` scoped it into. Keyed on the server address, never on
        // `raw()`, which carries `user:pass`.
        // The FULL proxy URL, hashed by `build_key`: a rotating residential pool
        // is one gateway host whose exit is chosen by the session token in the
        // credentials, so keying on the host alone would collapse every exit
        // onto one entry.
        let proxy_id = crw_renderer::REQUEST_PROXY
            .try_with(|p| p.as_ref().map(|e| e.raw().to_string()))
            .ok()
            .flatten();
        page_cache::build_key(&page_cache::KeyInputs {
            url: &req.url,
            headers: &req.headers,
            // The value actually handed to the renderer, not the resolved bool
            // used for the temp-fetcher gate.
            render_js: effective_render_js_request,
            wait_for: req.wait_for,
            renderer_pin: pinned,
            force_cloak: req.force_cloak.unwrap_or(false),
            country: req.country.as_deref(),
            proxy_id: proxy_id.as_deref(),
            user_agent,
            cache_scope: req.cache_scope.as_deref(),
            may_escalate,
        })
    });
    // A tight budget makes the renderer ladder skip tiers without setting any
    // flag on the result, so the budget a fetch ran under travels with it. This
    // is the READER's budget: what this request could have achieved on its own.
    // The writer's is measured at commit time, from what was actually left when
    // the page was settled, because a request can start rich and still arrive at
    // the escalation decision with nothing.
    let reader_budget_bucket = page_cache::budget_bucket(deadline.remaining().as_millis() as u64);
    let cache_hit = match &cache_key {
        Some(key) => page_cache::lookup(key, cache_max_age, reader_budget_bucket).await,
        None => None,
    };
    let from_cache = cache_hit.is_some();

    let mut fetch_result = match cache_hit {
        Some(hit) => hit,
        None => {
            if needs_temp_fetcher {
                // Rotate UA from built-in pool when stealth is active, so the request
                // looks like a real browser even for per-request stealth overrides.
                let effective_ua = if inject_stealth {
                    BUILTIN_UA_POOL[rand::random_range(0..BUILTIN_UA_POOL.len())].to_string()
                } else {
                    user_agent.to_string()
                };

                // The impersonated pin never coerces renderJs (see above) and is served
                // by the shared renderer's early pin arm, so it must not be diverted
                // into this plain-HTTP temp fetcher either.
                let is_impersonated_pin = !pin_implies_js && pinned.is_some();
                if effective_render_js == Some(false) && !wants_screenshot && !is_impersonated_pin {
                    // HTTP-only temp fetcher with per-request stealth. Honor REQUEST_PROXY
                    // so a stealth-override request still egresses through the resolved
                    // proxy — fail-closed, a set proxy is never bypassed.
                    let temp_http = match crw_renderer::REQUEST_PROXY
                        .try_with(|p| p.clone())
                        .ok()
                        .flatten()
                    {
                        Some(entry) => HttpFetcher::with_proxy(
                            &effective_ua,
                            entry.raw(),
                            inject_stealth,
                            std::time::Duration::from_secs(30),
                        )?,
                        None => HttpFetcher::new(&effective_ua, None, inject_stealth),
                    };
                    temp_http
                        .fetch(&req.url, &req.headers, req.wait_for, deadline)
                        .await?
                } else {
                    // JS rendering needed (or auto-detect): use the shared renderer which
                    // has CDP backends configured. Inject stealth headers via custom headers
                    // so the shared renderer's CDP connections are still used.
                    let mut merged_headers = req.headers.clone();
                    if inject_stealth {
                        merged_headers
                            .entry("User-Agent".to_string())
                            .or_insert(effective_ua);
                    }
                    renderer
                        .fetch_hinted(
                            &req.url,
                            &merged_headers,
                            effective_render_js_request,
                            req.wait_for,
                            pinned,
                            req.force_cloak.unwrap_or(false),
                            deadline,
                        )
                        .await?
                }
            } else {
                renderer
                    .fetch_hinted(
                        &req.url,
                        &req.headers,
                        effective_render_js_request,
                        req.wait_for,
                        pinned,
                        req.force_cloak.unwrap_or(false),
                        deadline,
                    )
                    .await?
            }
        }
    };

    // When the network actually answered. Extraction sits between here and the
    // commit and can take seconds, so timestamping at commit would let a slow
    // old fetch overwrite a newer one while claiming to be fresher.
    let fetched_at = std::time::Instant::now();
    // Snapshot for the cache while the result is still whole: `raw_bytes` is
    // take()n below for PDFs, and the JS escalation later overwrites only some
    // of these fields in place. Skipped when the result already cannot be
    // stored, so a 50MB PDF that will be rejected is not deep-copied first.
    let worth_a_snapshot = (200..300).contains(&fetch_result.status_code)
        && !fetch_result.truncated
        && !fetch_result.deadline_exceeded;
    let mut cache_candidate =
        (cache_key.is_some() && !from_cache && worth_a_snapshot).then(|| fetch_result.clone());

    let warning = derive_target_warning(&fetch_result);
    // Per-request debug collector — shared across the multi-attempt JS
    // escalation so all candidate ladders land in one trace.
    let debug_enabled = req.debug.unwrap_or(false);
    let debug_sink: Option<Arc<Mutex<crw_extract::DebugCollector>>> = if debug_enabled {
        Some(Arc::new(Mutex::new(crw_extract::DebugCollector::new())))
    } else {
        None
    };
    // Build the OWNED extraction input so the CPU-bound `extract()` can run off
    // the async reactor via `extract_pool::extract_offloaded` (spawn_blocking
    // needs `'static`, so the borrowed `ExtractOptions` can't cross the
    // boundary). `domain_selectors` is wrapped in an `Arc` to avoid deep-copying
    // the host→selector map on every request.
    fn build_owned_extract_input(
        fr: &FetchResult,
        req: &ScrapeRequest,
        extraction_cfg: &ExtractionConfig,
        debug: bool,
        sink: Option<Arc<Mutex<crw_extract::DebugCollector>>>,
    ) -> crw_extract::OwnedExtractInput {
        crw_extract::OwnedExtractInput {
            raw_html: fr.html.clone(),
            content_type: fr.content_type.clone(),
            source_url: fr.url.clone(),
            status_code: fr.status_code,
            rendered_with: fr.rendered_with.clone(),
            elapsed_ms: fr.elapsed_ms,
            render_decision: fr.render_decision.clone(),
            credit_cost: fr.credit_cost,
            warnings: fr.warnings.clone(),
            formats: req.formats.clone(),
            only_main_content: req.only_main_content,
            include_tags: req.include_tags.clone(),
            exclude_tags: req.exclude_tags.clone(),
            css_selector: req.css_selector.clone(),
            xpath: req.xpath.clone(),
            chunk_strategy: req.chunk_strategy.clone(),
            query: req.query.clone(),
            filter_mode: req.filter_mode.clone(),
            top_k: req.top_k,
            domain_selectors: Some(Arc::new(extraction_cfg.domain_selectors.clone())),
            captured_responses: fr.captured_responses.clone(),
            debug,
            debug_sink: sink,
            normalize_tables: extraction_cfg.normalize_tables,
        }
    }
    // ── PDF document branch ────────────────────────────────────────────────
    // When the HTTP renderer captured a PDF body (`raw_bytes`), convert it to
    // markdown via pdf-inspector instead of running the HTML pipeline. Sits
    // BEFORE extract() so every `scrape_url` caller (single scrape, crawl item,
    // search enrichment, batch) inherits PDF support for free. The HTML
    // cleaning + JS-escalation paths are skipped entirely for PDFs; the shared
    // downstream stages (LLM json/summary, change-tracking) run unchanged on
    // the produced `data.markdown` / `data.content_type`.
    let pdf_bytes = if fetch_result.content_type.as_deref() == Some("application/pdf")
        && crate::pdf::pdf_parse_requested(req)
    {
        fetch_result.raw_bytes.take()
    } else {
        None
    };

    let mut effective_warning = warning;
    // Whether this fetch is safe to cache from a "is it actually the page"
    // point of view. A request that cannot escalate (json-only, for instance)
    // may be holding an unrendered shell, and pinning that shell would serve it
    // to every later reader. Defaults true: a PDF has no shell problem, it
    // never escalates and its bytes are the document.
    let mut fetch_is_not_a_shell = true;
    // Set when a JS escalation was accepted, carrying the renderer identity of
    // the fetch that actually produced the body.
    let mut escalated_renderer: Option<(Option<String>, Option<crw_core::types::RenderDecision>)> =
        None;
    let mut data = if let Some(bytes) = pdf_bytes {
        let source = crate::pdf::PdfSource {
            source_url: fetch_result.url.clone(),
            status_code: fetch_result.status_code,
            elapsed_ms: fetch_result.elapsed_ms,
            source_filename: None,
        };
        crate::pdf::convert_pdf_bytes(bytes, req, source).await?
    } else {
        let mut data = crate::extract_pool::extract_offloaded(build_owned_extract_input(
            &fetch_result,
            req,
            extraction_cfg,
            debug_enabled,
            debug_sink.clone(),
        ))
        .await?;
        // LLM-assisted re-extraction when DOM result is low-quality and the
        // operator opted in via [extraction.llm_fallback]. Failure paths inside
        // the helper preserve the original markdown.
        if extraction_cfg.llm_fallback.enable
            && let Some(llm_cfg) = llm_config.or(extraction_cfg.llm.as_ref())
        {
            let params = crw_extract::LlmFallbackParams {
                api_key: &llm_cfg.api_key,
                model: &llm_cfg.model,
                provider: &llm_cfg.provider,
                base_url: llm_cfg.base_url.as_deref(),
                quality_threshold: extraction_cfg.llm_fallback.quality_threshold,
                max_html_bytes: extraction_cfg.llm_fallback.max_html_bytes,
                max_tokens: llm_cfg.max_tokens,
                azure_api_version: llm_cfg.azure_api_version.as_deref(),
                always_run: extraction_cfg.llm_fallback.always_run,
            };
            let _ =
                crw_extract::maybe_run_llm_fallback(&mut data, &fetch_result.html, &params).await;
        }

        // Post-extract escalation: HTTP-only fetch returned 2xx but extraction
        // produced no markdown. Re-fetch with JS rendering forced. Catches sites
        // whose HTML is substantive (so `looks_like_thin_html` doesn't trigger at
        // the renderer layer) but whose content lives entirely in client-side
        // hydration or post-load shadow DOM. Bench analysis: ~13/147 failures.
        // Threshold for "empty enough to trigger an escalation".
        //   - HTTP tier: 100 bytes is enough — even a basic shell exceeds that.
        //   - LightPanda tier: 500 bytes. LightPanda routinely returns 90–200 byte
        //     SPA husks (just <head> + a hydration sentinel) that pass the 100-byte
        //     bar but contain nothing the user wants. Bench analysis showed 6 URLs
        //     where chrome retrieves the full page after lightpanda gave us a 90B
        //     stub (bandbhdwr, cascadehomecenter, laportehardware, apploi,
        //     indiamart, zujuan.xkw) — bumping the lightpanda-only threshold to
        //     500 captures all of them without changing http-tier behavior.
        // Tier of renderer that produced fetch_result. We always escalate from
        // "below" — http and lightpanda → try chrome — but never re-call chrome
        // when chrome already produced the empty markdown (that would just churn).
        // Thresholds default to 100B (http) and 2000B (lightpanda); both are
        // overridable via [extraction] in server config so operators can tune
        // per-deployment without recompiling.
        let prior_renderer = fetch_result.rendered_with.as_deref();
        let retry_threshold = if prior_renderer == Some("lightpanda") {
            extraction_cfg.lightpanda_retry_threshold_bytes
        } else {
            extraction_cfg.http_retry_threshold_bytes
        };
        let md_bytes = data
            .markdown
            .as_deref()
            .map(|s| s.trim().len())
            .unwrap_or(0);
        let md_is_byte_thin = md_bytes < retry_threshold;
        let md_quality = data
            .markdown
            .as_deref()
            .map(crw_extract::quality::analyze_md_only);
        let md_is_low_quality = md_quality
            .as_ref()
            .is_some_and(crw_extract::quality::is_low_quality);
        // `impersonated-http` belongs here for the same reason `http` does: it
        // executes no JS, so a 2xx whose markdown comes out empty is exactly
        // the gap this second-chance escalation exists to close. Leaving it out
        // would mean a page the auto hop cleared at the TLS layer, but whose
        // content is client-rendered, never reaches a browser from this layer
        // either.
        let used_low_tier = matches!(
            prior_renderer,
            Some("http")
                | Some("http_only_fallback")
                | Some("lightpanda")
                | Some("impersonated-http")
        );
        // Only escalate on 2xx here. Renderer-level (lib.rs) already handles
        // soft-block status codes (401/403/405/406/410/412/429/451/503) via its
        // own `is_auth_blocked` path; running another escalation from this layer
        // would just hit the same circuit breakers a second time and waste a
        // request budget. Our job here is the 2xx-with-empty-markdown gap that
        // the renderer's HTML-shape thinness heuristic doesn't catch.
        let should_escalate_status = (200..300).contains(&fetch_result.status_code);
        let escalation_eligible = effective_render_js != Some(false)
        && !needs_temp_fetcher
        && !renderer.js_renderer_names().is_empty()
        && req.formats.contains(&OutputFormat::Markdown)
        // Never JS-render a PDF: even when parsing is disabled (`parsers: []`)
        // the document has no DOM to escalate into.
        && fetch_result.content_type.as_deref() != Some("application/pdf");

        // Deliberately NOT `escalation_eligible`: being allowed to escalate is
        // not evidence that escalating happened. The escalation can be skipped
        // for budget, find no chrome tier, exhaust its ladder, or fail
        // outright, and each of those leaves the thin body in place. Storing a
        // thin page pins it for every later reader, so a thin page is simply
        // never stored.
        fetch_is_not_a_shell = !md_is_byte_thin;

        let escalate_for_quality = escalate_for_quality(
            md_is_byte_thin,
            md_is_low_quality,
            fetch_result.status_code,
            &fetch_result.html,
            fetch_result.content_type.as_deref(),
        );
        // A request whose JS ladder already failed comes back as an HTTP body
        // carrying the `js_escalation_failed:` warning. It looks exactly like a
        // low-tier result, so without this check we would re-run the whole
        // ladder that was just exhausted — a second HTTP fetch, a second full JS
        // attempt and a second extraction, on the same (already spent) deadline,
        // for a result that cannot differ.
        let js_ladder_exhausted = fetch_result
            .warning
            .as_deref()
            .is_some_and(|w| w.contains(crw_renderer::JS_ESCALATION_FAILED));
        // The renderer ladder enforces this same floor per tier; apply it one
        // layer up so we never DISPATCH an escalation that cannot run. `deadline`
        // is Copy and is the same one the first fetch already spent, so by this
        // point it is routinely near-exhausted — prod measured 432 of 536
        // escalations starting with <50ms of budget, which is where the
        // "JS escalation after empty markdown failed: Timeout after 1ms" lines
        // come from. Those attempts cannot succeed; they only burn a pool slot
        // and hide the real outcome behind a fabricated timeout.
        let escalation_budget = deadline.remaining();
        let has_escalation_budget = escalation_budget >= crw_renderer::MIN_TIER_BUDGET;
        // If the prior tier was lightpanda (returned 200 with thin/no content that
        // fooled the renderer-level thinness check), escalate to chrome, and when
        // this deployment runs no Chrome sidecar, to whatever stronger tier it does
        // have. Pinning the bare literal made the second case a dead end: the pool
        // rejects a name it does not hold outright, so the escalation failed on the
        // pin rather than on the page and a configured, healthy tier was never
        // reached. Chrome stays the first choice so a pool that holds it behaves
        // exactly as before, including for a host the preference learner has already
        // promoted to chrome; `auto_ladder_names` supplies the fallback and drops
        // the tiers the auto chain would not have entered by itself.
        // `None` means there is nothing above lightpanda, so the escalation is
        // skipped rather than dispatched: "auto" would re-render the same tier for
        // the same thin result, and a pin the pool cannot satisfy only produces a
        // misleading failure.
        // Otherwise (http tier), pass the caller's pin through, or `None` so
        // the chain decides and chrome is reached through the existing
        // failover path.
        let escalation_target: Option<&str> = if prior_renderer == Some("lightpanda") {
            renderer.lightpanda_escalation_target()
        } else {
            pinned
        };
        let has_escalation_target =
            escalation_target.is_some() || prior_renderer != Some("lightpanda");
        let should_escalate = (md_is_byte_thin || escalate_for_quality)
            && used_low_tier
            && !js_ladder_exhausted
            && should_escalate_status
            && escalation_eligible
            && has_escalation_budget
            && has_escalation_target;
        if (md_is_byte_thin || escalate_for_quality)
            && used_low_tier
            && should_escalate_status
            && escalation_eligible
            && has_escalation_budget
            && !has_escalation_target
        {
            tracing::debug!(
                url = %req.url,
                pool = ?renderer.auto_ladder_names(),
                "skipping JS escalation: no tier above lightpanda in this pool"
            );
            // Say so on the response as well. The thin body ships as a
            // success, and without this line nothing tells the operator that
            // the deployment has no browser tier to render the page with.
            let skip_warning = "JS escalation skipped: no chrome tier is configured above \
                                lightpanda; add one for full SPA rendering"
                .to_string();
            effective_warning = Some(match effective_warning {
                Some(w) => format!("{w}; {skip_warning}"),
                None => skip_warning,
            });
        }
        if (md_is_byte_thin || escalate_for_quality)
            && used_low_tier
            && should_escalate_status
            && escalation_eligible
            && !has_escalation_budget
        {
            tracing::debug!(
                url = %req.url,
                remaining_ms = escalation_budget.as_millis() as u64,
                min_ms = crw_renderer::MIN_TIER_BUDGET.as_millis() as u64,
                "skipping JS escalation: not enough deadline left to attempt it"
            );
        }
        if should_escalate {
            let quality_score_before = md_quality.as_ref().map(|q| q.score);
            tracing::info!(
                url = %req.url,
                status = fetch_result.status_code,
                html_len = fetch_result.html.len(),
                prior = prior_renderer,
                target = escalation_target,
                md_bytes,
                quality_score_before = ?quality_score_before,
                escalate_for_quality,
                "empty markdown after fetch, escalating to JS renderer"
            );
            match renderer
                .fetch(
                    &req.url,
                    &req.headers,
                    Some(true),
                    req.wait_for,
                    escalation_target,
                    deadline,
                )
                .await
            {
                Ok(mut js_fetch) => {
                    // Accept JS result even if status >= 400, as long as it produced
                    // real content. Anti-bot/UA-detection sites frequently return a
                    // 4xx code while still serving the actual page body — the status
                    // is a soft signal, not a content gate.
                    let js_status = js_fetch.status_code;
                    let js_warning = derive_target_warning(&js_fetch);
                    if let Ok(js_data) =
                        crate::extract_pool::extract_offloaded(build_owned_extract_input(
                            &js_fetch,
                            req,
                            extraction_cfg,
                            debug_enabled,
                            debug_sink.clone(),
                        ))
                        .await
                    {
                        let js_md_len = js_data
                            .markdown
                            .as_deref()
                            .map(|s| s.trim().len())
                            .unwrap_or(0);
                        let js_md_quality = js_data
                            .markdown
                            .as_deref()
                            .map(crw_extract::quality::analyze_md_only);
                        let js_score = js_md_quality.as_ref().map(|q| q.score).unwrap_or(0.0);
                        let before_score = md_quality.as_ref().map(|q| q.score).unwrap_or(0.0);
                        let http_was_thin = md_is_byte_thin;
                        let quality_improved = js_score > before_score + 0.05;
                        let accept =
                            js_md_len >= retry_threshold && (http_was_thin || quality_improved);
                        if accept {
                            // Remember which tier really produced the body, so
                            // the entry assembled after the swap below can say
                            // so. Without it a replay reports the discarded
                            // tier, `used_low_tier` turns true, and every hit
                            // re-runs this escalation: the exact cost the cache
                            // exists to remove.
                            escalated_renderer = Some((
                                js_fetch.rendered_with.clone(),
                                js_fetch.render_decision.clone(),
                            ));
                            data = js_data;
                            // The escalation re-rendered via CDP, so a screenshot (if
                            // requested) lives on `js_fetch`, not the original low-tier
                            // `fetch_result`. Carry it over so it isn't dropped.
                            fetch_result.screenshot = js_fetch.screenshot.take();
                            // Same for the body: `classify_block` below reads
                            // `fetch_result.html`, and leaving the DISCARDED tier's
                            // shell there means a challenge we just solved still
                            // carries `_cf_chl_opt` into CF_STRONG_MARKERS — which
                            // runs ahead of the markdown guard and would clear the
                            // page this escalation just recovered.
                            // `content_type` is deliberately NOT swapped: every
                            // browser tier hardcodes it to `None`, and it is read
                            // later for `data.content_type`.
                            fetch_result.html = std::mem::take(&mut js_fetch.html);
                            // And for the same reason, the wall verdict. This is
                            // an assignment, not a merge, which is the point: it
                            // carries a verdict the escalation reached AND clears
                            // one the discarded tier left behind, so a wall we
                            // just escaped cannot be stamped onto the page that
                            // escaped it.
                            //
                            // Deliberately inside the `accept` branch only. The
                            // reject arm below is the case where the HTTP tier
                            // produced substantial good markdown and the JS
                            // ladder got walled; carrying the verdict there would
                            // block a page we can serve, which is a recall
                            // regression and the exact failure this whole change
                            // exists to avoid.
                            fetch_result.block = js_fetch.block.take();
                            // Replace the original "Target returned 4xx" with the JS
                            // fetch's warning (which is None for a clean 2xx render),
                            // so a successful escalation doesn't leak the original
                            // soft-block status into the response top-level warning.
                            effective_warning = js_warning;
                            tracing::info!(
                                url = %req.url,
                                from_status = fetch_result.status_code,
                                to_status = js_status,
                                md_len = js_md_len,
                                quality_score_before = before_score,
                                quality_score_after = js_score,
                                "JS escalation recovered content"
                            );
                        } else if js_md_len >= retry_threshold && !http_was_thin {
                            tracing::info!(
                                url = %req.url,
                                before = before_score,
                                after = js_score,
                                "JS retry returned worse-quality markdown ({before_score} -> {js_score}), keeping HTTP",
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(url = %req.url, "JS escalation after empty markdown failed: {e}");
                    // Surface the failure on the response too, not just the log:
                    // otherwise a renderer that's configured but unreachable (e.g.
                    // connection refused) silently returns success:true with thin
                    // HTTP-only markdown and no signal to the caller.
                    let js_fail_warning = format!("JS escalation failed: {e}");
                    effective_warning = Some(match effective_warning {
                        Some(w) => format!("{w}; {js_fail_warning}"),
                        None => js_fail_warning,
                    });
                }
            }
        }
        data
    };
    // ROOT-CAUSE: classify the anti-bot outcome ONCE here, at the shared choke,
    // and stamp a typed verdict onto ScrapeData so v1/v2/crawl/batch inherit one
    // decision. Runs before the summary-mode markdown strip below (~L620) so the
    // recovered markdown is still populated for the anti-over-trigger guard.
    //
    // Two possible sources, one verdict. `classify_block` reads the body and can
    // NAME the vendor (cloudflare, datadome, ...), which the SaaS routing
    // registry learns cloak-needing hosts from, so a named verdict always wins.
    // Its `structural_failure` verdict does not: that means "we fetched a page
    // and there was nothing usable in it", a weaker claim than the renderer's
    // own "this is a wall no tier could clear", and it would route the page to
    // the `http_error` branch of `routes/scrape.rs` instead of `anti_bot`.
    //
    // `fetch_result.block` is set only by the fallback arms of
    // `crw_renderer::Renderer::fetch`, for a body that is itself an anti-bot
    // wall. It used to be raised as an error that the server mapped to 502.
    let classified = classify_block(
        fetch_result.status_code,
        fetch_result.content_type.as_deref(),
        &fetch_result.html,
        data.markdown.as_deref(),
        fetch_result.screenshot.is_some(),
        extraction_cfg.http_retry_threshold_bytes,
        &fetch_result.url,
        fetch_result.final_url.as_deref(),
    );
    data.block = match (classified, fetch_result.block.clone()) {
        (Some(c), Some(carried)) if c.vendor == STRUCTURAL_FAILURE_VENDOR => Some(carried),
        (Some(c), _) => Some(c),
        (None, carried) => carried,
    };
    // Surface redirect mismatch as warning. Helps detect cases like
    // northernair.ca/history.htm silently 302'ing to the homepage — extraction
    // looks "successful" but the user got the wrong page.
    if let Some(final_url) = fetch_result.final_url.as_deref()
        && redirect_is_material(&fetch_result.url, final_url)
    {
        let warning = format!("redirected_to: {final_url}");
        if !data.warnings.iter().any(|w| w == &warning) {
            data.warnings.push(warning);
        }
    }

    // Merge target warning with any extraction warning (e.g. orphan chunk params).
    data.warning = match (effective_warning, data.warning) {
        (Some(w1), Some(w2)) => Some(format!("{w1}; {w2}")),
        (Some(w), None) | (None, Some(w)) => Some(w),
        (None, None) => None,
    };

    // ---- page cache: commit ----
    //
    // Here rather than at the fetch site, because only now is it known whether
    // the page is worth replaying: the JS escalation has run and the anti-bot
    // verdict is stamped. Here rather than at the end, because the LLM legs
    // below can fail and the caller who retries ten seconds later should not
    // pay for the fetch again.
    let escalated = escalated_renderer.is_some();
    if let Some(key) = cache_key.as_ref() {
        // Assemble the entry rather than storing whichever struct is lying
        // around. An accepted escalation lends only its body to `fetch_result`,
        // so the merged view is what a fresh request returns; the renderer
        // identity is corrected to the tier that really produced the body,
        // which is both true and what stops a replay from escalating again.
        //
        // This also runs when the base fetch was a HIT: an escalation on top of
        // a stale thin entry has just produced a better page, and writing it
        // back is the only thing that can unstick that entry before its TTL.
        let candidate = match escalated_renderer.take() {
            Some((rendered_with, render_decision)) => {
                let mut merged = fetch_result.clone();
                merged.rendered_with = rendered_with;
                merged.render_decision = render_decision;
                // An escalated page came through a browser, so it is never the
                // PDF branch and never carries raw bytes.
                merged.raw_bytes = None;
                Some(merged)
            }
            None => cache_candidate.take(),
        };
        let worth_replaying = candidate.as_ref().is_some_and(|candidate| {
            (200..300).contains(&candidate.status_code)
            // A partial DOM snapshotted on budget expiry is a 200 with a body.
            && !candidate.truncated
            && !candidate.deadline_exceeded
            // A challenge page is also a 200 with a body. Caching one would
            // serve the wall to everyone and fight the clearance ladder.
            && data.block.is_none()
            && data.http_error().is_none()
            // Gated on the markdown PRODUCED, not the format requested: an
            // extract request asks only for `json` yet computes markdown
            // internally, and that is the path this feature exists for.
            && data.markdown.as_deref().is_some_and(|md| !md.trim().is_empty())
            // A thin body is treated as an unrendered shell whatever the
            // request asked for: pinning one would serve it to every later
            // reader for the whole window.
            && fetch_is_not_a_shell
        });
        // Firecrawl's write switch. Reads stay allowed: the field is about what
        // we keep, not about what we may reuse.
        if let (true, Some(candidate)) = (
            worth_replaying && req.store_in_cache != Some(false),
            candidate,
        ) {
            // The WRITER's bucket, measured now rather than before the fetch. A
            // request can start with a minute and still reach the escalation
            // decision with nothing left, and it is that remainder, not the
            // starting budget, that decided which tier produced this page.
            let writer_bucket = page_cache::budget_bucket(deadline.remaining().as_millis() as u64);
            page_cache::store(key.clone(), candidate, writer_bucket, fetched_at).await;
        }
    }
    // A hit that then escalated returned a body fetched live milliseconds ago,
    // so it is not a cached answer however it started.
    data.cached = from_cache && !escalated;

    // Phase 4: LLM structured extraction
    // Merge Firecrawl-compatible extract.schema into json_schema if not already set.
    let effective_schema = req
        .json_schema
        .as_ref()
        .or_else(|| req.extract.as_ref().and_then(|e| e.schema.as_ref()));
    // Optional natural-language extraction instruction (`extract.prompt`). Works
    // alone — the LLM infers the output shape — or alongside a schema to steer
    // which fields get filled.
    let effective_prompt = req
        .extract
        .as_ref()
        .and_then(|e| e.prompt.as_deref())
        .filter(|p| !p.trim().is_empty());

    // Build BYOK LlmConfig once; reused by structured JSON + summary paths.
    let byok_config = build_byok_llm_config(req, llm_config);
    let effective_llm = byok_config.as_ref().or(llm_config);

    // `basis` is defined per schema leaf and rides the structured-extraction
    // call. Asking for it anywhere else is a caller mistake, not a no-op: fail
    // loudly rather than return a response with a silently absent `basis`.
    if req.basis && !formats_include_json(&req.formats) {
        return Err(crw_core::error::CrwError::InvalidRequest(
            "'basis' (per-field evidence) requires formats to include 'json'.".into(),
        ));
    }

    // Never send a wall or an origin error page to the model. The verdict is
    // already stamped above and every surface turns it into a failure, so any LLM
    // work below is billed to us by the provider and to nobody by us — an extract
    // job over a walled domain used to make one real call per URL and report zero
    // tokens.
    let unusable = data.block.is_some() || data.http_error().is_some();
    if formats_include_json(&req.formats) && !unusable {
        // A schema OR a prompt is enough to run structured extraction.
        if effective_schema.is_none() && effective_prompt.is_none() {
            return Err(crw_core::error::CrwError::InvalidRequest(
                "Structured extraction (formats: json/extract) requires a 'jsonSchema' field or an extraction 'prompt'.".into()
            ));
        }
        let Some(llm) = effective_llm else {
            return Err(crw_core::error::CrwError::ExtractionError(
                "JSON extraction requested but no LLM configured. Either set [extraction.llm] in server config, or pass 'llmApiKey' in the request body.".into()
            ));
        };
        let md = data.markdown.as_deref().unwrap_or("");
        // Evidence is emitted per top-level scalar property of the caller's
        // schema, so a prompt-only extraction (no schema) has nothing to attach
        // it to. Reject rather than degrade to an empty `basis`.
        let extraction = if req.basis {
            let Some(schema) = effective_schema else {
                return Err(crw_core::error::CrwError::InvalidRequest(
                    "'basis' (per-field evidence) requires a 'jsonSchema'; it is emitted per schema property, so a prompt-only extraction has no fields to attribute.".into()
                ));
            };
            // The document the server actually read, after redirects. This is
            // the url the citations carry and the one the model is shown; its
            // own echoed string is only ever compared, never surfaced.
            let doc_url = fetch_result
                .final_url
                .as_deref()
                .unwrap_or(&fetch_result.url)
                .to_string();
            crw_extract::structured::extract_structured_with_basis(
                md,
                schema,
                effective_prompt,
                llm,
                None,
                &doc_url,
            )
            .await
        } else {
            crw_extract::structured::extract_structured_with_usage(
                md,
                effective_schema,
                effective_prompt,
                llm,
                None,
            )
            .await
        };
        match extraction {
            Ok(result) => {
                data.json = Some(result.value);
                if req.basis {
                    data.basis = Some(result.basis);
                    data.basis_warnings = result.basis_warnings;
                    data.llm_input_hash = result.llm_input_hash;
                }
                // ACCUMULATE, never overwrite. A request can ask for json AND
                // summary, and change tracking can add a judge on top: three
                // separate model calls. Keeping only the first one made the others
                // invisible to the SaaS, which prices off this field — we paid the
                // provider for calls we never billed.
                crw_core::types::LlmUsage::accumulate(&mut data.llm_usage, result.usage);
            }
            Err(e) => {
                tracing::error!("Structured extraction failed: {e}");
                return Err(e);
            }
        }
    }

    // Same reason as the json branch above: neither is summarizable content.
    if formats_include_summary(&req.formats) && !unusable {
        let Some(llm) = effective_llm else {
            return Err(crw_core::error::CrwError::ExtractionError(
                "Summary format requires an LLM config. Either set [extraction.llm] in server config, or pass 'llmApiKey' in the request body.".into()
            ));
        };
        // Markdown is computed internally even if not in `formats`; if the
        // caller asked only for `summary`, the markdown is the input to the
        // LLM but is not surfaced in the response (see strip below).
        let md_owned = data.markdown.clone().unwrap_or_default();
        match crw_extract::summary::summarize(
            &md_owned,
            llm,
            req.summary_prompt.as_deref(),
            req.max_content_chars,
        )
        .await
        {
            Ok(result) => {
                data.summary = Some(result.content);
                crw_core::types::LlmUsage::accumulate(&mut data.llm_usage, result.usage);
                if let Some(w) = result.warning {
                    data.warnings.push(w);
                }
            }
            Err(e) => {
                tracing::warn!("Summary generation failed: {e}");
                data.warnings.push(format!("summary failed: {e}"));
            }
        }
        // If the caller didn't explicitly ask for markdown, strip the
        // internally-computed markdown from the response.
        if !req.formats.contains(&OutputFormat::Markdown) {
            data.markdown = None;
        }
    }

    // Drain the per-request debug sink into ScrapeData. The sink is the
    // last shared owner at this point — extract() returned, dropping its
    // clone — so try_unwrap should succeed; if a stray clone is alive we
    // fall back to a clone of the inner Vec.
    if let Some(sink) = debug_sink {
        // Each extract() call dropped its clone of the Arc, so by this
        // point we hold the only reference and can unwrap cheaply.
        let extraction = match Arc::try_unwrap(sink) {
            Ok(mu) => mu.into_inner().unwrap_or_default().into_extraction(),
            Err(_) => crw_core::types::DebugExtraction::default(),
        };
        data.debug_extraction = Some(extraction);
    }

    // Surface the fetched content type so change-tracking (here and on the
    // crawl path) can hash binary/non-text content rather than diff it.
    data.content_type = fetch_result.content_type.clone();

    // A partial-DOM snapshot (navigation budget elapsed) extracts to usable but
    // incomplete content. Carry the flag out so callers that bound the scrape
    // budget — `/v1/search` enrichment above all — can observe truncation
    // instead of mistaking it for a thin page.
    data.truncated = fetch_result.truncated;

    // ...but a truncated render that extracted to NOTHING is not incomplete
    // content, it is no content. The navigation budget expired mid-load and the
    // partial DOM held nothing extractable, so a 200 here bills the caller for a
    // blank document and reads as "this page is empty" when the truth is "we ran
    // out of time" — two states the caller cannot tell apart, and only one of
    // which they can fix (by raising `timeout`). Measured on prod 2026-07-24:
    // 4 of 26 truncated scrapes shipped 0 bytes as a billed success.
    //
    // Scoped deliberately: only when markdown was ASKED FOR (a `rawHtml`/`links`
    // caller can still use a partial DOM) and only when it is entirely empty.
    // A thin-but-present body stays a success — that is a judgment call about
    // quality, not a missing answer, and taking it would risk recall.
    // No renderer can fix a dead origin, so this is a failure rather than a page.
    // `TargetUnreachable` maps to 422, which the SaaS already refunds — the same
    // request currently costs a credit and reports success.
    if is_cdn_origin_error(fetch_result.status_code) {
        tracing::warn!(
            url = %req.url,
            status = fetch_result.status_code,
            elapsed_ms = fetch_result.elapsed_ms,
            "CDN could not reach the origin; failing instead of billing its error page"
        );
        return Err(crw_core::error::CrwError::TargetUnreachable(format!(
            "the site's own server did not respond (HTTP {} from its CDN)",
            fetch_result.status_code
        )));
    }

    if is_empty_truncated_render(data.truncated, &req.formats, data.markdown.as_deref()) {
        tracing::warn!(
            url = %req.url,
            elapsed_ms = fetch_result.elapsed_ms,
            "render budget expired with no extractable content; failing instead of billing an empty page"
        );
        return Err(crw_core::error::CrwError::Timeout(fetch_result.elapsed_ms));
    }

    // Wrap the raw base64 screenshot in a `data:image/png;base64,` URL exactly
    // once, here, so both v1 and v2 responses are identical (D8). FetchResult
    // keeps the raw b64.
    data.screenshot = fetch_result
        .screenshot
        .as_ref()
        .map(|b| format!("data:image/png;base64,{b}"));

    // ── Change tracking (monitor) ──────────────────────────────────────────
    // Activated by the `"changeTracking"` format string; options ride on the
    // sibling `change_tracking` field. The diff is computed against the
    // caller-supplied `previous` snapshot — opencore stores nothing. The LLM
    // judge is opt-in and runs below: it requires `goal` + `judgeEnabled: true`.
    if req.formats.contains(&OutputFormat::ChangeTracking) {
        let Some(ct_opts) = &req.change_tracking else {
            return Err(crw_core::error::CrwError::InvalidRequest(
                "formats includes 'changeTracking' but no 'changeTracking' options were provided."
                    .into(),
            ));
        };
        let wants_json = ct_opts.modes.contains(&ChangeTrackingMode::Json);

        // For json / mixed mode, extract the tracked fields using the
        // changeTracking schema (independent of the top-level `json` format).
        let mut current_json: Option<serde_json::Value> = None;
        if wants_json {
            match (ct_opts.schema.as_ref(), effective_llm) {
                (Some(schema), Some(llm)) => {
                    let md = data.markdown.as_deref().unwrap_or("");
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
                            current_json = Some(result.value);
                            // ACCUMULATE, never overwrite-if-empty. This is a
                            // second structured call, independent of the
                            // top-level `json` format, so a request asking for
                            // both ran two models and dropped the second one's
                            // tokens on the floor. The SaaS bills off these
                            // counts, so the difference was unbilled work.
                            crw_core::types::LlmUsage::accumulate(
                                &mut data.llm_usage,
                                result.usage,
                            );
                        }
                        Err(e) => return Err(e),
                    }
                }
                (None, _) => {
                    return Err(crw_core::error::CrwError::InvalidRequest(
                        "changeTracking json mode requires a 'schema' describing the fields to track.".into(),
                    ));
                }
                (Some(_), None) => {
                    return Err(crw_core::error::CrwError::ExtractionError(
                        "changeTracking json mode requires an LLM config. Set [extraction.llm] or pass 'llmApiKey'.".into(),
                    ));
                }
            }
        }

        let md = data.markdown.as_deref().unwrap_or("");
        let started = std::time::Instant::now();
        let mut result = crw_diff::compute_change_tracking(
            ct_opts,
            md,
            current_json.as_ref(),
            data.content_type.as_deref(),
        );

        // Observability: diff duration + retained snapshot size, by mode.
        let mode = change_tracking_mode_label(ct_opts, data.content_type.as_deref());
        let m = crw_core::metrics::metrics();
        m.change_tracking_duration_seconds
            .with_label_values(&[mode])
            .observe(started.elapsed().as_secs_f64());
        if let Some(snap) = &result.snapshot {
            let bytes = snap.markdown.as_ref().map(|s| s.len()).unwrap_or(0)
                + snap.json.as_ref().map(|j| j.to_string().len()).unwrap_or(0);
            m.change_tracking_snapshot_bytes
                .with_label_values(&[mode])
                .observe(bytes as f64);
        }

        // ── Meaningful-change judge (opt-in) ──────────────────────────────
        // Runs only on a changed page that produced a diff (excludes binary
        // and first-observation pages), when a goal is set and judging is
        // enabled. Judge failure never fails the scrape — it degrades to no
        // judgment plus a warning. opencore does no credit math; the SaaS
        // bills a flat +1 credit per judged changed page.
        if result.status == crw_core::types::ChangeStatus::Changed
            && result.diff.is_some()
            && req.judge_enabled == Some(true)
            && let Some(goal) = req.goal.as_deref().map(str::trim).filter(|g| !g.is_empty())
        {
            let has_json = ct_opts.modes.contains(&ChangeTrackingMode::Json);
            let diff_text = result.diff.as_ref().and_then(|d| d.text.as_deref());
            // Only the per-field json map (json/mixed) is a useful judge input;
            // the gitDiff-only AST under diff.json is not field-level changes.
            let json_diff = if has_json {
                result.diff.as_ref().and_then(|d| d.json.as_ref())
            } else {
                None
            };
            match effective_llm {
                Some(llm) => {
                    match crw_extract::judge::judge_change(goal, diff_text, json_diff, llm, None)
                        .await
                    {
                        Ok(judgment) => {
                            m.judge_calls_total.with_label_values(&["ok"]).inc();
                            if let Some(u) = &judgment.llm_usage {
                                m.judge_tokens_total
                                    .with_label_values(&["input"])
                                    .inc_by(u.input_tokens as u64);
                                m.judge_tokens_total
                                    .with_label_values(&["output"])
                                    .inc_by(u.output_tokens as u64);
                            }
                            // The judge is a real model call. Its tokens only ever
                            // reached Prometheus, so the SaaS — which prices off
                            // data.llm_usage — never saw them: a judged page either
                            // billed nothing for the judge, or (with no other leg
                            // to report usage) fell back to charging the whole
                            // worst-case reserve. Fold it in like every other leg.
                            crw_core::types::LlmUsage::accumulate(
                                &mut data.llm_usage,
                                judgment.llm_usage.clone(),
                            );
                            result.judgment = Some(judgment);
                        }
                        Err(e) => {
                            m.judge_calls_total.with_label_values(&["error"]).inc();
                            tracing::warn!("change-tracking judge failed: {e}");
                            data.warnings.push(format!("judge failed: {e}"));
                        }
                    }
                }
                None => {
                    m.judge_calls_total.with_label_values(&["skipped"]).inc();
                    data.warnings
                        .push("judge skipped: no LLM configured".into());
                }
            }
        }

        data.change_tracking = Some(result);
    }

    Ok(data)
}

/// Metric label for a change-tracking computation: `binary` when the content
/// type is non-text, else `mixed` / `json` / `gitDiff` per the active modes.
fn change_tracking_mode_label(
    opts: &crw_core::types::ChangeTrackingOptions,
    content_type: Option<&str>,
) -> &'static str {
    let is_text = content_type.is_none_or(|ct| {
        let ct = ct.to_ascii_lowercase();
        ct.starts_with("text/")
            || ct.contains("json")
            || ct.contains("xml")
            || ct.contains("html")
            || ct.contains("markdown")
            || ct.contains("javascript")
            || ct.contains("csv")
            || ct.contains("yaml")
    });
    if !is_text {
        return "binary";
    }
    let has_git = opts.modes.is_empty() || opts.modes.contains(&ChangeTrackingMode::GitDiff);
    let has_json = opts.modes.contains(&ChangeTrackingMode::Json);
    match (has_git, has_json) {
        (true, true) => "mixed",
        (false, true) => "json",
        _ => "gitDiff",
    }
}

/// Decide whether `final_url` represents a material redirect from `requested`.
/// Returns true when the host changed, or when the requested path was a
/// non-root resource (e.g. `/history.htm`) but the final URL collapsed to the
/// site root (`/` or empty). Pure same-origin path tweaks (trailing slash,
/// query string changes) are ignored.
fn redirect_is_material(requested: &str, final_url: &str) -> bool {
    let Ok(req) = url::Url::parse(requested) else {
        return false;
    };
    let Ok(fin) = url::Url::parse(final_url) else {
        return false;
    };
    if req.host_str() != fin.host_str() {
        return true;
    }
    let req_path = req.path().trim_end_matches('/');
    let fin_path = fin.path().trim_end_matches('/');
    !req_path.is_empty() && fin_path.is_empty()
}

/// The target sits behind a CDN that could not get a usable response out of the
/// origin, so the body is the CDN's own error page and not the page that was
/// asked for.
///
/// Cloudflare's 520-527 are not IANA-registered; Cloudflare generates them and
/// an origin does not emit them, which is what makes the status alone a safe
/// signal (response headers are not available this far up — `PageMetadata`
/// carries only `status_code`). The renderer never synthesises a 52x either, so
/// one arriving here always came off the wire.
///
/// Stops at 527 rather than matching the renderer's adjacent `520..=530`
/// (`lib.rs` `hard_block`) on purpose. That range is the *egress-recoverable*
/// set, and the two are complements: 520-527 are all origin-side (down,
/// unreachable, timed out, bad TLS) and no exit IP fixes them, whereas 530 rides
/// a Cloudflare `1XXX` code that includes firewall denials like 1020 — a real
/// block, and one a different egress genuinely can clear. Treating 530 as "the
/// origin is broken" would report our own blocked IP as the customer's site
/// being down.
///
/// This fires on the FINAL stitched result, so a 52x that some later tier
/// recovered into a real page never reaches it — no recall is at stake.
///
/// This is a status check and NOT a body check on purpose. The scrape routes
/// already refuse a `>= 400` whose body is under 200 bytes, and that guard is
/// structurally unable to catch this: Cloudflare's error page is a branded HTML
/// document that renders to ~1250 bytes of markdown, six times the threshold. So
/// `sacg.me` behind a dead origin was returned as `success: true` with "The
/// initial connection between Cloudflare's network and the origin web server
/// timed out" as its markdown, billed, and counted as a completed crawl page —
/// for one customer, on the same source, since June.
pub(crate) fn is_cdn_origin_error(status_code: u16) -> bool {
    (520..=527).contains(&status_code)
}

/// A truncated render that extracted to nothing: the render budget expired
/// mid-load and the partial DOM held no markdown. See the call site for why
/// that is a failure rather than an empty page.
fn is_empty_truncated_render(
    truncated: bool,
    formats: &[OutputFormat],
    markdown: Option<&str>,
) -> bool {
    truncated
        && formats.contains(&OutputFormat::Markdown)
        && markdown.map(|m| m.trim().is_empty()).unwrap_or(true)
}

pub(crate) fn derive_target_warning(fetch_result: &FetchResult) -> Option<String> {
    // Anti-bot detection wins over any other warning. The renderer chain
    // annotates thin results with "X returned a loading placeholder", but the
    // underlying HTML may be a CAPTCHA shell — surfacing the placeholder
    // misattributes the failure to our renderer instead of the site block.
    if let Some(block) = detect_block_interstitial(&fetch_result.html) {
        // Exception: a `js_escalation_failed:` prefix explains WHY the caller is
        // looking at an HTTP shell at all, and a block page is the single most
        // likely body to be holding one. Dropping it here would leave a
        // `renderJs:true` caller with "Blocked by …" and no way to tell the
        // browser tier ran and lost — which is exactly what the docs tell them
        // to look for. Keep both, block first.
        return Some(match fetch_result.warning.as_deref() {
            Some(w) if w.starts_with(crw_renderer::JS_ESCALATION_FAILED) => {
                format!("{block}; {w}")
            }
            _ => block,
        });
    }

    if fetch_result.warning.is_some() {
        return fetch_result.warning.clone();
    }

    if fetch_result.status_code >= 400 {
        return Some(format!(
            "Target returned {} {}",
            fetch_result.status_code,
            canonical_status_text(fetch_result.status_code)
        ));
    }

    None
}

fn canonical_status_text(status_code: u16) -> &'static str {
    match status_code {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        410 => "Gone",
        429 => "Too Many Requests",
        451 => "Unavailable For Legal Reasons",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}

fn detect_block_interstitial(html: &str) -> Option<String> {
    // If page has substantial content (>50KB), it's not a block/interstitial page
    if html.len() > 50_000 {
        return None;
    }

    const SCAN_LIMIT: usize = 128 * 1024;
    let end = if html.len() <= SCAN_LIMIT {
        html.len()
    } else {
        let mut e = SCAN_LIMIT;
        while e > 0 && !html.is_char_boundary(e) {
            e -= 1;
        }
        e
    };
    let lower = html[..end].to_lowercase();
    // Keep markers SPECIFIC to interstitial pages — bare "captcha"/"access
    // denied" false-positive on legit content (e.g. an HN headline mentioning
    // "reCAPTCHA" matches "captcha" anywhere in the document).
    let markers = [
        "just a moment",
        "attention required",
        "cf-browser-verification",
        "cf-challenge",
        // DataDome — captcha-delivery host + "datadome" string only appear on
        // actively-challenged pages.
        "captcha-delivery.com",
        "datadome captcha",
        // PerimeterX / HUMAN — _px3 cookie + px-captcha widget
        "px-captcha",
        "_px3=",
        // Akamai Bot Manager
        "_abck=",
        "ak-challenge",
    ];

    if markers.iter().any(|marker| lower.contains(marker)) {
        Some("Blocked by anti-bot protection".to_string())
    } else {
        None
    }
}

/// The one signal this arm trusts: **the page names its own host and declares itself a
/// placeholder**. Capture group 1 is the domain token, and the caller must match it
/// against the host actually scraped.
///
/// That comparison is the entire guarantee, and it is why every earlier shape was
/// removed. Unanchored template literals ("the nginx web server is successfully
/// installed", "apache2 ubuntu default page", "this domain has been registered") fire on
/// the most-scraped technical content there is: every nginx install tutorial quotes the
/// default vhost verbatim to confirm the install worked, every "Apache2 Ubuntu Default
/// Page still showing" StackOverflow thread repeats it in the title, and UDRP decisions
/// say "this domain has been registered and is being used in bad faith". A token match
/// alone is just as bad: "Insurance.com is for sale" is a real domain-industry headline,
/// and `[a-z]{2,12}` happily accepts `checkout.html`, `main.py` or `config.yaml`, so a
/// task board reading "- checkout.html ready for development" would fail too.
///
/// Requiring the token to BE the scraped host kills all of it at once: an article talks
/// about someone else's domain, a parking page talks about the one you asked for.
///
/// Measured over 20,012 real prod scrape successes (2026-08-09..12): 294 matches, and in
/// every single one the token equalled the scraped host — the host check costs nothing
/// and removes the whole false-positive surface. It also correctly rejected two pages
/// that named a DIFFERENT domain. Dropping the four unanchored literals costs 15
/// records (0.075%), which against rejecting a real tutorial is not a close trade.
static DOMAIN_PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    // The left guard is a CONSUMED character class, not `\b` and not a lookbehind:
    // the `regex` crate has no lookaround, and `\b` matches straight after a dot, so
    // `\b(...)` on "shop.example.com is for sale" captured only "example.com" — which
    // would then equal the scraped host and fail a real page. It also truncated
    // "example.co.uk" to "co.uk" (missing genuine parked pages) and could not match a
    // single-letter label like "x.com" at all. Capturing whole dotted labels fixes all
    // three at once and changed nothing on the 20,012-doc corpus (294 before, 294 after).
    Regex::new(
        r"(?:^|[^a-z0-9.\-])([a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)*\.[a-z]{2,24})\s{0,24}(?:[-–—]\s{0,24}|::\s{0,24}this domain\s{0,24})?(?:is for sale|ready for development|is parked free)\b",
    )
    .expect("static parked-domain pattern")
});

/// Upper bound on a placeholder page's own body. See `looks_like_parked_domain`.
const PARKED_MAX_BODY_BYTES: usize = 16 * 1024;

/// Host of `url`, lowercased with a leading `www.` stripped, for comparing against a
/// domain token rendered in the page body.
fn normalized_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    Some(host.strip_prefix("www.").unwrap_or(&host).to_string())
}

/// Registrar parking, domain-marketplace and holding pages: the page is served at the
/// domain and its whole content is "this domain is available", so nothing the caller
/// asked for was delivered.
///
/// Matches MARKDOWN, not HTML, for two load-bearing reasons: `classify_block` already
/// receives the markdown so no extraction is needed, and `detector.rs`'s HTML text
/// extractor emits no separator at tag boundaries, so `<h1>host</h1><h2>is for
/// sale</h2>` would collapse to `hostis for sale` and silently break the pattern.
///
/// Both the requested and the post-redirect host count, so a domain that 302s to its
/// registrar still matches on the name the caller asked for.
///
/// ponytail: "under construction" and "coming soon" are deliberately NOT here. Those are
/// pages the origin published on purpose — `eliteteam.ch` renders its own WordPress
/// under-construction plugin — so they are truthful scrapes and stay billable. That is
/// 68% of the flagged population, so this does not take the class to zero by design.
fn looks_like_parked_domain(markdown: &str, requested_url: &str, final_url: Option<&str>) -> bool {
    /// Byte-bounded prefix that never splits a UTF-8 char (`&s[..n]` panics off a
    /// char boundary, and scraped markdown is routinely non-ASCII).
    fn head(s: &str, max: usize) -> &str {
        if s.len() <= max {
            return s;
        }
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }

    // A placeholder page IS the whole document. Bounding the body stops a LIVE site
    // that merely carries a "<our domain> is for sale" banner from having every page of
    // a crawl failed and its body destroyed (`crawl.rs` clears the body on a block).
    // The largest real placeholder measured over the 20,012-doc corpus is 9,459 bytes,
    // so 16 KB costs nothing against all 294 matches and closes the banner class.
    if markdown.len() > PARKED_MAX_BODY_BYTES {
        return false;
    }

    let hosts: Vec<String> = [Some(requested_url), final_url]
        .into_iter()
        .flatten()
        .filter_map(normalized_host)
        .collect();
    if hosts.is_empty() {
        return false;
    }

    // ponytail: bound the scan rather than the verdict. A parking notice is always at
    // the top (largest observed placeholder body is ~13 KB); this only stops a
    // multi-hundred-KB article from being lowercased on the scrape hot path.
    let low = head(markdown, 40_960).to_lowercase();

    // `captures_iter`, not `captures`: a page may name another domain before its own.
    DOMAIN_PLACEHOLDER.captures_iter(&low).any(|c| {
        c.get(1).is_some_and(|m| {
            let token = m.as_str();
            let token = token.strip_prefix("www.").unwrap_or(token);
            hosts.iter().any(|h| h == token)
        })
    })
}

/// Classify whether a fetched page is an anti-bot block/challenge shell. Runs
/// ONCE at the scrape choke so every consumer (v1/v2/crawl/batch) inherits one
/// verdict. Returns `None` for real content; `Some(BlockOutcome)` for a block.
///
/// Guard ordering matters:
/// 1. PDF branch has empty `html` → would false-positive as StructuralFailure.
/// 2. Substantial markdown is authoritative even under a soft-block status
///    (anti-over-trigger): an accepted JS escalation guarantees markdown >=
///    `threshold`, so a stale block-shell `html` cannot mislabel it.
/// 3. Reuse the trusted `crw_extract::antibot::classify` detector.
/// 4. A captured screenshot may suppress ONLY the generic near-empty
///    StructuralFailure heuristic — never a positive vendor block.
#[allow(clippy::too_many_arguments)] // independent page signals; a struct adds noise
pub(crate) fn classify_block(
    status: u16,
    content_type: Option<&str>,
    html: &str,
    markdown: Option<&str>,
    screenshot_present: bool,
    threshold: usize,
    requested_url: &str,
    final_url: Option<&str>,
) -> Option<BlockOutcome> {
    if content_type.map(|c| c.contains("pdf")).unwrap_or(false) {
        return None;
    }
    // Our own egress failing proxy auth is not the target blocking us. Narrow to
    // exactly that: a 407 with no body. Everything else with an empty body stays
    // classifiable, because a near-empty 403/503 is the canonical CloudFront and
    // Akamai deny signature and a near-empty 429 is the canonical rate limit —
    // those feed `blockVendor`, which feeds the routing registry.
    if status == 407 && html.is_empty() {
        return None;
    }
    // Modern Cloudflare Turnstile / managed challenge is frequently served with
    // HTTP 200 and a LARGE (~118KB) body (e.g. barcodelookup.com). Both
    // antibot::classify (size-capped, challenge-form-era patterns) and
    // detector::looks_like_cloudflare_challenge (bails at 80KB) miss it, so the
    // interstitial leaks through as success:true. These markers appear ONLY on
    // the interstitial and the challenge script is injected near the END of the
    // body (measured at byte ~114k of a 118k page — NOT the <head>), so scan the
    // FULL html, not a prefix. They are fixed-case CF tokens, so a case-sensitive
    // substring search avoids allocating a lowercased copy on every scrape.
    // Runs before the markdown-substantial guard: a challenge is a block even if
    // it yields boilerplate text.
    //
    // `/cdn-cgi/challenge-platform/` is deliberately NOT in this list: Cloudflare
    // re-injects that telemetry loader into the CLEARED page too (measured at byte
    // ~782k of a real post-solve 783k Glassdoor page that carries NO _cf_chl_opt),
    // so a full-html scan for it false-positives every managed site the cloak arm
    // successfully solves — emptying real content back to a challenge block. The
    // remaining markers are interstitial-only: window._cf_chl_opt (the challenge
    // config object) and the older cf-*/managed-token strings are absent once the
    // page clears.
    const CF_STRONG_MARKERS: [&str; 4] = [
        "_cf_chl_opt",
        "cf-challenge-running",
        "cf-browser-verification",
        "__cf_chl_managed_tk__",
    ];
    if CF_STRONG_MARKERS.iter().any(|m| html.contains(m)) {
        return Some(BlockOutcome {
            vendor: "cloudflare".to_string(),
            reason: "cloudflare challenge interstitial".to_string(),
        });
    }
    // Wikimedia serves its datacenter-IP ban as an HTTP-200 static error shell
    // whose error prose extracts to ~110 bytes of markdown — over the guard
    // below — so the antibot classifier (which runs after the guard) never sees
    // it and the block leaks as success:true. This footer sentence is unique to
    // the Wikimedia error page, so treat it as a strong marker and classify
    // ahead of the markdown-substantial guard, mirroring the CF markers above.
    // Case-sensitive like CF_STRONG_MARKERS: the sentence is a fixed literal in
    // Wikimedia's Varnish/ops error template (below the app layer, language- and
    // wiki-independent), so its casing does not vary across requests.
    // ponytail: one canonical sentence keeps false positives ~nil; a real
    // article never carries it.
    if html.contains("report this error to the Wikimedia System Administrators") {
        return Some(BlockOutcome {
            vendor: "generic_block".to_string(),
            reason: "wikimedia datacenter-ip block".to_string(),
        });
    }
    // Reddit's own block page ("You've been blocked by network security...") is
    // itself ~115 bytes of prose — over the markdown-substantial guard below —
    // so antibot::classify (which already recognizes this exact phrase via its
    // NetworkSecurity pattern) never runs. Same trap as the Wikimedia/CF cases
    // above. Require both halves of the sentence (not just the first clause) so
    // an article merely quoting "blocked by network security" in isolation
    // cannot trip this ahead of the guard.
    if html.contains("blocked by network security")
        && html.contains("log in to your Reddit account")
    {
        return Some(BlockOutcome {
            vendor: "network_security".to_string(),
            reason: "blocked by network security".to_string(),
        });
    }
    // Cloudflare's hard block page (an outright deny, distinct from the
    // Turnstile/managed-challenge interstitial matched by CF_STRONG_MARKERS
    // above) also beats the guard. `<span class="cf-error-code">` is the exact
    // structural marker antibot::classify already trusts for this vendor
    // (`crw-extract/src/antibot.rs`); require it together with the page's own
    // block heading so a page that merely mentions the token in prose, a code
    // sample, or documentation cannot trip this ahead of the guard.
    if html.contains(r#"<span class="cf-error-code">"#) && html.contains("you have been blocked") {
        return Some(BlockOutcome {
            vendor: "cloudflare".to_string(),
            reason: "cloudflare block page (cf-error-code)".to_string(),
        });
    }
    // Cloudflare's CURRENT block-page template carries no `cf-error-code` span
    // at all (a live capture: 0 occurrences), so the arm above never sees it
    // and the page fell through to `generic_block`. Its structural markers are
    // the `cf-error-details` container and the `block_headline` translation
    // key on the heading; require both so a page that merely mentions one
    // token in prose cannot trip this. The vendor stamp is what the routing
    // learner counts; routing itself never reads `vendor`, so this arm cannot
    // move a request between tiers.
    if html.contains("cf-error-details") && html.contains(r#"data-translate="block_headline""#) {
        return Some(BlockOutcome {
            vendor: "cloudflare".to_string(),
            reason: "cloudflare block page (cf-error-details)".to_string(),
        });
    }
    // Vercel's bot-check interstitial beats the guard too — the real page (with
    // its "Website owner? Click here to fix" link) extracts to ~135 bytes, over
    // threshold, so antibot::classify's Vercel pattern (which requires this same
    // heading + verifying/failed phrase) never runs. Caught only by testing
    // against REAL production captures: the synthetic fixture used to validate
    // the antibot.rs pattern was artificially short and never hit this guard,
    // so this gap shipped once already — mirror the Reddit/CF strong-marker
    // pattern here too.
    if html.contains("Vercel Security Checkpoint")
        && (html.contains("verifying your browser")
            || html.contains("Failed to verify your browser"))
    {
        return Some(BlockOutcome {
            vendor: "vercel".to_string(),
            reason: "Vercel security checkpoint".to_string(),
        });
    }
    // Fifth instance of the same trap as the four strong-marker arms above, and the
    // one prod actually pays for. A registrar parking / marketplace / default-server
    // page is HTTP 200, so `ScrapeData::http_error()` clears it, and it renders 300 to
    // 9,000 chars of clean prose, so the `>= threshold` guard below clears it too and
    // `antibot::classify` is never even called. Measured over 20,007 prod scrape
    // successes (2026-08-09..12): 304 of them, every one billed as content.
    if let Some(md) = markdown
        && looks_like_parked_domain(md, requested_url, final_url)
    {
        return Some(BlockOutcome {
            vendor: crw_core::types::PARKED_DOMAIN_VENDOR.to_string(),
            reason: "parked domain / placeholder page".to_string(),
        });
    }
    // The substantial-content guard: real content overrules a soft-block status.
    //
    // `markdown: None` does NOT mean the page is empty. It means markdown was
    // never EXTRACTED, which is the normal case when the caller asked only for
    // `rawHtml`, `links` or a screenshot. Reading it as 0 made every such request
    // fall through to `antibot::classify`, whose catch-all answers GenericBlock
    // for ANY 4xx carrying an HTML body, so the same page was condemned purely by
    // which format was requested. Measured live against a pre-change engine on
    // `stackoverflow.com/questions/tagged/rust` (HTTP 403, serves the real page):
    //   formats:["markdown"] -> success:true, 65,209 chars
    //   formats:["rawHtml"]  -> success:false, anti_bot, empty body
    //
    // So when there is no markdown to weigh, weigh the page's own visible text,
    // the same measure the wall heuristics use. This only ever suppresses the
    // status-derived generic bucket: every strong-marker vendor arm above already
    // ran, and a wall the renderer ladder itself condemned arrives as a carried
    // verdict that wins the merge in `scrape_url_inner` regardless of this call.
    // Substantial MARKDOWN stays authoritative over every verdict below, vendor
    // arms included: an accepted JS escalation guarantees markdown >= threshold,
    // so a stale block-shell `html` cannot mislabel a page we really extracted.
    // Unchanged behaviour whenever markdown exists.
    if let Some(md) = markdown
        && md.trim().len() >= threshold
    {
        return None;
    }
    let r = crw_extract::antibot::classify(Some(status), html);
    if !r.signal.is_blocked() {
        return None;
    }
    // `markdown: None` means markdown was never EXTRACTED, the normal case when
    // the caller asked only for `rawHtml`, `links` or a screenshot, not that the
    // page is empty. Reading it as zero content made every such request fall
    // through to the status-derived bucket, which answers GenericBlock for ANY 4xx
    // carrying an HTML body. Measured live against a pre-change engine on
    // `stackoverflow.com/questions/tagged/rust` (HTTP 403, serves the real page):
    //   formats:["markdown"] -> success:true, 65,209 chars
    //   formats:["rawHtml"]  -> success:false, anti_bot, empty body
    //
    // So weigh the page's own visible text instead, but ONLY against the two
    // verdicts that are shape/status heuristics. Every NAMED vendor arm inside
    // `classify` (Akamai, PerimeterX, DataDome, Imperva, Sucuri, Kasada, Google
    // unusual-traffic, and the unconditional 429 / 521 arms) fires at any page
    // size by design, and a deny notice rendered inside a site's normal
    // header-nav-footer template clears 600 characters easily. Gating those on a
    // content bar would silently un-catch them for every non-markdown request.
    if markdown.is_none()
        && matches!(
            r.signal,
            crw_extract::antibot::AntibotSignal::GenericBlock
                | crw_extract::antibot::AntibotSignal::StructuralFailure
        )
        && crw_renderer::detector::visible_text_len(html)
            > crw_renderer::detector::WALL_VISIBLE_TEXT_MAX
    {
        return None;
    }
    if screenshot_present && r.signal == crw_extract::antibot::AntibotSignal::StructuralFailure {
        return None;
    }
    // A non-HTML body is CONTENT, so the HTML SHAPE heuristics do not apply to it:
    // the HTTP tier decodes every non-PDF response as HTML, so a 68-byte
    // requirements.txt has no `<body>` and came back `structural_failure` /
    // `anti_bot` — measured live on raw.githubusercontent.com. Only the structural
    // verdict is suppressed; the vendor arms above stay live, because a wall IS
    // sometimes served under a data content type (DataDome answers XHR-shaped
    // requests with an `application/json` captcha stub, and `escalate_for_quality`
    // below runs `classify` on JSON bodies for the same reason).
    if r.signal == crw_extract::antibot::AntibotSignal::StructuralFailure
        && !crw_renderer::is_html_like_content_type(content_type)
    {
        return None;
    }
    Some(BlockOutcome {
        vendor: r.signal.class_name().to_string(),
        reason: r.reason,
    })
}

/// Should a substantive-but-low-scoring body buy a browser render?
///
/// The content-type term matters because the HTTP tier decodes every non-PDF
/// body as HTML regardless of its declared type, so a healthy JSON API response
/// scores as low-quality (no sentences, few word-like tokens) and used to buy a
/// full render that was then discarded: prod measured 114 of 135 quality
/// escalations ending in "keeping HTTP", ~2.5-3s each. Unlike the byte-thin
/// trigger this one fires on a body we already hold in full, so for a
/// non-html-ish type there is nothing left for a browser (or a different
/// egress) to reveal. The byte-thin path stays content-type-agnostic on
/// purpose: a near-empty response may be a deny stub that a retry from another
/// fingerprint recovers.
///
/// Two things still earn a render under a data content type, so both get the
/// last word before we suppress. Both are short-circuited away on the html
/// path, which keeps its current cost exactly.
fn escalate_for_quality(
    md_is_byte_thin: bool,
    md_is_low_quality: bool,
    status: u16,
    html: &str,
    content_type: Option<&str>,
) -> bool {
    if md_is_byte_thin || !md_is_low_quality || html.len() <= 5000 {
        return false;
    }
    // Only JSON is gated, and only because it is the one type we measured: 114
    // of 135 quality escalations in 24h of production ended in "keeping HTTP",
    // all of them JSON API bodies. Every other content type keeps escalating
    // exactly as before, so no content sniffing has to be correct for recall to
    // hold. Widening this needs its own measurement.
    let is_json = content_type
        .and_then(|ct| ct.split(';').next())
        .map(|ct| ct.trim().eq_ignore_ascii_case("application/json"))
        .unwrap_or(false);
    if !is_json {
        return true;
    }
    // A vendor wall can be served under a data content type, and that one a
    // retry from another fingerprint can clear.
    crw_extract::antibot::classify(Some(status), html)
        .signal
        .is_blocked()
}

fn formats_include_json(formats: &[OutputFormat]) -> bool {
    formats.contains(&OutputFormat::Json)
}

fn formats_include_summary(formats: &[OutputFormat]) -> bool {
    formats.contains(&OutputFormat::Summary)
}

/// Build an `LlmConfig` from per-request BYOK fields, falling back to the
/// server-config values for non-credential fields (concurrency, header
/// guard) so a single request can't escape global limits.
fn build_byok_llm_config(req: &ScrapeRequest, server_cfg: Option<&LlmConfig>) -> Option<LlmConfig> {
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
    Some(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crw_core::config::{RendererConfig, StealthConfig};

    const THRESH: usize = 100;

    #[test]
    fn quality_escalation_skips_json_only() {
        // Prod: 114 of 135 quality escalations ended in "keeping HTTP" because
        // a healthy JSON body scores low-quality (no sentences, few word-like
        // tokens) and bought a browser render that was then discarded.
        let json = r#"{"id":1,"body":"comment text"},"#.repeat(200); // > 5000 bytes
        let q = |ct, html: &str| escalate_for_quality(false, true, 200, html, ct);
        assert!(!q(Some("application/json"), &json));
        // Production sends the charset suffix, so the media type is what counts.
        assert!(!q(Some("application/json; charset=utf-8"), &json));
        assert!(!q(Some("APPLICATION/JSON"), &json));
        // Everything else is left exactly as it behaves on main. Only JSON was
        // measured, and gating a type we never measured would trade recall for
        // a saving we cannot show. `text/plain` and `application/octet-stream`
        // in particular can carry a mislabeled SPA shell that a browser sniffs
        // and hydrates, so they must keep escalating.
        assert!(q(Some("text/html"), &json));
        assert!(q(None, &json));
        assert!(q(Some("text/plain"), &json));
        assert!(q(Some("application/octet-stream"), &json));
        // A vendor wall served as JSON is the one JSON case still worth a retry:
        // another fingerprint can clear it.
        let walled = format!("{json}\"url\":\"https://captcha-delivery.com/x\"");
        assert!(q(Some("application/json"), &walled));
        // A byte-thin body never reaches this trigger, whatever the type.
        assert!(!escalate_for_quality(
            true,
            true,
            200,
            &json,
            Some("text/html")
        ));
    }

    /// Every body below is a VERBATIM prefix of a real prod scrape from
    /// 2026-08-09..12 that was billed as content. They render fine and carry
    /// substantial markdown, so they sail past both the HTTP-status gate and the
    /// `>= threshold` guard — which is why they needed their own arm.
    #[test]
    fn classify_block_parked_domain_templates() {
        // (requested url, markdown) — the host must be the one the page names.
        let cases: [(&str, &str); 4] = [
            (
                "https://ft-access.com/",
                "ft-access.com\n\nis parked free, courtesy of GoDaddy.com.\n\n\
                 [Get This Domain](https://www.godaddy.com/domainsearch/find?key=parkweb)",
            ),
            (
                "https://arym.com/",
                "# Arym.com is for sale\n\nWe value your privacy\n\nWe use cookies to \
                 enhance your browsing experience and serve personalised ads.",
            ),
            (
                // eWeb holding page: no "for sale" wording at all.
                "https://sterki.com/",
                "# Sterki.com -\n        Ready for Development\n\n[Contact Us for Details]\
                 (https://ewebdevelopment.com/quotes/inquire/sterki.com)\n\n# Sterki.com\n\n\
                 ## Ready For Development\n\nIf you're interested in this domain, contact us.",
            ),
            (
                // `www.` on the request must still match the bare name in the body.
                "https://www.conditorei.com/",
                "conditorei.com :: this domain is for sale\n\nInquire now for pricing \
                 and availability through our brokerage partner.",
            ),
        ];
        for (url, md) in cases {
            let b = classify_block(
                200,
                Some("text/html"),
                "<html></html>",
                Some(md),
                false,
                THRESH,
                url,
                None,
            )
            .unwrap_or_else(|| panic!("{url} must be flagged"));
            assert_eq!(b.vendor, crw_core::types::PARKED_DOMAIN_VENDOR, "{url}");
            // Wording matters as much as the verdict: telling the caller a domain that
            // is for sale was "blocked by anti-bot" is what sends them to buy proxies.
            assert!(
                b.message().starts_with("No usable content"),
                "{url} must not be worded as an anti-bot block, got {:?}",
                b.message()
            );
        }
    }

    /// THE guarantee. The identical body is a parking page when served AT that domain
    /// and ordinary editorial content when served anywhere else, and the host
    /// comparison is the only thing that tells them apart.
    ///
    /// `Insurance.com is for sale` is a real domain-industry headline; without this
    /// check every article covering a domain sale, and every broker's inventory page,
    /// becomes a failed scrape.
    #[test]
    fn classify_block_parked_arm_requires_the_page_to_name_its_own_host() {
        let body = "# Insurance.com is for sale\n\nThe record-setting domain returns to \
                    the market more than a decade after it changed hands.";
        let call = |url: &str| {
            classify_block(
                200,
                Some("text/html"),
                "<html></html>",
                Some(body),
                false,
                THRESH,
                url,
                None,
            )
        };
        assert!(
            call("https://insurance.com/").is_some(),
            "served at the domain it names, this is a parking page"
        );
        assert!(
            call("https://domainnamewire.com/2026/08/12/insurance-com/").is_none(),
            "the same text on a news site is editorial content, not a parking page"
        );
    }

    /// A post-redirect host counts too. Discriminating on purpose: the REQUESTED host
    /// deliberately does not appear in the body, so this passes only if `final_url` is
    /// really consulted, and the body clears `THRESH` so a `Some` cannot come from the
    /// structural arm instead.
    #[test]
    fn classify_block_parked_arm_accepts_either_requested_or_final_host() {
        let md = "# Parked-Target.com is for sale\n\nThis premium name is available \
                  immediately. Submit an offer through our brokerage and we will respond \
                  within one business day with pricing and transfer details.";
        assert!(md.len() > THRESH, "fixture must clear the threshold guard");
        let call = |requested: &str, final_url: Option<&str>| {
            classify_block(
                200,
                Some("text/html"),
                "<html><body><p>a real enough shell</p></body></html>",
                Some(md),
                false,
                THRESH,
                requested,
                final_url,
            )
        };
        assert!(
            call(
                "https://links.example.org/out?to=parked-target",
                Some("https://parked-target.com/"),
            )
            .is_some(),
            "the post-redirect host names the page and must count"
        );
        // Same body, same requested URL, no redirect recorded: nothing names the page.
        assert!(
            call("https://links.example.org/out?to=parked-target", None).is_none(),
            "without the final host there is no anchor, so the arm must decline"
        );
    }

    /// The token must be a host, not any dotted word. `[a-z]{2,12}` after a dot also
    /// accepts file extensions, so a task board line like
    /// `- checkout.html ready for development` matched before the host check existed.
    #[test]
    fn classify_block_parked_arm_ignores_dotted_non_hosts() {
        let md = "# Sprint 12 board\n\n- checkout.html ready for development\n\
                  - main.py ready for development\n- config.yaml is for sale (internal joke)";
        assert!(
            classify_block(
                200,
                Some("text/html"),
                "<html></html>",
                Some(md),
                false,
                THRESH,
                "https://tasks.internal.example/board",
                None,
            )
            .is_none(),
            "dotted filenames are not the scraped host"
        );
    }

    /// The capture must be the WHOLE dotted name. `\b` matches straight after a dot, so
    /// the first version of this pattern read `shop.example.com is for sale` as
    /// `example.com` — which then equalled a scrape of example.com and failed a real
    /// page. It also truncated `example.co.uk` to `co.uk`, missing genuine parked
    /// pages, and could not match a single-letter label at all.
    #[test]
    fn classify_block_parked_arm_captures_whole_domain_tokens() {
        let call = |md: &str, url: &str| {
            classify_block(
                200,
                Some("text/html"),
                "<html></html>",
                Some(md),
                false,
                THRESH,
                url,
                None,
            )
        };
        // A subdomain named in the body is NOT the host that was scraped. The body is
        // kept comfortably above `THRESH` so a `None` here proves the parked arm
        // declined, rather than the structural arm firing on a thin page.
        let announcement = "# Our shop is moving\n\nshop.example.com is for sale, and the \
             main site stays exactly where it is. Existing orders, accounts and support \
             tickets are unaffected by the change; only the storefront hostname retires.";
        assert!(
            call(announcement, "https://example.com/").is_none(),
            "a subdomain mentioned in prose must not satisfy the host anchor"
        );
        // ...but scraping that subdomain directly does match it.
        assert!(
            call("shop.example.com is for sale", "https://shop.example.com/").is_some(),
            "the page names exactly the host it was served at"
        );
        // Multi-label TLDs must survive whole.
        assert!(
            call("example.co.uk is for sale", "https://example.co.uk/").is_some(),
            "a .co.uk parked page must be caught, not truncated to co.uk"
        );
        // Single-character label.
        assert!(
            call("x.com is for sale", "https://x.com/").is_some(),
            "a one-letter label is still a host"
        );
        // A leading www. in the body still matches the bare scraped host.
        assert!(
            call("www.arym.com is for sale", "https://arym.com/").is_some(),
            "www. is stripped on both sides before comparing"
        );
    }

    /// The deliberate scope limit, pinned so nobody "improves" it later. An
    /// under-construction page is a state the origin published on purpose — this body
    /// is `cset-ag.com`, a real WordPress site in maintenance serving its own logo —
    /// so it is a truthful scrape and stays billable. 68% of the flagged population
    /// looks like this, which is why the fix does not take the class to zero.
    #[test]
    fn classify_block_leaves_a_real_under_construction_page_alone() {
        let md = "![logo](https://www.cset-ag.com/wp-content/uploads/2025/09/cset-wit.png)\n\n\
                  ## Under construction for new update\n\n### Clear Sustainable Energy Trading\n\n\
                  © CSET Group 2025";
        assert!(
            classify_block(
                200,
                Some("text/html"),
                "<html></html>",
                Some(md),
                false,
                THRESH,
                "https://www.cset-ag.com/",
                None,
            )
            .is_none(),
            "an origin's own under-construction page is real content"
        );
    }

    /// The four unanchored template literals that used to live here are gone, and this
    /// pins why: every nginx install tutorial quotes the default vhost verbatim to
    /// confirm the install worked, and it is exactly the kind of page a RAG pipeline
    /// scrapes. Same shape for the Apache default page and for UDRP decisions saying
    /// "this domain has been registered and is being used in bad faith".
    #[test]
    fn classify_block_leaves_technical_docs_quoting_default_pages_alone() {
        let tutorial = "# How to install nginx on Ubuntu\n\nAfter `apt install nginx`, \
             open the server in a browser. You should see the default landing page: \
             \"Welcome to nginx! If you see this page, the nginx web server is \
             successfully installed and working. Further configuration is required.\" \
             If instead you get the Apache2 Ubuntu Default Page, another service is \
             bound to port 80.\n\nNote that this domain has been registered for the \
             lab and resolves locally.\n"
            .repeat(3);
        assert!(
            classify_block(
                200,
                Some("text/html"),
                "<html></html>",
                Some(&tutorial),
                false,
                THRESH,
                "https://www.digitalocean.com/community/tutorials/install-nginx",
                None,
            )
            .is_none(),
            "a tutorial quoting default landing pages is real content"
        );
    }

    /// Non-ASCII markdown is routine and `&s[..n]` panics off a char boundary. The
    /// 3-byte repeat unit puts byte 40_960 mid-character (40_960 % 3 == 1), so the
    /// boundary walk in `head()` actually executes — a 29-byte unit lands ON a
    /// boundary and the loop never runs, which made the previous version vacuous.
    #[test]
    fn classify_block_parked_arm_survives_multibyte_markdown() {
        let md = "€".repeat(20_000); // 60 KB, well past the 40_960 scan window
        assert_eq!(md.len(), 60_000);
        assert!(
            !md.is_char_boundary(40_960),
            "fixture must straddle the window"
        );
        assert!(
            classify_block(
                200,
                Some("text/html"),
                "<html></html>",
                Some(&md),
                false,
                THRESH,
                "https://example.com/",
                None,
            )
            .is_none(),
            "multibyte content must neither panic nor be called parked"
        );
    }

    #[test]
    fn classify_block_challenge_on_cf_200() {
        // Ticket headline: a CF challenge served with HTTP 200. TIER2 "Just a
        // moment" does NOT run on 200, so a real TIER1 token (__cf_chl_f_tk=) is
        // required to reach vendor=cloudflare.
        let html = r#"<html><body><form id="challenge-form" action="/cdn-cgi/?__cf_chl_f_tk=abc"></form></body></html>"#;
        let b = classify_block(
            200,
            Some("text/html"),
            html,
            None,
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("CF challenge must be flagged");
        assert_eq!(b.vendor, "cloudflare");
    }

    #[test]
    fn classify_block_turnstile_200_large_body() {
        // Real prod regression (#350, barcodelookup.com): modern CF Turnstile
        // served at HTTP 200 in a ~118KB body with NO old `challenge-form` markup
        // — antibot::classify misses it and it leaked as success:true. The strong
        // marker `_cf_chl_opt` sits in the <head> script; it must be detected even
        // past the 80KB scan cap AND even when substantial markdown was extracted.
        // The challenge script is injected near the END of the body (measured at
        // byte ~114k of the real 118k page), so the marker MUST be found past any
        // prefix cap — put it after 100KB of filler to lock in a full-html scan.
        let mut html = String::from("<html><body>");
        html.push_str(&"<p>filler</p>".repeat(10_000)); // ~120KB of leading filler
        html.push_str(r#"<script>window._cf_chl_opt={cvId:"3"};</script></body></html>"#);
        assert!(
            html.find("_cf_chl_opt").unwrap() > 80_000,
            "marker must sit past the old 80KB prefix to guard the regression"
        );
        let md = "recovered looking text ".repeat(50); // > THRESH, must NOT suppress
        let b = classify_block(
            200,
            Some("text/html"),
            &html,
            Some(&md),
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("Turnstile 200 interstitial must be flagged");
        assert_eq!(b.vendor, "cloudflare");
    }

    #[test]
    fn classify_block_no_block_on_cleared_managed_page_with_trailing_platform_script() {
        // Real prod regression (cloak arm): a managed-Turnstile site the cloak
        // tier successfully SOLVED returns the real page (~783KB Glassdoor), but
        // Cloudflare re-injects the `/cdn-cgi/challenge-platform/` telemetry loader
        // near the END of the cleared body (measured at byte ~782k) with NO
        // `_cf_chl_opt`. A full-html scan for challenge-platform false-positived it
        // as an interstitial and emptied the recovered content. The cleared page
        // must NOT be flagged: it carries content markers and no interstitial-only
        // token.
        let mut html = String::from(
            r#"<!DOCTYPE html><html><head><title>Working at Google | Glassdoor</title></head><body>"#,
        );
        html.push_str(&"<p>Real reviews and salaries content.</p>".repeat(10_000)); // >512KB
        html.push_str(
            r#"<script src="/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1?ray=abc"></script></body></html>"#,
        );
        assert!(
            html.contains("/cdn-cgi/challenge-platform/") && !html.contains("_cf_chl_opt"),
            "fixture must reproduce the cleared-page shape"
        );
        // Substantial recovered markdown too — the real content extracts fine.
        let md = "Real reviews and salaries content. ".repeat(50);
        assert!(
            classify_block(
                200,
                Some("text/html"),
                &html,
                Some(&md),
                false,
                THRESH,
                "https://example.com/",
                None
            )
            .is_none(),
            "a cleared managed page with a trailing challenge-platform telemetry \
             script (but no _cf_chl_opt) must not be misflagged as a challenge"
        );
    }

    #[test]
    fn classify_block_hard_block_on_datadome_403() {
        let html = r#"<html><body><script src="https://captcha-delivery.com/c.js"></script></body></html>"#;
        let b = classify_block(
            403,
            Some("text/html"),
            html,
            None,
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("DataDome block must be flagged");
        assert_eq!(b.vendor, "datadome");
    }

    #[test]
    fn classify_block_wikimedia_200_shell_over_markdown_guard() {
        // Regression for the silent-success bug: Wikimedia's HTTP-200 datacenter
        // ban extracts to ~110 bytes of error prose (> THRESH), so the
        // markdown-substantial guard would suppress the antibot verdict. The
        // strong-marker check must classify it as a block regardless, mirroring
        // the CF strong-marker path.
        let html = r#"<!DOCTYPE html><html lang="en"><title>Wikimedia Error</title>
<div class="content"><h1>Error</h1><p>Contabo networks are forbidden due to abuse.</p></div>
<div class="footer"><p>If you report this error to the Wikimedia System Administrators, please include the details below.</p></div></html>"#;
        // Matches what crw_extract::extract() yields for this shell (~114 bytes).
        let md = "# Wikimedia Error\n\n# Error\n\nContabo networks are forbidden due to abuse. Contact noc@wikimedia.org for assistance.";
        assert!(
            md.len() >= THRESH,
            "fixture must exceed the guard to be meaningful"
        );
        let b = classify_block(
            200,
            Some("text/html"),
            html,
            Some(md),
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("wikimedia datacenter block must be flagged even with substantial markdown");
        assert_eq!(b.vendor, "generic_block");
    }

    // Regression using the ACTUAL text captured in prod (9-day trace-log
    // investigation, 2026-07-15..23) rather than a synthetic fixture — this is
    // exactly what caught the Vercel gap below (the synthetic fixture used to
    // ship that fix was artificially short and never exercised this guard).
    // Raw HTML wasn't captured (format=markdown only); the real markdown
    // stands in for html here since this check is a text match, not
    // DOM-structural.
    #[test]
    fn classify_block_reddit_real_prod_capture() {
        let real_markdown = "You've been blocked by network security.\n\nTo continue, log in to your Reddit account or use your developer token  \n  \nIf you think you've been blocked by mistake, file a ticket below and we'll look into it.\n\n[Log in](https://www.reddit.com/login/) [File a ticket](https://support.reddithelp.com/hc/en-us/requests/new?ticket_form_id=21879292693140)";
        assert!(real_markdown.len() >= THRESH);
        let b = classify_block(
            200,
            Some("text/html"),
            real_markdown,
            Some(real_markdown),
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("the exact text that silently returned success:true 198x in prod must be flagged");
        assert_eq!(b.vendor, "network_security");
    }

    #[test]
    fn classify_block_reddit_network_security_over_markdown_guard() {
        // Regression: Reddit's own block page extracts to ~115 bytes of prose
        // (> THRESH), so without the strong-marker check the guard would
        // suppress the verdict before antibot::classify ever ran.
        let html = "<html><body><p>You've been blocked by network security.</p>\
            <p>To continue, log in to your Reddit account or use your developer token</p></body></html>";
        let md = "You've been blocked by network security.\n\nTo continue, \
            log in to your Reddit account or use your developer token";
        assert!(
            md.len() >= THRESH,
            "fixture must exceed the guard to be meaningful"
        );
        let b = classify_block(
            200,
            Some("text/html"),
            html,
            Some(md),
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("reddit network security block must be flagged even with substantial markdown");
        assert_eq!(b.vendor, "network_security");
    }

    #[test]
    fn classify_block_reddit_phrase_alone_is_not_enough() {
        // Negative: an article that quotes the first half of Reddit's block
        // sentence in isolation (no "log in to your Reddit account" nearby)
        // must NOT be flagged — only the full page's block page trips this.
        let html = "<html><body><article><p>Many scrapers report seeing \
            \"blocked by network security\" style errors when hitting Reddit at \
            scale, which is a common anti-bot pattern across social platforms.</p>\
            </article></body></html>";
        let md = "Many scrapers report seeing \"blocked by network security\" style \
            errors when hitting Reddit at scale, which is a common anti-bot pattern.";
        assert!(md.len() >= THRESH);
        assert!(
            classify_block(
                200,
                Some("text/html"),
                html,
                Some(md),
                false,
                THRESH,
                "https://example.com/",
                None
            )
            .is_none(),
            "an article merely discussing the phrase must not be misflagged as a block"
        );
    }

    #[test]
    fn classify_block_cloudflare_hard_block_over_markdown_guard() {
        // Regression: Cloudflare's hard-deny page (no interstitial, so
        // CF_STRONG_MARKERS above doesn't match) extracts to well over THRESH
        // bytes of prose, so it needs its own strong-marker check.
        let html = r#"<html><body><h1>Attention Required! | Cloudflare</h1>
            <p>Please enable cookies.</p>
            <span class="cf-error-code">1020</span>
            <h1>Sorry, you have been blocked</h1>
            <h2>You are unable to access example.com</h2></body></html>"#;
        let md = "# Attention Required! | Cloudflare\n\nPlease enable cookies.\n\n\
            # Sorry, you have been blocked\n\n## You are unable to access example.com";
        assert!(
            md.len() >= THRESH,
            "fixture must exceed the guard to be meaningful"
        );
        let b = classify_block(
            200,
            Some("text/html"),
            html,
            Some(md),
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("cloudflare hard block must be flagged even with substantial markdown");
        assert_eq!(b.vendor, "cloudflare");
    }

    #[test]
    fn classify_block_modern_cf_block_page_without_error_code_span() {
        // The CURRENT Cloudflare block template (scrubbed live capture) has no
        // `cf-error-code` span, so the arm above it never matched and the page
        // fell through to `generic_block`, invisible to the routing learner.
        let html = include_str!("../tests/fixtures/cloudflare_1020_block.html");
        assert!(!html.contains(r#"<span class="cf-error-code">"#));
        let md = "x".repeat(500);
        let out = classify_block(
            403,
            Some("text/html"),
            html,
            Some(&md),
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("the block page must classify");
        assert_eq!(out.vendor, "cloudflare");
        assert!(out.reason.contains("cf-error-details"), "{}", out.reason);
    }

    #[test]
    fn classify_block_cf_error_code_marker_alone_is_not_enough() {
        // Negative: a page that legitimately renders a `cf-error-code` span
        // (e.g. a status/monitoring dashboard embedding one as a live example,
        // not a real hard-block response) but has no "you have been blocked"
        // heading must not be misflagged — the marker alone isn't sufficient,
        // only its co-occurrence with the block heading is.
        let html = r#"<html><body><article><h1>Error code reference</h1>
            <p>Example live element: <span class="cf-error-code">1020</span></p>
            <p>This is a normal reference page with plenty of unrelated
            documentation content describing how status codes are displayed.</p>
            </article></body></html>"#;
        let md = "# Error code reference\n\nExample live element: 1020\n\n\
            This is a normal reference page with plenty of unrelated documentation \
            content describing how status codes are displayed.";
        assert!(md.len() >= THRESH);
        assert!(
            classify_block(
                200,
                Some("text/html"),
                html,
                Some(md),
                false,
                THRESH,
                "https://example.com/",
                None
            )
            .is_none(),
            "a page merely rendering the cf-error-code marker without the block heading must not be misflagged"
        );
    }

    #[test]
    fn classify_block_vercel_checkpoint_over_markdown_guard_real_capture() {
        // Regression using the EXACT text captured in prod (2026-07-24 trace-log
        // investigation): the real Vercel checkpoint page's "Website owner?
        // Click here to fix" link pushes it to ~135 bytes, over THRESH, so
        // antibot::classify's Vercel pattern never ran — this shipped once
        // already because the synthetic fixture used to validate that pattern
        // was artificially short (~58 bytes) and never hit this guard.
        let html = "<html><body><h1>Vercel Security Checkpoint</h1>\
            <p>We're verifying your browser</p>\
            <p><a href=\"https://vercel.link/security-checkpoint\">Website owner? Click here to fix</a></p>\
            </body></html>";
        let md = "# Vercel Security Checkpoint\n\nWe're verifying your browser\n\n\
            [Website owner? Click here to fix](https://vercel.link/security-checkpoint)";
        assert!(
            md.len() >= THRESH,
            "this is the real-world case: the fixture must exceed the guard"
        );
        let b = classify_block(
            200,
            Some("text/html"),
            html,
            Some(md),
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("vercel checkpoint must be flagged even with substantial markdown");
        assert_eq!(b.vendor, "vercel");
    }

    #[test]
    fn classify_block_vercel_mention_alone_is_not_enough() {
        // Negative: a page that merely mentions Vercel (a common hosting
        // platform) with no checkpoint heading must not be misflagged.
        let html = "<html><body><article><h1>Deploying on Vercel</h1>\
            <p>This site is deployed on Vercel, a popular platform for hosting \
            frontend applications and static sites with automatic previews on \
            every pull request submitted to the repository.</p></article></body></html>";
        let md = "# Deploying on Vercel\n\nThis site is deployed on Vercel, a popular \
            platform for hosting frontend applications and static sites with \
            automatic previews on every pull request submitted to the repository.";
        assert!(md.len() >= THRESH);
        assert!(
            classify_block(
                200,
                Some("text/html"),
                html,
                Some(md),
                false,
                THRESH,
                "https://example.com/",
                None
            )
            .is_none(),
            "an article merely mentioning Vercel without the checkpoint heading must not be misflagged"
        );
    }

    #[test]
    fn classify_block_no_block_when_markdown_substantial() {
        // Anti-over-trigger: real recovered content is authoritative even under a
        // soft-block status with CF markers in the (stale) html.
        let html = r#"<html><body><form id="challenge-form" action="/cdn-cgi/?__cf_chl_f_tk=abc"></form></body></html>"#;
        let md = "x".repeat(500);
        assert!(
            classify_block(
                403,
                Some("text/html"),
                html,
                Some(&md),
                false,
                THRESH,
                "https://example.com/",
                None
            )
            .is_none()
        );
    }

    #[test]
    fn classify_block_no_block_on_clean_200() {
        let html = "<!doctype html><html><head><title>Article</title></head><body>\
            <article><h1>Hello</h1><p>This is a normal article with plenty of \
            meaningful text content describing something at length.</p></article></body></html>";
        assert!(
            classify_block(
                200,
                Some("text/html"),
                html,
                Some("# Hello\n\nreal body"),
                false,
                THRESH,
                "https://example.com/",
                None
            )
            .is_none()
        );
    }

    #[test]
    fn classify_block_screenshot_suppresses_structural_only() {
        // Near-empty 200 shell + screenshot => structural failure suppressed.
        assert!(
            classify_block(
                200,
                Some("text/html"),
                "",
                None,
                true,
                THRESH,
                "https://example.com/",
                None
            )
            .is_none()
        );
        // Positive vendor block is NOT suppressed by a screenshot.
        let html = r#"<html><body><script src="https://captcha-delivery.com/c.js"></script></body></html>"#;
        assert!(
            classify_block(
                403,
                Some("text/html"),
                html,
                None,
                true,
                THRESH,
                "https://example.com/",
                None
            )
            .is_some()
        );
    }

    #[test]
    fn classify_block_pdf_skipped() {
        // PDF branch has empty html — must not false-flag as StructuralFailure.
        assert!(
            classify_block(
                200,
                Some("application/pdf"),
                "",
                None,
                false,
                THRESH,
                "https://example.com/",
                None
            )
            .is_none()
        );
    }

    #[test]
    fn classify_block_skips_non_html_payloads() {
        // A 68-byte requirements.txt from raw.githubusercontent.com came back
        // `success:false` / `anti_bot` in prod: "Near-empty content (68 bytes)
        // with HTTP 200". It is a complete, valid file.
        let txt = "fastapi>=0.110.0\nuvicorn>=0.29.0\npydantic>=2.6\npython-multipart\n";
        assert!(
            classify_block(
                200,
                Some("text/plain"),
                txt,
                Some(txt),
                false,
                THRESH,
                "https://example.com/",
                None
            )
            .is_none()
        );
        for ct in [
            "text/csv",
            "application/json",
            "text/markdown",
            "application/javascript",
            "text/css",
        ] {
            assert!(
                classify_block(
                    200,
                    Some(ct),
                    "x",
                    None,
                    false,
                    THRESH,
                    "https://example.com/",
                    None
                )
                .is_none(),
                "{ct} was classified as a block"
            );
        }
    }

    #[test]
    fn classify_block_still_sees_a_vendor_wall_under_a_data_content_type() {
        // Only the HTML SHAPE heuristics are suppressed for non-HTML bodies.
        // DataDome answers XHR-shaped requests with an application/json captcha
        // stub, and that is still a block.
        let json = r#"{"url":"https://geo.captcha-delivery.com/captcha/?initialCid=x"}"#;
        assert!(
            classify_block(
                200,
                Some("application/json"),
                json,
                None,
                false,
                THRESH,
                "https://example.com/",
                None
            )
            .is_some()
        );
    }

    #[test]
    fn classify_block_still_sees_walls_without_a_content_type() {
        // The guard must not blind the classifier: a wall is html or type-less.
        let html = "<html><body>You've been blocked by network security. \
                    Please log in to your Reddit account.</body></html>";
        assert!(
            classify_block(
                200,
                None,
                html,
                None,
                false,
                THRESH,
                "https://example.com/",
                None
            )
            .is_some()
        );
    }

    #[test]
    fn classify_block_407_is_our_proxy_not_a_target_block() {
        // 215 records in 14 days of prod were our own DataImpulse egress failing
        // auth, stamped `structural_failure` and poisoning the routing registry.
        assert!(
            classify_block(
                407,
                Some("text/html"),
                "",
                None,
                false,
                THRESH,
                "https://example.com/",
                None
            )
            .is_none()
        );
        // Everything else with an empty body stays classifiable: a near-empty
        // 403/503 is the canonical CloudFront/Akamai deny signature.
        assert!(
            classify_block(
                403,
                Some("text/html"),
                "",
                None,
                false,
                THRESH,
                "https://example.com/",
                None
            )
            .is_some()
        );
    }

    #[test]
    fn classify_block_modern_cf_marker_beats_the_markdown_guard() {
        // Why the accepted JS escalation must replace `fetch_result.html`: with
        // the discarded tier's shell still in place, a challenge we SOLVED is
        // stamped `cloudflare` here and `clear_body()` throws the recovery away.
        let html = r#"<html><body><script>window._cf_chl_opt={}</script></body></html>"#;
        let md = "x".repeat(500);
        assert!(
            classify_block(
                200,
                Some("text/html"),
                html,
                Some(&md),
                false,
                THRESH,
                "https://example.com/",
                None
            )
            .is_some()
        );
    }

    fn sample_fetch(status_code: u16, html: &str) -> FetchResult {
        FetchResult {
            url: "https://example.com".into(),
            final_url: None,
            status_code,
            html: html.into(),
            content_type: None,
            raw_bytes: None,
            rendered_with: None,
            elapsed_ms: 10,
            warning: None,
            render_decision: None,
            credit_cost: 0,
            warnings: Vec::new(),
            truncated: false,
            deadline_exceeded: false,
            captured_responses: Vec::new(),
            screenshot: None,
            block: None,
        }
    }

    /// The raw base64 in `FetchResult.screenshot` is wrapped into a
    /// `data:image/png;base64,` URL exactly once when building `ScrapeData`.
    #[test]
    fn screenshot_wrapped_as_data_url() {
        let mut fetch = sample_fetch(200, "<html><body>hi</body></html>");
        fetch.screenshot = Some("AAAQ".to_string());
        let wrapped = fetch
            .screenshot
            .as_ref()
            .map(|b| format!("data:image/png;base64,{b}"));
        assert_eq!(
            wrapped.as_deref(),
            Some("data:image/png;base64,AAAQ"),
            "raw b64 must be prefixed with the data URL scheme exactly once"
        );
    }

    #[test]
    fn redirect_material_detects_path_to_root_collapse() {
        assert!(redirect_is_material(
            "https://northernair.ca/history.htm",
            "https://northernair.ca/"
        ));
    }

    #[test]
    fn redirect_material_detects_host_change() {
        assert!(redirect_is_material(
            "https://example.com/path",
            "https://other.com/path"
        ));
    }

    #[test]
    fn redirect_material_ignores_trailing_slash() {
        assert!(!redirect_is_material(
            "https://example.com/path",
            "https://example.com/path/"
        ));
    }

    #[test]
    fn redirect_material_ignores_query_only_change() {
        assert!(!redirect_is_material(
            "https://example.com/page",
            "https://example.com/page?utm=x"
        ));
    }

    #[test]
    fn warning_detects_target_status_codes() {
        let warning = derive_target_warning(&sample_fetch(403, "<html></html>"));
        assert_eq!(warning.as_deref(), Some("Target returned 403 Forbidden"));
    }

    #[test]
    fn cdn_origin_errors_are_failures_not_pages() {
        // Cloudflare's private 52x range: generated by the CDN, never by an
        // origin, so the body is always the CDN's own error page.
        for status in 520..=527 {
            assert!(
                is_cdn_origin_error(status),
                "{status} not treated as a CDN origin error"
            );
        }
        // Everything around it must be untouched. 502/503/504 are ordinary
        // gateway statuses that a real origin (or its reverse proxy) does emit,
        // and a 503 maintenance page can be content the caller wants. 530 is
        // excluded deliberately: it carries a Cloudflare 1XXX code that includes
        // firewall denials (1020), which is a block on us rather than a broken
        // origin, and a different egress can clear it — see the doc comment.
        for status in [
            200, 301, 403, 404, 429, 500, 502, 503, 504, 508, 519, 528, 530,
        ] {
            assert!(
                !is_cdn_origin_error(status),
                "{status} wrongly treated as a CDN origin error"
            );
        }
    }

    #[test]
    fn cdn_origin_error_page_defeats_the_body_length_guard() {
        // The reason this needs its own rule rather than a bigger threshold: the
        // scrape routes fail a `>= 400` only when the body is under 200 bytes,
        // and a real Cloudflare 522 page renders to ~1250 bytes of markdown.
        let body = "You\n\n### Browser\n\nWorking\n\n### Cloudflare\n\nWorking\n\nwww.example.com\n\n### Host\n\nError\n\n## What happened?\n\nThe initial connection between Cloudflare's network and the origin web server timed out.".repeat(4);
        assert!(
            body.len() > 200,
            "fixture must exceed the routes' 200-byte guard"
        );
        assert!(is_cdn_origin_error(522));
    }

    #[test]
    fn warning_detects_block_markers() {
        let warning = derive_target_warning(&sample_fetch(
            200,
            "<html><title>Just a moment</title><body>cf-browser-verification</body></html>",
        ));
        assert_eq!(warning.as_deref(), Some("Blocked by anti-bot protection"));
    }

    #[test]
    fn empty_truncated_render_is_a_failure_not_an_empty_page() {
        let md = [OutputFormat::Markdown];
        // The billed-blank-page case: budget expired, nothing extracted.
        assert!(is_empty_truncated_render(true, &md, None));
        assert!(is_empty_truncated_render(true, &md, Some("   \n ")));
        // A thin-but-present body is a quality judgment, not a missing answer —
        // failing it would cost recall.
        assert!(!is_empty_truncated_render(true, &md, Some("# Title")));
        // An empty page that rendered fully is genuinely empty; say so.
        assert!(!is_empty_truncated_render(false, &md, None));
        // A caller who wanted raw HTML can still use a partial DOM.
        assert!(!is_empty_truncated_render(
            true,
            &[OutputFormat::RawHtml],
            None
        ));
    }

    #[test]
    fn warning_keeps_js_escalation_failure_alongside_a_block() {
        // A block page is the likeliest body to be holding a failed-ladder
        // explanation, and the docs tell callers to look for that prefix. The
        // block marker used to short-circuit and drop it.
        let mut fetch = sample_fetch(
            200,
            "<html><title>Just a moment</title><body>cf-browser-verification</body></html>",
        );
        fetch.warning = Some(format!(
            "{} Timeout after 5000ms",
            crw_renderer::JS_ESCALATION_FAILED
        ));
        let warning = derive_target_warning(&fetch).expect("both signals expected");
        assert!(
            warning.contains("Blocked by anti-bot protection"),
            "{warning}"
        );
        assert!(warning.contains("js_escalation_failed"), "{warning}");
    }

    #[test]
    fn warning_skips_legit_pages_mentioning_captcha() {
        // Regression: HN front page used to false-positive because the headline
        // "Google broke reCAPTCHA…" matched a bare "captcha" substring marker.
        let warning = derive_target_warning(&sample_fetch(
            200,
            "<html><body>Google broke reCAPTCHA for de-googled Android users</body></html>",
        ));
        assert!(warning.is_none(), "got false-positive: {warning:?}");
    }

    // ── canonical_status_text ───────────────────────────────────────────────

    #[test]
    fn canonical_status_text_maps_every_documented_code() {
        let cases: [(u16, &str); 13] = [
            (400, "Bad Request"),
            (401, "Unauthorized"),
            (403, "Forbidden"),
            (404, "Not Found"),
            (405, "Method Not Allowed"),
            (408, "Request Timeout"),
            (410, "Gone"),
            (429, "Too Many Requests"),
            (451, "Unavailable For Legal Reasons"),
            (500, "Internal Server Error"),
            (502, "Bad Gateway"),
            (503, "Service Unavailable"),
            (504, "Gateway Timeout"),
        ];
        for (code, text) in cases {
            assert_eq!(canonical_status_text(code), text, "status {code}");
        }
    }

    #[test]
    fn canonical_status_text_falls_back_to_error_for_unmapped_codes() {
        for code in [200, 301, 402, 406, 418, 499, 501, 507, 599] {
            assert_eq!(
                canonical_status_text(code),
                "Error",
                "status {code} should not have a specific mapping"
            );
        }
    }

    #[test]
    fn canonical_status_text_boundary_around_a_mapped_code() {
        // 429 is mapped, its neighbors are not.
        assert_eq!(canonical_status_text(428), "Error");
        assert_eq!(canonical_status_text(429), "Too Many Requests");
        assert_eq!(canonical_status_text(430), "Error");
    }

    // ── change_tracking_mode_label ──────────────────────────────────────────

    fn ct_opts(modes: Vec<ChangeTrackingMode>) -> crw_core::types::ChangeTrackingOptions {
        crw_core::types::ChangeTrackingOptions {
            modes,
            ..Default::default()
        }
    }

    #[test]
    fn change_tracking_label_binary_for_non_text_content_type() {
        assert_eq!(
            change_tracking_mode_label(&ct_opts(vec![]), Some("image/png")),
            "binary"
        );
        assert_eq!(
            change_tracking_mode_label(&ct_opts(vec![]), Some("application/octet-stream")),
            "binary"
        );
    }

    #[test]
    fn change_tracking_label_empty_modes_defaults_to_git_diff() {
        // `opts.modes.is_empty()` counts as gitDiff per the doc comment.
        assert_eq!(
            change_tracking_mode_label(&ct_opts(vec![]), None),
            "gitDiff"
        );
    }

    #[test]
    fn change_tracking_label_explicit_git_diff_only() {
        assert_eq!(
            change_tracking_mode_label(&ct_opts(vec![ChangeTrackingMode::GitDiff]), None),
            "gitDiff"
        );
    }

    #[test]
    fn change_tracking_label_json_only() {
        assert_eq!(
            change_tracking_mode_label(&ct_opts(vec![ChangeTrackingMode::Json]), None),
            "json"
        );
    }

    #[test]
    fn change_tracking_label_mixed_modes() {
        assert_eq!(
            change_tracking_mode_label(
                &ct_opts(vec![ChangeTrackingMode::GitDiff, ChangeTrackingMode::Json]),
                None
            ),
            "mixed"
        );
    }

    #[test]
    fn change_tracking_label_content_type_none_is_text() {
        // No content type means "assume text" (`is_none_or`).
        assert_eq!(
            change_tracking_mode_label(&ct_opts(vec![ChangeTrackingMode::Json]), None),
            "json"
        );
    }

    #[test]
    fn change_tracking_label_content_type_case_and_charset_insensitive() {
        assert_eq!(
            change_tracking_mode_label(&ct_opts(vec![]), Some("TEXT/PLAIN; charset=utf-8")),
            "gitDiff"
        );
        assert_eq!(
            change_tracking_mode_label(&ct_opts(vec![]), Some("application/XML")),
            "gitDiff"
        );
    }

    #[test]
    fn change_tracking_label_json_modes_irrelevant_under_binary_type() {
        // Binary short-circuits before the modes are even inspected.
        assert_eq!(
            change_tracking_mode_label(
                &ct_opts(vec![ChangeTrackingMode::GitDiff, ChangeTrackingMode::Json]),
                Some("image/jpeg")
            ),
            "binary"
        );
    }

    // ── normalized_host ──────────────────────────────────────────────────────

    #[test]
    fn normalized_host_lowercases_the_host() {
        assert_eq!(
            normalized_host("https://EXAMPLE.com/path"),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn normalized_host_strips_a_leading_www() {
        assert_eq!(
            normalized_host("https://www.example.com/"),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn normalized_host_strips_only_one_leading_www() {
        assert_eq!(
            normalized_host("https://www.www.example.com/"),
            Some("www.example.com".to_string())
        );
    }

    #[test]
    fn normalized_host_bracketed_ipv6() {
        assert_eq!(
            normalized_host("http://[::1]:8080/"),
            Some("[::1]".to_string())
        );
    }

    #[test]
    fn normalized_host_none_for_unparseable_url() {
        assert_eq!(normalized_host("not a url at all"), None);
    }

    #[test]
    fn normalized_host_none_for_hostless_scheme() {
        assert_eq!(normalized_host("mailto:foo@example.com"), None);
    }

    #[test]
    fn normalized_host_preserves_trailing_dot() {
        assert_eq!(
            normalized_host("https://example.com./x"),
            Some("example.com.".to_string())
        );
    }

    // ── build_byok_llm_config ────────────────────────────────────────────────

    #[test]
    fn byok_config_none_without_an_api_key() {
        let req = ScrapeRequest::default();
        assert!(build_byok_llm_config(&req, None).is_none());
        assert!(build_byok_llm_config(&req, Some(&LlmConfig::default())).is_none());
    }

    #[test]
    fn byok_config_uses_request_key_over_defaults_when_no_server_cfg() {
        let req = ScrapeRequest {
            llm_api_key: Some("sk-test".to_string()),
            ..Default::default()
        };
        let cfg = build_byok_llm_config(&req, None).expect("api key present");
        assert_eq!(cfg.api_key, "sk-test");
        assert_eq!(cfg.provider, LlmConfig::default().provider);
        assert_eq!(cfg.model, LlmConfig::default().model);
        assert_eq!(cfg.base_url, None);
    }

    #[test]
    fn byok_config_inherits_non_credential_fields_from_server_config() {
        let server = LlmConfig {
            max_tokens: 9999,
            max_concurrency: 42,
            provider: "azure".to_string(),
            ..LlmConfig::default()
        };
        let req = ScrapeRequest {
            llm_api_key: Some("sk-test".to_string()),
            ..Default::default()
        };
        let cfg = build_byok_llm_config(&req, Some(&server)).expect("api key present");
        assert_eq!(
            cfg.api_key, "sk-test",
            "credential must come from the request"
        );
        assert_eq!(
            cfg.max_tokens, 9999,
            "non-credential fields inherit from server cfg"
        );
        assert_eq!(cfg.max_concurrency, 42);
        assert_eq!(
            cfg.provider, "azure",
            "not overridden, so server's provider wins"
        );
    }

    #[test]
    fn byok_config_llm_provider_override_wins() {
        let server = LlmConfig {
            provider: "azure".to_string(),
            ..LlmConfig::default()
        };
        let req = ScrapeRequest {
            llm_api_key: Some("sk-test".to_string()),
            llm_provider: Some("openai".to_string()),
            ..Default::default()
        };
        let cfg = build_byok_llm_config(&req, Some(&server)).unwrap();
        assert_eq!(cfg.provider, "openai");
    }

    #[test]
    fn byok_config_llm_model_override_wins() {
        let req = ScrapeRequest {
            llm_api_key: Some("sk-test".to_string()),
            llm_model: Some("gpt-5".to_string()),
            ..Default::default()
        };
        let cfg = build_byok_llm_config(&req, None).unwrap();
        assert_eq!(cfg.model, "gpt-5");
    }

    #[test]
    fn byok_config_base_url_override_wins() {
        let req = ScrapeRequest {
            llm_api_key: Some("sk-test".to_string()),
            base_url: Some("https://byok.example.com/v1".to_string()),
            ..Default::default()
        };
        let cfg = build_byok_llm_config(&req, None).unwrap();
        assert_eq!(cfg.base_url.as_deref(), Some("https://byok.example.com/v1"));
    }

    #[test]
    fn byok_config_combines_all_overrides_with_retained_server_fields() {
        let server = LlmConfig {
            max_tokens: 500,
            ..LlmConfig::default()
        };
        let req = ScrapeRequest {
            llm_api_key: Some("sk-combo".to_string()),
            llm_provider: Some("anthropic".to_string()),
            llm_model: Some("claude".to_string()),
            base_url: Some("https://combo.example.com".to_string()),
            ..Default::default()
        };
        let cfg = build_byok_llm_config(&req, Some(&server)).unwrap();
        assert_eq!(cfg.api_key, "sk-combo");
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.model, "claude");
        assert_eq!(cfg.base_url.as_deref(), Some("https://combo.example.com"));
        assert_eq!(cfg.max_tokens, 500, "server field not overridden stays");
    }

    // ── formats_include_json / formats_include_summary ──────────────────────

    #[test]
    fn formats_include_json_false_on_empty() {
        assert!(!formats_include_json(&[]));
    }

    #[test]
    fn formats_include_json_true_when_present() {
        assert!(formats_include_json(&[OutputFormat::Json]));
    }

    #[test]
    fn formats_include_summary_false_on_empty() {
        assert!(!formats_include_summary(&[]));
    }

    #[test]
    fn formats_include_summary_true_when_present() {
        assert!(formats_include_summary(&[OutputFormat::Summary]));
    }

    #[test]
    fn formats_include_json_and_summary_are_independent() {
        let both = [
            OutputFormat::Markdown,
            OutputFormat::Json,
            OutputFormat::Summary,
        ];
        assert!(formats_include_json(&both));
        assert!(formats_include_summary(&both));
        let markdown_only = [OutputFormat::Markdown];
        assert!(!formats_include_json(&markdown_only));
        assert!(!formats_include_summary(&markdown_only));
    }

    // ── detect_block_interstitial ────────────────────────────────────────────

    #[test]
    fn detect_interstitial_every_marker_trips_the_generic_message() {
        let markers = [
            "just a moment",
            "attention required",
            "cf-browser-verification",
            "cf-challenge",
            "captcha-delivery.com",
            "datadome captcha",
            "px-captcha",
            "_px3=",
            "_abck=",
            "ak-challenge",
        ];
        for marker in markers {
            let html = format!("<html><body>filler text {marker} more filler</body></html>");
            assert_eq!(
                detect_block_interstitial(&html),
                Some("Blocked by anti-bot protection".to_string()),
                "marker {marker} was not detected"
            );
        }
    }

    #[test]
    fn detect_interstitial_is_case_insensitive() {
        assert_eq!(
            detect_block_interstitial("<html><body>JUST A MOMENT...</body></html>"),
            Some("Blocked by anti-bot protection".to_string())
        );
        assert_eq!(
            detect_block_interstitial("<html><body>Cf-Browser-Verification</body></html>"),
            Some("Blocked by anti-bot protection".to_string())
        );
    }

    #[test]
    fn detect_interstitial_none_on_ordinary_content() {
        let html = "<html><body><article><h1>Weekly digest</h1><p>Nothing scary here, \
            just a regular newsletter about gardening.</p></article></body></html>";
        assert_eq!(detect_block_interstitial(html), None);
    }

    #[test]
    fn detect_interstitial_skips_pages_over_50kb() {
        // "Just a moment" is a real Cloudflare marker, but a 50KB+ page can't be
        // a pure interstitial shell, so the size guard suppresses it.
        let mut html = "<html><body>".to_string();
        html.push_str(&"filler ".repeat(8000)); // > 50_000 bytes
        html.push_str("just a moment</body></html>");
        assert!(html.len() > 50_000);
        assert_eq!(detect_block_interstitial(&html), None);
    }

    #[test]
    fn detect_interstitial_boundary_at_exactly_50kb_still_scans() {
        let filler = "a".repeat(50_000 - "just a moment".len());
        let html = format!("{filler}just a moment");
        assert_eq!(html.len(), 50_000, "fixture must land exactly on the guard");
        assert_eq!(
            detect_block_interstitial(&html),
            Some("Blocked by anti-bot protection".to_string()),
            "the guard is `> 50_000`, so exactly 50_000 bytes must still be scanned"
        );
    }

    #[test]
    fn detect_interstitial_boundary_at_50kb_plus_one_is_skipped() {
        let filler = "a".repeat(50_001 - "just a moment".len());
        let html = format!("{filler}just a moment");
        assert_eq!(html.len(), 50_001);
        assert_eq!(detect_block_interstitial(&html), None);
    }

    #[test]
    fn detect_interstitial_multiple_markers_still_returns_the_generic_message() {
        let html = "<html><body>just a moment, cf-browser-verification, _abck=xyz</body></html>";
        assert_eq!(
            detect_block_interstitial(html),
            Some("Blocked by anti-bot protection".to_string())
        );
    }

    #[test]
    fn detect_interstitial_empty_string() {
        assert_eq!(detect_block_interstitial(""), None);
    }

    // ── looks_like_parked_domain (direct) ────────────────────────────────────

    #[test]
    fn parked_domain_body_at_exactly_the_size_cap_is_still_checked() {
        // Filler is spaces (not word characters) so it neither breaks the
        // trailing `\b` after "sale" nor collides with the regex's left guard.
        let filler = " ".repeat(PARKED_MAX_BODY_BYTES - "arym.com is for sale".len());
        let md = format!("arym.com is for sale{filler}");
        assert_eq!(md.len(), PARKED_MAX_BODY_BYTES);
        assert!(looks_like_parked_domain(&md, "https://arym.com/", None));
    }

    #[test]
    fn parked_domain_body_one_byte_over_the_cap_is_rejected() {
        let filler = "x".repeat(PARKED_MAX_BODY_BYTES - "arym.com is for sale".len() + 1);
        let md = format!("arym.com is for sale{filler}");
        assert_eq!(md.len(), PARKED_MAX_BODY_BYTES + 1);
        assert!(!looks_like_parked_domain(&md, "https://arym.com/", None));
    }

    #[test]
    fn parked_domain_no_usable_host_never_matches() {
        assert!(!looks_like_parked_domain(
            "arym.com is for sale",
            "not a url",
            None
        ));
        assert!(!looks_like_parked_domain(
            "arym.com is for sale",
            "not a url",
            Some("also not a url")
        ));
    }

    #[test]
    fn parked_domain_matches_on_final_url_when_requested_is_malformed() {
        assert!(looks_like_parked_domain(
            "parked-target.com is for sale",
            "not a url",
            Some("https://parked-target.com/")
        ));
    }

    // ── redirect_is_material (additional) ────────────────────────────────────

    #[test]
    fn redirect_material_false_on_malformed_requested_url() {
        assert!(!redirect_is_material("not a url", "https://example.com/"));
    }

    #[test]
    fn redirect_material_false_on_malformed_final_url() {
        assert!(!redirect_is_material(
            "https://example.com/page",
            "not a url"
        ));
    }

    #[test]
    fn redirect_material_false_when_both_urls_are_malformed() {
        assert!(!redirect_is_material("not a url", "also not a url"));
    }

    #[test]
    fn redirect_material_ignores_port_only_change() {
        assert!(!redirect_is_material(
            "https://example.com:8080/x",
            "https://example.com:9090/x"
        ));
    }

    #[test]
    fn redirect_material_ignores_scheme_only_change() {
        assert!(!redirect_is_material(
            "http://example.com/x",
            "https://example.com/x"
        ));
    }

    #[test]
    fn redirect_material_host_comparison_is_case_normalized() {
        // `url::Url` lowercases the host on parse, so mixed-case input on
        // either side must still compare equal.
        assert!(!redirect_is_material(
            "https://Example.COM/x",
            "https://example.com/x"
        ));
    }

    #[test]
    fn redirect_material_subdomain_change_counts_as_a_host_change() {
        assert!(redirect_is_material(
            "https://www.example.com/x",
            "https://example.com/x"
        ));
    }

    #[test]
    fn redirect_material_deep_path_collapsing_to_root_with_trailing_slash() {
        assert!(redirect_is_material(
            "https://example.com/a/b/c",
            "https://example.com/"
        ));
        // Root-to-root (both empty after trim) is not material.
        assert!(!redirect_is_material(
            "https://example.com/",
            "https://example.com"
        ));
    }

    // ── is_cdn_origin_error (additional) ─────────────────────────────────────

    #[test]
    fn cdn_origin_error_far_outside_the_range() {
        assert!(!is_cdn_origin_error(0));
        assert!(!is_cdn_origin_error(u16::MAX));
    }

    // ── is_empty_truncated_render (additional) ───────────────────────────────

    #[test]
    fn empty_truncated_render_true_when_markdown_is_one_of_several_formats() {
        let formats = [
            OutputFormat::Markdown,
            OutputFormat::RawHtml,
            OutputFormat::Links,
        ];
        assert!(is_empty_truncated_render(true, &formats, None));
    }

    #[test]
    fn empty_truncated_render_false_when_markdown_was_never_requested() {
        let formats = [OutputFormat::Json, OutputFormat::Links];
        assert!(!is_empty_truncated_render(true, &formats, None));
    }

    #[test]
    fn empty_truncated_render_true_on_empty_string_markdown() {
        let formats = [OutputFormat::Markdown];
        assert!(is_empty_truncated_render(true, &formats, Some("")));
    }

    #[test]
    fn empty_truncated_render_false_on_a_single_meaningful_character() {
        let formats = [OutputFormat::Markdown];
        assert!(!is_empty_truncated_render(true, &formats, Some("x")));
    }

    // ── escalate_for_quality (additional) ────────────────────────────────────

    #[test]
    fn escalate_for_quality_false_exactly_at_the_5000_byte_boundary() {
        let html = "x".repeat(5000);
        assert!(!escalate_for_quality(
            false,
            true,
            200,
            &html,
            Some("text/html")
        ));
    }

    #[test]
    fn escalate_for_quality_true_one_byte_past_the_boundary() {
        let html = "x".repeat(5001);
        assert!(escalate_for_quality(
            false,
            true,
            200,
            &html,
            Some("text/html")
        ));
    }

    #[test]
    fn escalate_for_quality_false_when_quality_is_not_low() {
        let html = "x".repeat(6000);
        assert!(!escalate_for_quality(false, false, 200, &html, None));
    }

    #[test]
    fn escalate_for_quality_status_is_irrelevant_for_non_json() {
        let html = "x".repeat(6000);
        for status in [200, 403, 429, 500] {
            assert!(escalate_for_quality(
                false,
                true,
                status,
                &html,
                Some("text/plain")
            ));
        }
    }

    // ── derive_target_warning (additional) ───────────────────────────────────

    #[test]
    fn warning_detects_datadome_perimeterx_and_akamai_markers() {
        let cases = [
            "<html><body><script src=\"https://captcha-delivery.com/c.js\"></script></body></html>",
            "<html><body><div class=\"px-captcha\">verify</div></body></html>",
            "<html><body>set-cookie: _abck=1234abcd</body></html>",
        ];
        for html in cases {
            let warning = derive_target_warning(&sample_fetch(200, html));
            assert_eq!(
                warning.as_deref(),
                Some("Blocked by anti-bot protection"),
                "{html}"
            );
        }
    }

    #[test]
    fn warning_prefers_fetch_warning_over_status_text_when_no_block_present() {
        let mut fetch = sample_fetch(500, "<html><body>ordinary error page</body></html>");
        fetch.warning = Some("upstream connection reset".to_string());
        assert_eq!(
            derive_target_warning(&fetch).as_deref(),
            Some("upstream connection reset")
        );
    }

    #[test]
    fn warning_none_on_a_clean_200_with_no_signals() {
        let fetch = sample_fetch(200, "<html><body>hello world</body></html>");
        assert_eq!(derive_target_warning(&fetch), None);
    }

    #[test]
    fn warning_status_text_covers_every_mapped_code() {
        for (code, text) in [
            (401u16, "Unauthorized"),
            (404, "Not Found"),
            (405, "Method Not Allowed"),
            (429, "Too Many Requests"),
            (500, "Internal Server Error"),
            (502, "Bad Gateway"),
            (503, "Service Unavailable"),
        ] {
            let warning =
                derive_target_warning(&sample_fetch(code, "<html><body>plain</body></html>"));
            assert_eq!(warning, Some(format!("Target returned {code} {text}")));
        }
    }

    #[test]
    fn warning_falls_back_to_generic_error_text_for_unmapped_status() {
        let warning = derive_target_warning(&sample_fetch(599, "<html><body>plain</body></html>"));
        assert_eq!(warning.as_deref(), Some("Target returned 599 Error"));
    }

    // ── classify_block (additional vendor arms) ──────────────────────────────

    #[test]
    fn classify_block_perimeterx_marker() {
        let html = "<html><body><script>window._pxAppId = 'PXabc123';</script></body></html>";
        let b = classify_block(
            403,
            Some("text/html"),
            html,
            None,
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("PerimeterX must be flagged");
        assert_eq!(b.vendor, "perimeterx");
    }

    #[test]
    fn classify_block_akamai_marker() {
        let html =
            "<html><body>Pardon Our Interruption while we verify you are human</body></html>";
        let b = classify_block(
            403,
            Some("text/html"),
            html,
            None,
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("Akamai must be flagged");
        assert_eq!(b.vendor, "akamai");
    }

    #[test]
    fn classify_block_kasada_marker() {
        let html = "<html><body><script>KPSDK.scriptStart = KPSDK.now();</script></body></html>";
        let b = classify_block(
            403,
            Some("text/html"),
            html,
            None,
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("Kasada must be flagged");
        assert_eq!(b.vendor, "kasada");
    }

    #[test]
    fn classify_block_sucuri_marker() {
        let html = "<html><body>Sucuri WebSite Firewall blocked this request</body></html>";
        let b = classify_block(
            403,
            Some("text/html"),
            html,
            None,
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("Sucuri must be flagged");
        assert_eq!(b.vendor, "sucuri");
    }

    #[test]
    fn classify_block_rate_limited_on_status_429() {
        let b = classify_block(
            429,
            Some("text/html"),
            "<html><body>slow down</body></html>",
            None,
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("429 must always be flagged as rate limited");
        assert_eq!(b.vendor, "rate_limited");
    }

    #[test]
    fn classify_block_named_vendor_wall_survives_boilerplate_without_markdown() {
        // The invariant the visible-text bar risks. A deny notice rendered inside
        // a site's normal header/nav/footer easily clears WALL_VISIBLE_TEXT_MAX,
        // so the bar must NOT be allowed to overrule a named vendor arm, only the
        // shape/status heuristics (GenericBlock / StructuralFailure).
        let chrome = "site navigation home about contact ".repeat(40); // >600 chars
        for (marker, want_vendor) in [
            ("window._pxAppId = 'PXbd4Ss3Lg';", "perimeterx"),
            ("captcha-delivery.com", "datadome"),
            ("_Incapsula_Resource", "imperva"),
        ] {
            let html =
                format!("<html><body><p>{chrome}</p><script>{marker}</script></body></html>");
            let b = classify_block(
                403,
                Some("text/html"),
                &html,
                None,
                false,
                THRESH,
                "https://example.com/",
                None,
            )
            .unwrap_or_else(|| panic!("{want_vendor} wall must still be flagged"));
            assert_eq!(b.vendor, want_vendor, "marker {marker}");
        }
    }

    #[test]
    fn classify_block_429_still_flagged_without_markdown_on_a_full_page() {
        // `classify`'s 429 arm is unconditional and size-independent by design.
        let chrome = "site navigation home about contact ".repeat(40);
        let html = format!("<html><body><p>{chrome}</p><p>Too many requests</p></body></html>");
        let b = classify_block(
            429,
            Some("text/html"),
            &html,
            None,
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("429 must always be flagged");
        assert_eq!(b.vendor, "rate_limited");
    }

    #[test]
    fn classify_block_403_serving_a_real_page_is_not_a_block_without_markdown() {
        // Regression pin, measured live before the fix against
        // stackoverflow.com/questions/tagged/rust (HTTP 403, serves the real page):
        //   formats:["markdown"] -> success:true, 65,209 chars
        //   formats:["rawHtml"]  -> success:false, anti_bot, empty body
        // `markdown: None` means markdown was never EXTRACTED, not that the page
        // is empty, so a caller asking only for rawHtml/links must not have their
        // page condemned by the status-derived generic bucket.
        let article = format!("<html><body><p>{}</p></body></html>", "word ".repeat(400));
        assert!(
            classify_block(
                403,
                Some("text/html"),
                &article,
                None,
                false,
                THRESH,
                "https://example.com/",
                None,
            )
            .is_none(),
            "a 403 that still served a full article is not a block"
        );
        // ...and the same page WITH markdown was already fine, both before and now.
        assert!(
            classify_block(
                403,
                Some("text/html"),
                &article,
                Some(&"word ".repeat(400)),
                false,
                THRESH,
                "https://example.com/",
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn classify_block_generic_access_denied_on_403() {
        let html = "Access Denied. ".repeat(10); // > EMPTY_CONTENT_THRESHOLD, tier2-eligible
        let b = classify_block(
            403,
            Some("text/html"),
            &html,
            None,
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("Access Denied must be flagged");
        assert_eq!(b.vendor, "generic_block");
    }

    #[test]
    fn classify_block_structural_failure_on_missing_body_tag() {
        let b = classify_block(
            200,
            Some("text/html"),
            "<html><head><title>empty shell</title></head></html>",
            None,
            false,
            THRESH,
            "https://example.com/",
            None,
        )
        .expect("a body-less shell must be flagged");
        assert_eq!(b.vendor, "structural_failure");
    }

    #[test]
    fn classify_block_cf_strong_markers_each_trip_independently() {
        for marker in [
            "cf-challenge-running",
            "cf-browser-verification",
            "__cf_chl_managed_tk__",
        ] {
            let html = format!("<html><body><script>{marker}</script></body></html>");
            let b = classify_block(
                200,
                Some("text/html"),
                &html,
                None,
                false,
                THRESH,
                "https://example.com/",
                None,
            )
            .unwrap_or_else(|| panic!("{marker} must be flagged"));
            assert_eq!(b.vendor, "cloudflare", "{marker}");
        }
    }

    #[test]
    fn classify_block_cloudflare_arm_wins_over_parked_domain_wording() {
        // CF_STRONG_MARKERS is checked before the parked-domain regex, so a
        // page that is BOTH a live CF challenge and happens to name a domain
        // "for sale" must classify as the challenge, not the parking page.
        let html = r#"<html><body><script>window._cf_chl_opt={}</script></body></html>"#;
        let md = "arym.com is for sale";
        let b = classify_block(
            200,
            Some("text/html"),
            html,
            Some(md),
            false,
            THRESH,
            "https://arym.com/",
            None,
        )
        .expect("must be flagged");
        assert_eq!(b.vendor, "cloudflare");
    }

    #[test]
    fn classify_block_threshold_zero_short_circuits_before_the_generic_classifier() {
        // With threshold 0, ANY non-negative markdown length clears the guard,
        // so a body-less shell that would otherwise be a structural failure is
        // never even handed to `crw_extract::antibot::classify`.
        assert!(
            classify_block(
                200,
                Some("text/html"),
                "<html><head></head></html>",
                Some(""),
                false,
                0,
                "https://example.com/",
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn classify_block_pdf_match_is_case_sensitive() {
        // `content_type.contains("pdf")` is case-sensitive, so an uppercase
        // media type does NOT take the early PDF exemption. Proven via the
        // CF_STRONG_MARKERS arm specifically, because it (unlike the generic
        // antibot classifier further down) never consults content_type at
        // all, so it isolates just the PDF-exemption guard being skipped.
        let html = r#"<html><body><script>window._cf_chl_opt={}</script></body></html>"#;
        let b = classify_block(
            200,
            Some("APPLICATION/PDF"),
            html,
            None,
            false,
            THRESH,
            "https://example.com/",
            None,
        );
        assert!(
            b.is_some(),
            "uppercase PDF content-type must not hit the (case-sensitive) PDF skip"
        );
    }

    // ── resolve_request_proxy ────────────────────────────────────────────────

    fn plain_renderer() -> FallbackRenderer {
        FallbackRenderer::new(
            &RendererConfig::default(),
            "crw-test",
            None,
            &StealthConfig::default(),
        )
        .expect("hermetic renderer construction must not fail")
    }

    fn proxy_req(url: &str) -> ScrapeRequest {
        ScrapeRequest {
            url: url.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn resolve_proxy_none_when_nothing_configured() {
        let renderer = plain_renderer();
        let req = proxy_req("https://example.com/");
        let resolved = resolve_request_proxy(&req, &renderer).expect("must not error");
        assert!(resolved.is_none());
    }

    #[test]
    fn resolve_proxy_single_valid_proxy_is_used() {
        let renderer = plain_renderer();
        let req = ScrapeRequest {
            proxy: Some("http://user:pass@proxy.example.com:8080".to_string()),
            ..proxy_req("https://example.com/")
        };
        let resolved = resolve_request_proxy(&req, &renderer)
            .expect("must not error")
            .expect("a proxy was configured");
        assert_eq!(resolved.raw(), "http://user:pass@proxy.example.com:8080");
    }

    #[test]
    fn resolve_proxy_malformed_url_is_invalid_request() {
        let renderer = plain_renderer();
        let req = ScrapeRequest {
            proxy: Some("not a proxy url".to_string()),
            ..proxy_req("https://example.com/")
        };
        match resolve_request_proxy(&req, &renderer) {
            Err(crw_core::error::CrwError::InvalidRequest(_)) => {}
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn resolve_proxy_unsupported_scheme_is_invalid_request() {
        let renderer = plain_renderer();
        let req = ScrapeRequest {
            proxy: Some("ftp://proxy.example.com:21".to_string()),
            ..proxy_req("https://example.com/")
        };
        match resolve_request_proxy(&req, &renderer) {
            Err(crw_core::error::CrwError::InvalidRequest(_)) => {}
            other => panic!("expected InvalidRequest for ftp scheme, got {other:?}"),
        }
    }

    #[test]
    fn resolve_proxy_socks5h_scheme_is_accepted() {
        let renderer = plain_renderer();
        let req = ScrapeRequest {
            proxy: Some("socks5h://proxy.example.com:1080".to_string()),
            ..proxy_req("https://example.com/")
        };
        let resolved = resolve_request_proxy(&req, &renderer).unwrap();
        assert!(resolved.is_some());
    }

    #[test]
    fn resolve_proxy_blank_string_falls_back_to_renderer_default() {
        let renderer = plain_renderer();
        let req = ScrapeRequest {
            proxy: Some("   ".to_string()),
            ..proxy_req("https://example.com/")
        };
        let resolved = resolve_request_proxy(&req, &renderer).expect("must not error");
        assert!(
            resolved.is_none(),
            "a blank proxy must not be treated as BYOP"
        );
    }

    #[test]
    fn resolve_proxy_list_takes_precedence_over_single_proxy() {
        let renderer = plain_renderer();
        let req = ScrapeRequest {
            proxy: Some("http://single.example.com:8080".to_string()),
            proxy_list: vec![
                "http://list-a.example.com:8080".to_string(),
                "http://list-b.example.com:8080".to_string(),
            ],
            ..proxy_req("https://example.com/")
        };
        let resolved = resolve_request_proxy(&req, &renderer).unwrap().unwrap();
        assert_ne!(resolved.raw(), "http://single.example.com:8080");
        assert!(
            resolved.raw() == "http://list-a.example.com:8080"
                || resolved.raw() == "http://list-b.example.com:8080"
        );
    }

    #[test]
    fn resolve_proxy_list_with_one_malformed_entry_errors() {
        let renderer = plain_renderer();
        let req = ScrapeRequest {
            proxy_list: vec![
                "http://good.example.com:8080".to_string(),
                "not a proxy".to_string(),
            ],
            ..proxy_req("https://example.com/")
        };
        match resolve_request_proxy(&req, &renderer) {
            Err(crw_core::error::CrwError::InvalidRequest(_)) => {}
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn resolve_proxy_round_robin_picks_the_only_entry_in_a_singleton_pool() {
        let renderer = plain_renderer();
        let req = ScrapeRequest {
            proxy_list: vec!["http://only.example.com:8080".to_string()],
            proxy_rotation: Some(crw_core::ProxyRotation::RoundRobin),
            ..proxy_req("https://example.com/")
        };
        let resolved = resolve_request_proxy(&req, &renderer).unwrap().unwrap();
        assert_eq!(resolved.raw(), "http://only.example.com:8080");
    }

    #[test]
    fn resolve_proxy_survives_an_unparseable_request_url() {
        // `req.url` feeds only the sticky-per-host key derivation, which
        // degrades to `None` on a parse failure — it must not abort proxy
        // resolution.
        let renderer = plain_renderer();
        let req = ScrapeRequest {
            proxy: Some("http://proxy.example.com:8080".to_string()),
            ..proxy_req("not a url at all")
        };
        let resolved = resolve_request_proxy(&req, &renderer).unwrap();
        assert!(resolved.is_some());
    }
}
