use crw_core::Deadline;
use crw_core::config::AppConfig;
use crw_core::error::{CrwError, CrwResult};
use crw_core::types::{
    CrawlRequest, CrawlState, CrawlStatus, RequestedRenderer, ScrapeRequest,
    resolve_pinned_renderer, resolve_render_js,
};
use crw_crawl::crawl::{CrawlOptions, failed_page, run_crawl};
use crw_crawl::single::scrape_url;
use crw_renderer::FallbackRenderer;
use crw_search::SearxngClient;
use futures::stream::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{RwLock, watch};
use uuid::Uuid;

/// Validate that a request's pinned renderer is available before accepting
/// the job. Returns `InvalidRequest` (→ HTTP 400) when the named renderer is
/// not in the configured pool. Skipped when `renderJs:false` is set, since
/// HTTP-only ignores the pin.
///
/// We surface this explicitly (rather than silently falling back to "auto")
/// so users get clear feedback when they ask for a renderer the operator
/// hasn't configured. Sites that fail under one renderer often need a
/// specific other one — silent fallback would leave callers wondering why
/// "chrome" gave them the same broken result as "auto".
pub(crate) fn validate_renderer_pin(
    renderer: Option<RequestedRenderer>,
    render_js: Option<bool>,
    state: &AppState,
) -> CrwResult<()> {
    let Some(name) = resolve_pinned_renderer(renderer) else {
        return Ok(());
    };

    // Mirror the fetch-path resolution at `crw-crawl/src/single.rs` so
    // validation is consistent with what the actual request does. "Pinned
    // implies JS": when a BROWSER renderer is pinned and the request omits
    // `renderJs`, force the request to JS=true so a `render_js_default=false`
    // server config doesn't silently send the request through HTTP-only.
    // The impersonated-http tier never executes JS
    // (`RequestedRenderer::implies_js`), so it takes neither the coercion nor
    // the HTTP-only skip below: it is validated regardless of the resolved
    // renderJs, against `available_renderer_names()` which lists it when the
    // tier is present.
    let pin_implies_js = renderer.is_some_and(|r| r.implies_js());
    let effective_request = if render_js.is_none() && pin_implies_js {
        Some(true)
    } else {
        render_js
    };
    let effective_render_js =
        resolve_render_js(effective_request, state.config.renderer.render_js_default);

    if effective_render_js == Some(false) && pin_implies_js {
        return Ok(());
    }

    let available = state.renderer.available_renderer_names();
    if !available.contains(&name) {
        return Err(CrwError::InvalidRequest(format!(
            "renderer '{}' not available; configured renderers: [{}]. \
             Update server config or omit the 'renderer' field.",
            name,
            available.join(", ")
        )));
    }
    Ok(())
}

/// Crawl-specific wrapper around [`validate_renderer_pin`]. Also mirrors the
/// scrape surface's `validate_scrape_template` contradiction the crawl path
/// has no equivalent of: an impersonated-http pin never executes JS, so an
/// explicit `renderJs: true` on a crawl is rejected exactly like the same
/// body on `/v1/scrape` instead of being silently coerced away.
pub(crate) fn validate_crawl_renderer(req: &CrawlRequest, state: &AppState) -> CrwResult<()> {
    if req.renderer == Some(RequestedRenderer::ImpersonatedHttp) && req.render_js == Some(true) {
        return Err(CrwError::InvalidRequest(
            "renderer 'impersonated-http' never executes JS; remove renderJs:true (or omit it)"
                .into(),
        ));
    }
    validate_renderer_pin(req.renderer, req.render_js, state)
}

/// Tracks a crawl job receiver + creation time for TTL cleanup.
pub struct CrawlJob {
    pub rx: watch::Receiver<CrawlState>,
    /// Sender kept alongside the receiver so cancel handlers can flip the
    /// job to a terminal `Cancelled` state after aborting the task.
    pub tx: watch::Sender<CrawlState>,
    pub created_at: Instant,
    /// Handle to abort the crawl task.
    pub abort_handle: Option<tokio::task::AbortHandle>,
}

/// RAII guard that inc/decrements the `crw_batch_pipelines_inflight` gauge for
/// the lifetime of one in-flight batch URL-pipeline.
struct InflightGuard;
impl InflightGuard {
    fn new() -> Self {
        crw_core::metrics::metrics().batch_pipelines_inflight.inc();
        InflightGuard
    }
}
impl Drop for InflightGuard {
    fn drop(&mut self) {
        crw_core::metrics::metrics().batch_pipelines_inflight.dec();
    }
}

/// Maximum number of concurrent crawl jobs.
const MAX_CONCURRENT_CRAWLS: usize = 10;
/// Interval between expired crawl job cleanup runs.
const JOB_CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

/// Canonical lifecycle of an async extract job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractStatus {
    Processing,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl ExtractStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ExtractStatus::Processing => "processing",
            ExtractStatus::Cancelling => "cancelling",
            ExtractStatus::Completed => "completed",
            ExtractStatus::Failed => "failed",
            ExtractStatus::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            ExtractStatus::Completed | ExtractStatus::Failed | ExtractStatus::Cancelled
        )
    }
}

/// One URL's extraction outcome. Powers the native `/v1/extract` per-URL array
/// contract (`results:[{url,status,data,error,llmUsage}]`), which sidesteps the
/// FC-legacy last-write-wins merge. `llm_usage` lets the SaaS settle real cost.
///
/// The `basis*` / `llm_input_hash` fields carry per-field evidence and are
/// populated only when the request set `basis: true`. They stay per-URL on
/// purpose: a citation is only meaningful next to the document it came from, so
/// merging them into the FC-legacy flattened `data` object would destroy the
/// attribution.
#[derive(Debug, Clone)]
pub struct UrlResult {
    pub url: String,
    pub status: ExtractStatus,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
    pub llm_usage: Option<crw_core::types::LlmUsage>,
    pub basis: Option<Vec<crw_core::evidence::Basis>>,
    pub basis_warnings: Vec<crw_core::evidence::BasisWarning>,
    pub llm_input_hash: Option<String>,
}

/// A URL prepared by the handler for the worker, in original request order.
/// `preflight_error: Some(..)` marks a parse/SSRF failure that must surface as a
/// `failed` result without being fetched (native contract: no silent drops).
#[derive(Debug, Clone)]
pub struct PreparedUrl {
    pub url: String,
    pub preflight_error: Option<String>,
}

/// An async extract job record. `data` is the single merged JSON object (the
/// scrape's `json` field unioned across URLs), preserved for the FC-legacy
/// `GET /v2/extract/{id}` `data` shape. `per_url` is the native per-URL array
/// (`GET /v1/extract/{id}`), in original request order.
#[derive(Debug, Clone)]
pub struct ExtractRecord {
    pub status: ExtractStatus,
    pub data: Option<serde_json::Value>,
    pub per_url: Vec<UrlResult>,
    pub tokens_used: u32,
    pub credits_used: u32,
    pub error: Option<String>,
    pub created_at: Instant,
    /// Absolute wall-clock expiry captured once at admission. Serializers use
    /// this persisted value so repeated lifecycle envelopes never drift.
    pub expires_at: SystemTime,
    /// The one URL currently dispatched by the sequential worker. Cancellation
    /// cannot cross its terminal barrier until this slot has persisted.
    pub claimed_index: Option<usize>,
}

impl ExtractRecord {
    fn is_expired(&self, ttl: Duration) -> bool {
        self.created_at.elapsed() >= ttl
    }

    /// Complete the cancellation barrier. Call only while holding the extract
    /// jobs write lock and after observing that no URL remains claimed.
    fn finish_cancellation(&mut self) {
        if self.status != ExtractStatus::Cancelling || self.claimed_index.is_some() {
            return;
        }
        let mut cancelled_any = false;
        for result in &mut self.per_url {
            if result.status == ExtractStatus::Processing {
                result.status = ExtractStatus::Cancelled;
                result.data = None;
                result.error = None;
                result.llm_usage = None;
                result.basis = None;
                result.basis_warnings.clear();
                result.llm_input_hash = None;
                cancelled_any = true;
            }
        }
        if cancelled_any {
            // A genuine cancel: at least one in-flight URL was actually stopped.
            self.status = ExtractStatus::Cancelled;
        } else {
            // Every URL had already reached a terminal state before the cancel
            // landed. Reporting "cancelled" would contradict the per-URL results
            // (all completed/failed with real data), so settle it as the
            // naturally finished job it actually is.
            self.complete_from_outcomes();
        }
    }

    /// Set the terminal job status from the per-URL outcomes of a job that ran
    /// to the end: completed if any URL succeeded, otherwise failed, with the
    /// one-credit floor a naturally finished job carries.
    fn complete_from_outcomes(&mut self) {
        let any_ok = self
            .per_url
            .iter()
            .any(|result| result.status == ExtractStatus::Completed);
        if any_ok {
            self.status = ExtractStatus::Completed;
            self.data
                .get_or_insert_with(|| serde_json::Value::Object(Default::default()));
        } else {
            self.status = ExtractStatus::Failed;
            self.error = self
                .per_url
                .iter()
                .rev()
                .find_map(|result| result.error.clone());
        }
        // Preserve the existing one-credit floor for a naturally finished
        // all-failed job. Cancelled jobs retain only measured usage.
        self.credits_used = self.credits_used.max(1);
    }

    /// Commit the worker's final job-level write. DELETE races this method on
    /// the same map lock, so exactly one transition can win and terminal state
    /// is never rewritten by the loser.
    fn finish_processing(&mut self) {
        if self.status != ExtractStatus::Processing {
            if self.status == ExtractStatus::Cancelling {
                self.finish_cancellation();
            }
            return;
        }

        self.complete_from_outcomes();
    }
}

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub renderer: Arc<FallbackRenderer>,
    pub crawl_jobs: Arc<RwLock<HashMap<Uuid, CrawlJob>>>,
    /// `/v2/extract` jobs. Separate from `crawl_jobs` because an extract result
    /// is a single merged JSON object, not a `Vec<ScrapeData>`.
    pub extract_jobs: Arc<RwLock<HashMap<Uuid, ExtractRecord>>>,
    pub crawl_semaphore: Arc<tokio::sync::Semaphore>,
    /// Process-wide cap on the total in-flight `/v2/batch/scrape` URL-pipelines
    /// across all batch-scrape jobs (aggregate bound so `N jobs × width` can't
    /// explode). Targets batch scrape specifically because that's the only wide
    /// fan-out: crawl is BFS-sequential and `/v2/extract` scrapes one URL at a
    /// time, both already bounded by the `crawl_semaphore` job cap. `None` =
    /// unbounded (config `max_aggregate_batch_pipelines = 0`/absent). Acquired
    /// as the first op in each batch URL future, before fetch.
    pub batch_pipeline_sem: Option<Arc<tokio::sync::Semaphore>>,
    /// SearXNG client. `None` when `[search].search_backend_url` is unset, in which
    /// case `/v1/search` returns a clear `search_disabled` error.
    pub searxng: Option<Arc<SearxngClient>>,
    /// Server-wide default /map URL filter. `None` disables the filter
    /// entirely (legacy behaviour). Per-request overrides may swap or
    /// extend this at handler time.
    pub url_filter: Option<Arc<crw_crawl::url_filter::UrlFilterCfg>>,
}

impl AppState {
    pub fn new(config: AppConfig) -> CrwResult<Self> {
        // Build the proxy rotator from config (list takes precedence over the
        // single `proxy`). When present, it owns ALL proxy routing (HTTP pool +
        // per-request CDP proxyServer), so `new()` gets `proxy = None` and the
        // rotator is attached via `with_proxy_rotator`. An invalid proxy URL is
        // a hard startup error — never a silent direct-connection fallback.
        let proxy_rotator = crw_core::ProxyRotator::build(
            &config.crawler.proxy_list,
            config.crawler.proxy.as_deref(),
            config.crawler.proxy_rotation,
        )
        .map_err(CrwError::ConfigError)?
        .map(Arc::new);
        let renderer = FallbackRenderer::new(
            &config.renderer,
            &config.crawler.user_agent,
            None,
            &config.crawler.stealth,
        )?
        .with_proxy_rotator(proxy_rotator)?
        .with_host_limits(
            config.crawler.requests_per_second,
            config.crawler.per_host_max_concurrent,
            config.crawler.per_host_interactive_reserve,
        );

        let searxng = if config.search.enabled
            && let Some(url) = config.search.resolve_backend_url()
        {
            // Dedicated reqwest client for SearXNG so its connection pool is
            // hot and isolated from the renderer / scrape paths. SearXNG runs
            // on the same docker network in the bundled compose so a 5s
            // connect_timeout is generous.
            let http = reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .build()
                .map_err(|e| {
                    CrwError::Internal(format!("failed to build SearXNG http client: {e}"))
                })?;
            let timeout = Duration::from_millis(config.search.timeout_ms);
            Some(Arc::new(SearxngClient::new(Arc::new(http), url, timeout)))
        } else {
            None
        };

        let url_filter_cfg =
            crw_crawl::url_filter::UrlFilterCfg::from_map_config(&config.map.url_filter);
        // One-shot snapshot of how many rules the filter knows about. Helps
        // operators confirm at boot that the deny-lists actually loaded.
        let m = crw_core::metrics::metrics();
        m.map_filter_rules_loaded
            .with_label_values(&["action"])
            .inc_by(
                (crw_crawl::url_filter_data::DEFAULT_ACTION_PARAMS.len()
                    + url_filter_cfg.action_params.len()) as u64,
            );
        m.map_filter_rules_loaded
            .with_label_values(&["tracking"])
            .inc_by(
                (crw_crawl::url_filter_data::DEFAULT_TRACKING_PARAMS.len()
                    + url_filter_cfg.tracking_params.len()) as u64,
            );
        m.map_filter_rules_loaded
            .with_label_values(&["preserve"])
            .inc_by(
                (crw_crawl::url_filter_data::ALWAYS_PRESERVE.len()
                    + url_filter_cfg.preserve_params.len()) as u64,
            );
        m.map_filter_rules_loaded
            .with_label_values(&["host_override"])
            .inc_by(url_filter_cfg.host_overrides.len() as u64);
        let url_filter = Some(Arc::new(url_filter_cfg));

        // Install the process-wide reserved-lane limits (extract / PDF / LLM)
        // HERE — inside `AppState::new` — so every entry point that builds an
        // AppState (the `crw-server` binary AND `crw serve` / embedded CLI) gets
        // the configured concurrency + reservations, not just the fallbacks.
        // All three are idempotent (first-call-wins).
        let extract_total = config.extraction.max_concurrent_extracts;
        crw_crawl::extract_pool::configure_extract_limit(
            extract_total,
            crw_core::config::resolve_interactive_reserve(
                config.extraction.reserved_interactive_extracts,
                extract_total,
            ),
        );
        crw_crawl::pdf::configure_limits(&config.document);
        if let Some(llm) = &config.extraction.llm {
            crw_extract::llm_gate::configure_llm_limits(
                llm.max_concurrency,
                crw_core::config::resolve_interactive_reserve(
                    llm.reserved_interactive_llm,
                    llm.max_concurrency,
                ),
            );
        }

        // `0`/absent = unbounded aggregate (no cap); any n>0 bounds total
        // in-flight batch URL-pipelines process-wide.
        let batch_pipeline_sem = match config.crawler.max_aggregate_batch_pipelines {
            0 => None,
            n => Some(Arc::new(tokio::sync::Semaphore::new(n))),
        };

        let state = Self {
            config: Arc::new(config),
            renderer: Arc::new(renderer),
            crawl_jobs: Arc::new(RwLock::new(HashMap::new())),
            extract_jobs: Arc::new(RwLock::new(HashMap::new())),
            crawl_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CRAWLS)),
            batch_pipeline_sem,
            searxng,
            url_filter,
        };

        // Wrap the not-yet-returned state in a block to keep the Ok() shape at the end.
        // Spawn background job cleanup task.
        let cleanup_state = state.clone();
        tokio::spawn(async move {
            let ttl = Duration::from_secs(cleanup_state.config.crawler.job_ttl_secs);
            loop {
                tokio::time::sleep(JOB_CLEANUP_INTERVAL).await;
                let mut jobs = cleanup_state.crawl_jobs.write().await;
                let before = jobs.len();
                jobs.retain(|_id, job| {
                    let is_done = matches!(
                        job.rx.borrow().status,
                        CrawlStatus::Completed | CrawlStatus::Failed | CrawlStatus::Cancelled
                    );
                    // Keep if not done, or if done but within TTL.
                    !is_done || job.created_at.elapsed() < ttl
                });
                let removed = before - jobs.len();
                if removed > 0 {
                    tracing::info!(
                        removed,
                        remaining = jobs.len(),
                        "Cleaned up expired crawl jobs"
                    );
                }
                drop(jobs);

                // TTL is authoritative for every extract lifecycle state. A
                // stalled processing/cancelling job must not live forever.
                cleanup_state.prune_expired_extract_jobs(ttl).await;
            }
        });

        Ok(state)
    }

    /// Start a new crawl job and return its UUID.
    /// Spawns a background task that acquires the crawl semaphore before running.
    pub async fn start_crawl_job(&self, req: CrawlRequest) -> Uuid {
        let id = Uuid::new_v4();
        let initial = CrawlState {
            id,
            success: true,
            status: CrawlStatus::InProgress,
            total: 0,
            completed: 0,
            blocked: 0,
            data: vec![],
            error: None,
        };

        let (tx, rx) = watch::channel(initial);

        {
            let mut jobs = self.crawl_jobs.write().await;
            jobs.insert(
                id,
                CrawlJob {
                    rx,
                    tx: tx.clone(),
                    created_at: Instant::now(),
                    abort_handle: None,
                },
            );
        }

        let renderer = self.renderer.clone();
        let max_concurrency = self.config.crawler.max_concurrency;
        let respect_robots = self.config.crawler.respect_robots_txt;
        let rps = self.config.crawler.requests_per_second;
        let user_agent = self.config.crawler.user_agent.clone();
        let crawl_semaphore = self.crawl_semaphore.clone();
        let llm_config = self.config.extraction.llm.clone();
        let proxy = self.config.crawler.proxy.clone();
        let jitter_factor = self.config.crawler.stealth.jitter_factor;
        let deadline_ms_per_page = self.config.effective_deadline_ms(None, req.wait_for);
        let per_host_max_concurrent = self.config.crawler.per_host_max_concurrent;
        let normalize_tables = self.config.extraction.normalize_tables;
        let only_main_content_default = self.config.extraction.only_main_content;
        let http_retry_threshold_bytes = self.config.extraction.http_retry_threshold_bytes;

        let handle = tokio::spawn(async move {
            let _permit = match crawl_semaphore.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    let _ = tx.send(CrawlState {
                        id,
                        success: false,
                        status: CrawlStatus::Failed,
                        total: 0,
                        completed: 0,
                        blocked: 0,
                        data: vec![],
                        error: Some("Server is overloaded, try again later".into()),
                    });
                    return;
                }
            };
            // Crawl pages are `Batch` traffic (same reserved-lane treatment as
            // batch scrape). Scoped inside the job's spawned task so the
            // task-local reaches every per-page fetch/extract; a handler-level
            // scope would be lost across this `tokio::spawn`.
            crw_core::REQUEST_CLASS
                .scope(crw_core::ScrapeClass::Batch, async {
                    run_crawl(CrawlOptions {
                        id,
                        req,
                        renderer,
                        max_concurrency,
                        respect_robots,
                        requests_per_second: rps,
                        user_agent: &user_agent,
                        state_tx: tx,
                        llm_config: llm_config.as_ref(),
                        proxy,
                        jitter_factor,
                        deadline_ms_per_page,
                        per_host_max_concurrent,
                        normalize_tables,
                        only_main_content_default,
                        http_retry_threshold_bytes,
                    })
                    .await;
                })
                .await;
        });

        // Store the abort handle so the job can be cancelled via DELETE.
        {
            let mut jobs = self.crawl_jobs.write().await;
            if let Some(job) = jobs.get_mut(&id) {
                job.abort_handle = Some(handle.abort_handle());
            }
        }

        id
    }

    /// Start a `/v2/batch/scrape` job over an explicit URL list and return its
    /// UUID. Reuses the crawl-job machinery (`crawl_jobs` + `CrawlState`) but
    /// scrapes the given URLs directly — no link discovery, no same-origin
    /// filtering, no dedup; input order is recoverable via `metadata.sourceURL`.
    pub async fn start_batch_job(
        &self,
        urls: Vec<String>,
        template: ScrapeRequest,
        max_concurrency_override: Option<usize>,
    ) -> Uuid {
        let id = Uuid::new_v4();
        let total = urls.len() as u32;
        let (tx, rx) = watch::channel(CrawlState {
            id,
            success: true,
            status: CrawlStatus::InProgress,
            total,
            completed: 0,
            blocked: 0,
            data: vec![],
            error: None,
        });
        {
            let mut jobs = self.crawl_jobs.write().await;
            jobs.insert(
                id,
                CrawlJob {
                    rx,
                    tx: tx.clone(),
                    created_at: Instant::now(),
                    abort_handle: None,
                },
            );
        }

        let renderer = self.renderer.clone();
        let crawl_semaphore = self.crawl_semaphore.clone();
        let batch_pipeline_sem = self.batch_pipeline_sem.clone();
        let config = self.config.clone();
        // Per-job OUTER pipeline width: the SaaS-injected (plan-scaled)
        // `maxConcurrency`, or `max_concurrency` when absent. BOTH paths are
        // clamped to `[1, max_batch_concurrency]` so a batch job never exceeds
        // the ceiling regardless of source (wire value never trusted).
        let width_ceiling = config.crawler.max_batch_concurrency.max(1);
        let max_concurrency = max_concurrency_override
            .unwrap_or(config.crawler.max_concurrency)
            .clamp(1, width_ceiling);

        let handle = tokio::spawn(async move {
            let _permit = match crawl_semaphore.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    let _ = tx.send(CrawlState {
                        id,
                        success: false,
                        status: CrawlStatus::Failed,
                        total,
                        completed: 0,
                        blocked: 0,
                        data: vec![],
                        error: Some("Server is overloaded, try again later".into()),
                    });
                    return;
                }
            };

            if total == 0 {
                let _ = tx.send(CrawlState {
                    id,
                    success: true,
                    status: CrawlStatus::Completed,
                    total: 0,
                    completed: 0,
                    blocked: 0,
                    data: vec![],
                    error: None,
                });
                return;
            }

            let user_agent = config.crawler.user_agent.clone();
            let default_stealth =
                config.crawler.stealth.enabled && config.crawler.stealth.inject_headers;
            let render_js_default = config.renderer.render_js_default;
            let deadline_ms = config.effective_deadline_ms(template.deadline_ms, template.wait_for);

            let reqs: Vec<ScrapeRequest> = urls
                .into_iter()
                .map(|u| {
                    let mut r = template.clone();
                    r.url = u;
                    r
                })
                .collect();

            // Stamp every URL in this job as `Batch` traffic. The scope wraps the
            // whole `for_each_concurrent` stream; that combinator polls its
            // futures cooperatively within THIS task (no `tokio::spawn` per URL),
            // so the task-local propagates to each per-URL `scrape_url` and on to
            // the reserved lanes it reads. Scoped here (inside the job's spawned
            // task), not at the handler, because the task-local would be lost
            // across this job's `tokio::spawn`.
            crw_core::REQUEST_CLASS
                .scope(crw_core::ScrapeClass::Batch, async move {
                    futures::stream::iter(reqs)
                        .for_each_concurrent(max_concurrency, |req| {
                            let renderer = renderer.clone();
                            let config = config.clone();
                            let user_agent = user_agent.clone();
                            let tx = tx.clone();
                            let batch_pipeline_sem = batch_pipeline_sem.clone();
                            async move {
                                // Aggregate cap: acquire a process-wide pipeline
                                // permit BEFORE fetching so `N jobs × width` can't
                                // explode. `None` = unbounded. Held for this URL's
                                // whole lifetime.
                                let _pipeline_permit = match &batch_pipeline_sem {
                                    Some(sem) => sem.acquire().await.ok(),
                                    None => None,
                                };
                                // In-flight batch-pipeline gauge (RAII inc/dec).
                                let _inflight = InflightGuard::new();
                                let deadline = Deadline::from_request_ms(deadline_ms);
                                let scraped = scrape_url(
                                    &req,
                                    &renderer,
                                    config.extraction.llm.as_ref(),
                                    &config.extraction,
                                    &user_agent,
                                    default_stealth,
                                    render_js_default,
                                    deadline,
                                )
                                .await;
                                // Mutate the shared status in place — push one document
                                // and bump the counter without cloning the whole
                                // accumulated Vec on every completion (avoids O(n^2)
                                // copying on large batches). A failed scrape still
                                // advances `completed`.
                                tx.send_modify(|st| {
                                    let mut d = scraped.unwrap_or_else(|err| {
                                        failed_page(&req.url, 0, err.to_string())
                                    });
                                    // `scrape_url` stamps the verdict but this
                                    // path used to push it through untouched,
                                    // so a wall shipped as an ordinary batch
                                    // document (and `/v2`'s adapter drops
                                    // `block`, hiding it completely). Clear the
                                    // shell and count it, exactly as the single
                                    // scrape route does.
                                    let is_wall = d.block.is_some();
                                    if !is_wall && let Some(reason) = d.http_error() {
                                        d.block = Some(crw_core::types::BlockOutcome {
                                            vendor: crw_core::types::HTTP_ERROR_VENDOR.to_string(),
                                            reason,
                                        });
                                    }
                                    if d.block.is_some() {
                                        // Same split as `/v1/scrape` and the crawl
                                        // loop: a wall loses its shell, an origin
                                        // error page stays readable.
                                        if is_wall {
                                            d.clear_body();
                                        }
                                        st.blocked += 1;
                                    }
                                    st.data.push(d);
                                    st.completed += 1;
                                    // Only flip to Completed from InProgress — never
                                    // overwrite a terminal Cancelled set by DELETE.
                                    if st.completed >= total && st.status == CrawlStatus::InProgress
                                    {
                                        st.status = CrawlStatus::Completed;
                                    }
                                });
                            }
                        })
                        .await;
                })
                .await;
        });

        {
            let mut jobs = self.crawl_jobs.write().await;
            if let Some(job) = jobs.get_mut(&id) {
                job.abort_handle = Some(handle.abort_handle());
            }
        }

        id
    }

    /// Start an async extract job. Each entry is scraped with `formats:[json]` +
    /// the shared template; per-URL `json` objects are both (a) merged into one
    /// object for the FC-legacy `data` shape and (b) kept as an ordered per-URL
    /// array for the native `/v1/extract` contract. `entries` is in original
    /// request order and may include preflight-failed URLs (surfaced as `failed`
    /// results without being fetched).
    pub async fn start_extract_job(
        &self,
        entries: Vec<PreparedUrl>,
        template: ScrapeRequest,
    ) -> Uuid {
        let id = Uuid::new_v4();
        // Seed the fixed-cardinality result array before the worker can run.
        // Preflight failures are already final; valid URLs remain processing
        // until claimed and persisted, or are converted to cancelled at the
        // cancellation barrier.
        let per_url = entries
            .iter()
            .map(|entry| UrlResult {
                url: entry.url.clone(),
                status: if entry.preflight_error.is_some() {
                    ExtractStatus::Failed
                } else {
                    ExtractStatus::Processing
                },
                data: None,
                error: entry.preflight_error.clone(),
                llm_usage: None,
                basis: None,
                basis_warnings: Vec::new(),
                llm_input_hash: None,
            })
            .collect();
        {
            let mut jobs = self.extract_jobs.write().await;
            let created_at = Instant::now();
            let wall_now = SystemTime::now();
            let expires_at =
                wall_now.checked_add(Duration::from_secs(self.config.crawler.job_ttl_secs));
            jobs.insert(
                id,
                ExtractRecord {
                    status: ExtractStatus::Processing,
                    data: None,
                    per_url,
                    tokens_used: 0,
                    credits_used: 0,
                    error: None,
                    created_at,
                    expires_at: expires_at.unwrap_or(wall_now),
                    claimed_index: None,
                },
            );
        }

        let renderer = self.renderer.clone();
        let config = self.config.clone();
        let extract_jobs = self.extract_jobs.clone();
        let finalizer = self.clone();

        tokio::spawn(async move {
            // `/v2/extract` is a multi-URL background job — `Batch` traffic, so its
            // scrapes use the batch lanes and don't consume the interactive reserve.
            // Scoped inside the spawned task (a handler-level scope is lost across
            // `tokio::spawn`).
            crw_core::REQUEST_CLASS
                .scope(crw_core::ScrapeClass::Batch, async move {
                    let user_agent = config.crawler.user_agent.clone();
                    let default_stealth =
                        config.crawler.stealth.enabled && config.crawler.stealth.inject_headers;
                    let render_js_default = config.renderer.render_js_default;
                    let deadline_ms =
                        config.effective_deadline_ms(template.deadline_ms, template.wait_for);

                    for (index, entry) in entries.into_iter().enumerate() {
                        // Preflight failures were persisted at admission and
                        // never count as dispatched work.
                        if entry.preflight_error.is_some() {
                            continue;
                        }

                        // Claim exactly one slot while holding the same state
                        // lock DELETE uses. Once cancelling is visible no new
                        // URL can cross this point.
                        {
                            let mut jobs = extract_jobs.write().await;
                            let Some(rec) = jobs.get_mut(&id) else {
                                return;
                            };
                            match rec.status {
                                ExtractStatus::Processing => {
                                    if rec.per_url[index].status != ExtractStatus::Processing {
                                        continue;
                                    }
                                    rec.claimed_index = Some(index);
                                }
                                ExtractStatus::Cancelling => {
                                    rec.finish_cancellation();
                                    return;
                                }
                                ExtractStatus::Completed
                                | ExtractStatus::Failed
                                | ExtractStatus::Cancelled => return,
                            }
                        }

                        let mut req = template.clone();
                        req.url = entry.url.clone();
                        let deadline = Deadline::from_request_ms(deadline_ms);
                        let (result, merged_fields, tokens, credits) = match scrape_url(
                            &req,
                            &renderer,
                            config.extraction.llm.as_ref(),
                            &config.extraction,
                            &user_agent,
                            default_stealth,
                            render_js_default,
                            deadline,
                        )
                        .await
                        {
                            // A wall or an origin error page is not a completed
                            // extraction: it used to be marked Completed, charged,
                            // and to have burned the LLM call on the wall's text.
                            Ok(d) if d.block.is_some() || d.http_error().is_some() => {
                                let msg = d
                                    .block
                                    .as_ref()
                                    .map(|b| b.message())
                                    .or_else(|| d.http_error())
                                    .unwrap_or_else(|| "Blocked".into());
                                (
                                    UrlResult {
                                        url: entry.url,
                                        status: ExtractStatus::Failed,
                                        data: None,
                                        error: Some(msg),
                                        llm_usage: None,
                                        basis: None,
                                        basis_warnings: Vec::new(),
                                        llm_input_hash: None,
                                    },
                                    None,
                                    0,
                                    0,
                                )
                            }
                            Ok(d) => {
                                let merged_fields = match &d.json {
                                    Some(serde_json::Value::Object(obj)) => Some(obj.clone()),
                                    _ => None,
                                };
                                let tokens =
                                    d.llm_usage.as_ref().map_or(0, |usage| usage.total_tokens);
                                let credits = if d.credit_cost == 0 { 1 } else { d.credit_cost };
                                (
                                    UrlResult {
                                        url: entry.url,
                                        status: ExtractStatus::Completed,
                                        data: d.json,
                                        error: None,
                                        llm_usage: d.llm_usage,
                                        basis: d.basis,
                                        basis_warnings: d.basis_warnings,
                                        llm_input_hash: d.llm_input_hash,
                                    },
                                    merged_fields,
                                    tokens,
                                    credits,
                                )
                            }
                            Err(e) => {
                                let msg = e.to_string();
                                (
                                    UrlResult {
                                        url: entry.url,
                                        status: ExtractStatus::Failed,
                                        data: None,
                                        error: Some(msg),
                                        llm_usage: None,
                                        basis: None,
                                        basis_warnings: Vec::new(),
                                        llm_input_hash: None,
                                    },
                                    None,
                                    0,
                                    0,
                                )
                            }
                        };

                        // Persist the completed claimed slot and cumulative
                        // measured usage before dispatching anything else.
                        let mut jobs = extract_jobs.write().await;
                        let Some(rec) = jobs.get_mut(&id) else {
                            return;
                        };
                        if rec.status.is_terminal() || rec.claimed_index != Some(index) {
                            return;
                        }
                        rec.per_url[index] = result;
                        if let Some(fields) = merged_fields {
                            let merged = rec.data.get_or_insert_with(|| {
                                serde_json::Value::Object(Default::default())
                            });
                            if let serde_json::Value::Object(merged) = merged {
                                merged.extend(fields);
                            }
                        }
                        rec.tokens_used = rec.tokens_used.saturating_add(tokens);
                        rec.credits_used = rec.credits_used.saturating_add(credits);
                        rec.claimed_index = None;
                        if rec.status == ExtractStatus::Cancelling {
                            rec.finish_cancellation();
                            return;
                        }
                    }

                    finalizer.finalize_extract_job(id).await;
                })
                .await;
        });

        id
    }

    /// Request cancellation and return the persisted canonical state. Repeated
    /// calls are idempotent; terminal jobs are never rewritten.
    pub async fn cancel_extract_job(&self, id: Uuid) -> CrwResult<ExtractRecord> {
        let mut jobs = self.extract_jobs.write().await;
        let ttl = Duration::from_secs(self.config.crawler.job_ttl_secs);
        if jobs.get(&id).is_some_and(|rec| rec.is_expired(ttl)) {
            jobs.remove(&id);
            return Err(CrwError::NotFound(format!("Extract job {id} not found")));
        }
        let rec = jobs
            .get_mut(&id)
            .ok_or_else(|| CrwError::NotFound(format!("Extract job {id} not found")))?;
        if rec.status == ExtractStatus::Processing {
            rec.status = ExtractStatus::Cancelling;
        }
        if rec.status == ExtractStatus::Cancelling {
            rec.finish_cancellation();
        }
        Ok(rec.clone())
    }

    /// TTL-aware canonical lookup shared by v1, v2, and MCP handlers. A write
    /// lock makes expiry observation and removal one atomic operation.
    pub async fn get_extract_job(&self, id: Uuid) -> CrwResult<ExtractRecord> {
        let mut jobs = self.extract_jobs.write().await;
        let ttl = Duration::from_secs(self.config.crawler.job_ttl_secs);
        if jobs.get(&id).is_some_and(|rec| rec.is_expired(ttl)) {
            jobs.remove(&id);
            return Err(CrwError::NotFound(format!("Extract job {id} not found")));
        }
        jobs.get(&id)
            .cloned()
            .ok_or_else(|| CrwError::NotFound(format!("Extract job {id} not found")))
    }

    async fn finalize_extract_job(&self, id: Uuid) {
        let mut jobs = self.extract_jobs.write().await;
        if let Some(rec) = jobs.get_mut(&id) {
            rec.finish_processing();
        }
    }

    async fn prune_expired_extract_jobs(&self, ttl: Duration) -> usize {
        let mut jobs = self.extract_jobs.write().await;
        let before = jobs.len();
        jobs.retain(|_id, rec| !rec.is_expired(ttl));
        before - jobs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::extract::serialize_extract_status;
    use serde_json::json;
    use tokio::sync::oneshot;

    fn completed_record(created_at: Instant) -> ExtractRecord {
        ExtractRecord {
            status: ExtractStatus::Processing,
            data: Some(json!({"last": 3})),
            per_url: (1..=3)
                .map(|index| UrlResult {
                    url: format!("https://example.com/{index}"),
                    status: ExtractStatus::Completed,
                    data: Some(json!({"index": index})),
                    error: None,
                    llm_usage: None,
                    basis: None,
                    basis_warnings: Vec::new(),
                    llm_input_hash: None,
                })
                .collect(),
            tokens_used: 30,
            credits_used: 3,
            error: None,
            created_at,
            expires_at: SystemTime::now() + Duration::from_secs(3_600),
            claimed_index: None,
        }
    }

    fn spawn_final_write(
        state: AppState,
        id: Uuid,
    ) -> (oneshot::Receiver<()>, tokio::task::JoinHandle<()>) {
        let (ready_tx, ready_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            let _ = ready_tx.send(());
            state.finalize_extract_job(id).await;
        });
        (ready_rx, handle)
    }

    fn spawn_delete(
        state: AppState,
        id: Uuid,
    ) -> (oneshot::Receiver<()>, tokio::task::JoinHandle<()>) {
        let (ready_tx, ready_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            let _ = ready_tx.send(());
            state.cancel_extract_job(id).await.unwrap();
        });
        (ready_rx, handle)
    }

    async fn run_final_write_delete_race(final_write_first: bool) -> (AppState, Uuid) {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        let id = Uuid::new_v4();
        state
            .extract_jobs
            .write()
            .await
            .insert(id, completed_record(Instant::now()));

        // Hold the exact lock used by both operations, then enqueue them in a
        // known order. Tokio's fair write lock releases to the first waiter,
        // making both legal race winners deterministic rather than probabilistic.
        let gate = state.extract_jobs.write().await;
        let ((first_ready, first), (second_ready, second)) = if final_write_first {
            (
                spawn_final_write(state.clone(), id),
                spawn_delete(state.clone(), id),
            )
        } else {
            (
                spawn_delete(state.clone(), id),
                spawn_final_write(state.clone(), id),
            )
        };
        first_ready.await.unwrap();
        second_ready.await.unwrap();
        drop(gate);
        first.await.unwrap();
        second.await.unwrap();
        (state, id)
    }

    #[tokio::test]
    async fn final_write_versus_delete_race_covers_both_winners_and_freezes_terminal_state() {
        // Whichever of the final write or the DELETE wins, a job whose URLs all
        // completed settles as Completed: a late cancel that stopped nothing
        // in-flight must not relabel a finished job (the per-URL results below
        // are all Completed in both branches).
        for (final_write_first, expected) in [
            (true, ExtractStatus::Completed),
            (false, ExtractStatus::Completed),
        ] {
            let (state, id) = run_final_write_delete_race(final_write_first).await;
            let terminal = state.get_extract_job(id).await.unwrap();
            assert_eq!(terminal.status, expected);
            assert_eq!(terminal.per_url.len(), 3);
            assert_eq!(
                terminal
                    .per_url
                    .iter()
                    .map(|result| result.url.as_str())
                    .collect::<Vec<_>>(),
                [
                    "https://example.com/1",
                    "https://example.com/2",
                    "https://example.com/3"
                ]
            );
            assert!(
                terminal
                    .per_url
                    .iter()
                    .all(|result| result.status == ExtractStatus::Completed)
            );

            let frozen = serde_json::to_value(serialize_extract_status(id, terminal)).unwrap();
            state.finalize_extract_job(id).await;
            let repeated_delete = state.cancel_extract_job(id).await.unwrap();
            let repeated =
                serde_json::to_value(serialize_extract_status(id, repeated_delete)).unwrap();
            assert_eq!(repeated, frozen, "terminal envelope must be immutable");
        }
    }

    #[tokio::test]
    async fn cleanup_removes_expired_nonterminal_extract_jobs() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        let processing_id = Uuid::new_v4();
        let cancelling_id = Uuid::new_v4();
        let old = Instant::now() - Duration::from_secs(5);
        let mut cancelling = completed_record(old);
        cancelling.status = ExtractStatus::Cancelling;
        {
            let mut jobs = state.extract_jobs.write().await;
            jobs.insert(processing_id, completed_record(old));
            jobs.insert(cancelling_id, cancelling);
        }

        assert_eq!(
            state
                .prune_expired_extract_jobs(Duration::from_secs(1))
                .await,
            2
        );
        assert!(state.extract_jobs.read().await.is_empty());
    }

    // ── validate_renderer_pin / validate_crawl_renderer ──

    fn crawl_request(
        url: &str,
        renderer: Option<RequestedRenderer>,
        render_js: Option<bool>,
    ) -> CrawlRequest {
        CrawlRequest {
            url: url.to_string(),
            max_depth: None,
            max_pages: None,
            formats: vec![crw_core::types::OutputFormat::Markdown],
            only_main_content: None,
            json_schema: None,
            render_js,
            wait_for: None,
            renderer,
            country: None,
            proxy_list: Vec::new(),
            proxy_rotation: None,
            headers: std::collections::HashMap::new(),
        }
    }

    #[tokio::test]
    async fn validate_renderer_pin_auto_or_absent_is_always_ok() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        assert!(validate_renderer_pin(Some(RequestedRenderer::Auto), None, &state).is_ok());
        assert!(validate_renderer_pin(None, Some(true), &state).is_ok());
    }

    #[tokio::test]
    async fn validate_renderer_pin_unavailable_renderer_is_rejected_with_name_and_list() {
        // Default config builds no CDP tier (no ws_url configured), so the pool
        // is always empty here regardless of build features.
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        let err = validate_renderer_pin(Some(RequestedRenderer::Chrome), None, &state).unwrap_err();
        match err {
            CrwError::InvalidRequest(msg) => {
                assert!(
                    msg.contains("renderer 'chrome' not available"),
                    "message was: {msg}"
                );
                // A feature-on build's default config constructs the
                // impersonated tier, so it appears in the advertised list; a
                // lean build has an empty vocabulary.
                #[cfg(feature = "impersonated")]
                assert!(
                    msg.contains("configured renderers: [impersonated-http]"),
                    "message was: {msg}"
                );
                #[cfg(not(feature = "impersonated"))]
                assert!(
                    msg.contains("configured renderers: []"),
                    "message was: {msg}"
                );
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_renderer_pin_skipped_when_render_js_explicitly_false() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        // Explicit renderJs:false takes the HTTP-only path and never consults
        // the (unavailable) JS renderer pool.
        assert!(
            validate_renderer_pin(Some(RequestedRenderer::Chrome), Some(false), &state).is_ok()
        );
    }

    #[tokio::test]
    async fn validate_renderer_pin_forces_js_true_when_request_omits_render_js() {
        // "Pinned implies JS": a server default of render_js_default=false must
        // not let an omitted renderJs silently skip validation.
        let config: AppConfig = toml::from_str("[renderer]\nrender_js_default = false\n").unwrap();
        let state = AppState::new(config).unwrap();
        let err = validate_renderer_pin(Some(RequestedRenderer::Chrome), None, &state).unwrap_err();
        assert!(matches!(err, CrwError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn validate_crawl_renderer_delegates_and_surfaces_the_pinned_name() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        let req = crawl_request(
            "https://example.com",
            Some(RequestedRenderer::Lightpanda),
            None,
        );
        let err = validate_crawl_renderer(&req, &state).unwrap_err();
        match err {
            CrwError::InvalidRequest(msg) => assert!(msg.contains("lightpanda")),
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_crawl_renderer_ok_when_no_renderer_pinned() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        let req = crawl_request("https://example.com", None, None);
        assert!(validate_crawl_renderer(&req, &state).is_ok());
    }

    // ── ExtractStatus ──

    #[test]
    fn extract_status_as_str_matches_wire_values() {
        assert_eq!(ExtractStatus::Processing.as_str(), "processing");
        assert_eq!(ExtractStatus::Cancelling.as_str(), "cancelling");
        assert_eq!(ExtractStatus::Completed.as_str(), "completed");
        assert_eq!(ExtractStatus::Failed.as_str(), "failed");
        assert_eq!(ExtractStatus::Cancelled.as_str(), "cancelled");
    }

    #[test]
    fn extract_status_is_terminal_only_for_completed_failed_cancelled() {
        assert!(!ExtractStatus::Processing.is_terminal());
        assert!(!ExtractStatus::Cancelling.is_terminal());
        assert!(ExtractStatus::Completed.is_terminal());
        assert!(ExtractStatus::Failed.is_terminal());
        assert!(ExtractStatus::Cancelled.is_terminal());
    }

    // ── ExtractRecord::is_expired ──

    #[test]
    fn extract_record_is_expired_false_within_ttl() {
        let rec = completed_record(Instant::now());
        assert!(!rec.is_expired(Duration::from_secs(60)));
    }

    #[test]
    fn extract_record_is_expired_true_once_ttl_elapsed() {
        let rec = completed_record(Instant::now() - Duration::from_secs(10));
        assert!(rec.is_expired(Duration::from_secs(5)));
    }

    #[test]
    fn extract_record_is_expired_true_at_zero_ttl() {
        let rec = completed_record(Instant::now());
        assert!(rec.is_expired(Duration::ZERO));
    }

    // ── ExtractRecord state machine (finish_cancellation / complete_from_outcomes / finish_processing) ──

    fn record_with_statuses(statuses: &[ExtractStatus]) -> ExtractRecord {
        ExtractRecord {
            status: ExtractStatus::Processing,
            data: None,
            per_url: statuses
                .iter()
                .enumerate()
                .map(|(index, &status)| UrlResult {
                    url: format!("https://example.com/{index}"),
                    status,
                    data: (status == ExtractStatus::Completed).then(|| json!({"index": index})),
                    error: (status == ExtractStatus::Failed).then(|| format!("error-{index}")),
                    llm_usage: None,
                    basis: None,
                    basis_warnings: Vec::new(),
                    llm_input_hash: None,
                })
                .collect(),
            tokens_used: 0,
            credits_used: 0,
            error: None,
            created_at: Instant::now(),
            expires_at: SystemTime::now() + Duration::from_secs(3_600),
            claimed_index: None,
        }
    }

    #[test]
    fn finish_cancellation_noop_when_status_is_not_cancelling() {
        let mut rec = record_with_statuses(&[ExtractStatus::Processing]);
        rec.finish_cancellation();
        assert_eq!(rec.status, ExtractStatus::Processing);
        assert_eq!(rec.per_url[0].status, ExtractStatus::Processing);
    }

    #[test]
    fn finish_cancellation_noop_while_a_url_is_still_claimed() {
        let mut rec = record_with_statuses(&[ExtractStatus::Processing]);
        rec.status = ExtractStatus::Cancelling;
        rec.claimed_index = Some(0);
        rec.finish_cancellation();
        // Barrier not crossed yet: nothing may settle while a slot is claimed.
        assert_eq!(rec.status, ExtractStatus::Cancelling);
        assert_eq!(rec.per_url[0].status, ExtractStatus::Processing);
    }

    #[test]
    fn finish_cancellation_cancels_remaining_processing_urls_and_clears_their_fields() {
        let mut rec = record_with_statuses(&[ExtractStatus::Completed, ExtractStatus::Processing]);
        rec.status = ExtractStatus::Cancelling;
        rec.finish_cancellation();
        assert_eq!(rec.status, ExtractStatus::Cancelled);
        assert_eq!(
            rec.per_url[0].status,
            ExtractStatus::Completed,
            "already-terminal URL must be left untouched"
        );
        assert_eq!(rec.per_url[1].status, ExtractStatus::Cancelled);
        assert!(rec.per_url[1].data.is_none());
        assert!(rec.per_url[1].error.is_none());
    }

    #[test]
    fn finish_cancellation_settles_completed_when_nothing_was_actually_in_flight() {
        // Every URL had already finished before the cancel landed: reporting
        // "cancelled" would contradict real per-URL results that all succeeded.
        let mut rec = record_with_statuses(&[ExtractStatus::Completed, ExtractStatus::Failed]);
        rec.status = ExtractStatus::Cancelling;
        rec.finish_cancellation();
        assert_eq!(rec.status, ExtractStatus::Completed);
    }

    #[test]
    fn finish_cancellation_settles_failed_when_every_finished_url_failed() {
        let mut rec = record_with_statuses(&[ExtractStatus::Failed, ExtractStatus::Failed]);
        rec.status = ExtractStatus::Cancelling;
        rec.finish_cancellation();
        assert_eq!(rec.status, ExtractStatus::Failed);
    }

    #[test]
    fn finish_processing_completes_and_seeds_empty_data_when_data_was_none() {
        let mut rec = record_with_statuses(&[ExtractStatus::Completed, ExtractStatus::Failed]);
        rec.data = None;
        rec.finish_processing();
        assert_eq!(rec.status, ExtractStatus::Completed);
        assert_eq!(rec.data, Some(json!({})));
    }

    #[test]
    fn finish_processing_fails_and_picks_the_last_error_in_original_order() {
        let mut rec = record_with_statuses(&[ExtractStatus::Failed, ExtractStatus::Failed]);
        rec.per_url[0].error = Some("first".into());
        rec.per_url[1].error = Some("second".into());
        rec.finish_processing();
        assert_eq!(rec.status, ExtractStatus::Failed);
        assert_eq!(rec.error.as_deref(), Some("second"));
    }

    #[test]
    fn finish_processing_reports_no_error_when_all_failed_urls_carried_none() {
        let mut rec = record_with_statuses(&[ExtractStatus::Failed]);
        rec.per_url[0].error = None;
        rec.finish_processing();
        assert_eq!(rec.status, ExtractStatus::Failed);
        assert_eq!(rec.error, None);
    }

    #[test]
    fn finish_processing_credits_floor_is_one_when_nothing_was_measured() {
        let mut rec = record_with_statuses(&[ExtractStatus::Failed]);
        rec.credits_used = 0;
        rec.finish_processing();
        assert_eq!(rec.credits_used, 1);
    }

    #[test]
    fn finish_processing_preserves_measured_credits_above_the_floor() {
        let mut rec = record_with_statuses(&[ExtractStatus::Completed]);
        rec.credits_used = 5;
        rec.finish_processing();
        assert_eq!(rec.credits_used, 5);
    }

    #[test]
    fn finish_processing_is_a_noop_once_already_terminal() {
        let mut rec = record_with_statuses(&[ExtractStatus::Completed]);
        rec.status = ExtractStatus::Completed;
        rec.error = Some("must not change".into());
        rec.finish_processing();
        assert_eq!(rec.status, ExtractStatus::Completed);
        assert_eq!(rec.error.as_deref(), Some("must not change"));
    }

    #[test]
    fn finish_processing_delegates_to_finish_cancellation_when_status_is_cancelling() {
        let mut rec = record_with_statuses(&[ExtractStatus::Processing]);
        rec.status = ExtractStatus::Cancelling;
        rec.finish_processing();
        assert_eq!(rec.status, ExtractStatus::Cancelled);
        assert_eq!(rec.per_url[0].status, ExtractStatus::Cancelled);
    }

    // ── AppState::new ──

    #[tokio::test]
    async fn app_state_new_default_crawl_semaphore_has_max_concurrent_crawls_permits() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        let permits = state
            .crawl_semaphore
            .acquire_many(MAX_CONCURRENT_CRAWLS as u32)
            .await
            .unwrap();
        assert_eq!(state.crawl_semaphore.available_permits(), 0);
        drop(permits);
        assert_eq!(
            state.crawl_semaphore.available_permits(),
            MAX_CONCURRENT_CRAWLS
        );
    }

    #[tokio::test]
    async fn app_state_new_batch_pipeline_sem_is_none_when_aggregate_cap_is_zero() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        assert!(state.batch_pipeline_sem.is_none());
    }

    #[tokio::test]
    async fn app_state_new_batch_pipeline_sem_carries_the_configured_permit_count() {
        let config: AppConfig =
            toml::from_str("[crawler]\nmax_aggregate_batch_pipelines = 7\n").unwrap();
        let state = AppState::new(config).unwrap();
        let sem = state
            .batch_pipeline_sem
            .expect("expected a bounded semaphore");
        assert_eq!(sem.available_permits(), 7);
    }

    #[tokio::test]
    async fn app_state_new_searxng_is_none_when_no_backend_url_is_configured() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        assert!(state.searxng.is_none());
    }

    #[tokio::test]
    async fn app_state_new_searxng_is_some_once_a_backend_url_is_configured() {
        let config: AppConfig =
            toml::from_str("[search]\nsearch_backend_url = \"http://searxng:8080\"\n").unwrap();
        let state = AppState::new(config).unwrap();
        assert!(state.searxng.is_some());
    }

    #[tokio::test]
    async fn app_state_new_url_filter_is_always_configured() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        assert!(state.url_filter.is_some());
    }

    #[test]
    fn app_state_new_rejects_a_malformed_proxy_url() {
        let config: AppConfig =
            toml::from_str("[crawler]\nproxy = \"htp://not-a-scheme\"\n").unwrap();
        let err = match AppState::new(config) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert!(matches!(err, CrwError::ConfigError(_)));
    }

    // ── start_batch_job / start_extract_job (network-free paths only) ──

    #[tokio::test]
    async fn start_batch_job_with_no_urls_completes_immediately_without_fetching() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        let template = ScrapeRequest {
            url: String::new(),
            ..Default::default()
        };
        let id = state.start_batch_job(Vec::new(), template, None).await;

        let mut settled = None;
        for _ in 0..100 {
            tokio::task::yield_now().await;
            let jobs = state.crawl_jobs.read().await;
            let st = jobs.get(&id).unwrap().rx.borrow().clone();
            if st.status != CrawlStatus::InProgress {
                settled = Some(st);
                break;
            }
        }
        let job = settled.expect("empty batch job did not settle");
        assert_eq!(job.status, CrawlStatus::Completed);
        assert_eq!(job.total, 0);
        assert_eq!(job.completed, 0);
        assert!(job.data.is_empty());
    }

    #[tokio::test]
    async fn start_batch_job_records_scrape_errors_as_blocked_documents() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        let template = ScrapeRequest {
            actions: Some(serde_json::json!([])),
            ..Default::default()
        };
        let url = "https://example.com/error";
        let id = state
            .start_batch_job(vec![url.to_string()], template, None)
            .await;

        let mut settled = None;
        for _ in 0..100 {
            tokio::task::yield_now().await;
            let jobs = state.crawl_jobs.read().await;
            let job = jobs.get(&id).unwrap().rx.borrow().clone();
            if job.status != CrawlStatus::InProgress {
                settled = Some(job);
                break;
            }
        }
        let job = settled.expect("batch job did not settle");
        assert_eq!(job.status, CrawlStatus::Completed);
        assert_eq!(job.completed, 1);
        assert_eq!(job.blocked, 1);
        assert_eq!(job.data.len(), 1);
        assert_eq!(job.data[0].metadata.source_url, url);
        let block = job.data[0]
            .block
            .as_ref()
            .expect("failed URL carries a block");
        assert_eq!(block.vendor, crw_core::types::HTTP_ERROR_VENDOR);
        // Substring, not the whole sentence: the wording lives in `crw-crawl` and
        // rewording a customer-facing message must not break a `crw-server` test.
        assert!(
            block.reason.contains("actions"),
            "reason should name the rejected parameter, got: {}",
            block.reason
        );
    }

    #[tokio::test]
    async fn start_extract_job_with_all_preflight_errors_finalizes_without_any_fetch() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        let entries = vec![
            PreparedUrl {
                url: "not-a-url".into(),
                preflight_error: Some("invalid URL".into()),
            },
            PreparedUrl {
                url: "javascript:alert(1)".into(),
                preflight_error: Some("blocked scheme".into()),
            },
        ];
        let id = state
            .start_extract_job(entries, ScrapeRequest::default())
            .await;

        let mut record = None;
        for _ in 0..100 {
            tokio::task::yield_now().await;
            let rec = state.get_extract_job(id).await.unwrap();
            if rec.status.is_terminal() {
                record = Some(rec);
                break;
            }
        }
        let record = record.expect("all-preflight-failed extract job did not finalize");
        assert_eq!(record.status, ExtractStatus::Failed);
        assert_eq!(record.error.as_deref(), Some("blocked scheme"));
        assert_eq!(
            record.credits_used, 1,
            "one-credit floor on an all-failed job"
        );
        // Original request order is preserved and neither entry was fetched.
        assert_eq!(record.per_url.len(), 2);
        assert_eq!(record.per_url[0].url, "not-a-url");
        assert_eq!(record.per_url[0].error.as_deref(), Some("invalid URL"));
        assert_eq!(record.per_url[1].url, "javascript:alert(1)");
        assert_eq!(record.per_url[1].error.as_deref(), Some("blocked scheme"));
    }

    // ── cancel_extract_job / get_extract_job ──

    #[tokio::test]
    async fn cancel_extract_job_errors_not_found_for_a_missing_id() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        let err = state.cancel_extract_job(Uuid::new_v4()).await.unwrap_err();
        assert!(matches!(err, CrwError::NotFound(_)));
    }

    #[tokio::test]
    async fn cancel_extract_job_removes_and_errors_on_an_expired_job() {
        let config: AppConfig = toml::from_str("[crawler]\njob_ttl_secs = 1\n").unwrap();
        let state = AppState::new(config).unwrap();
        let id = Uuid::new_v4();
        state.extract_jobs.write().await.insert(
            id,
            completed_record(Instant::now() - Duration::from_secs(5)),
        );

        let err = state.cancel_extract_job(id).await.unwrap_err();
        assert!(matches!(err, CrwError::NotFound(_)));
        assert!(!state.extract_jobs.read().await.contains_key(&id));
    }

    #[tokio::test]
    async fn cancel_extract_job_moves_processing_to_cancelled_when_no_url_is_claimed() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        let id = Uuid::new_v4();
        let mut rec = record_with_statuses(&[ExtractStatus::Completed, ExtractStatus::Processing]);
        rec.status = ExtractStatus::Processing;
        state.extract_jobs.write().await.insert(id, rec);

        let cancelled = state.cancel_extract_job(id).await.unwrap();
        assert_eq!(cancelled.status, ExtractStatus::Cancelled);
        assert_eq!(cancelled.per_url[1].status, ExtractStatus::Cancelled);
    }

    #[tokio::test]
    async fn cancel_extract_job_is_idempotent_on_a_second_call() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        let id = Uuid::new_v4();
        state
            .extract_jobs
            .write()
            .await
            .insert(id, completed_record(Instant::now()));

        let first = state.cancel_extract_job(id).await.unwrap();
        let second = state.cancel_extract_job(id).await.unwrap();
        assert_eq!(first.status, second.status);
        assert_eq!(first.credits_used, second.credits_used);
    }

    #[tokio::test]
    async fn get_extract_job_errors_not_found_for_a_missing_id() {
        let config: AppConfig = toml::from_str("").unwrap();
        let state = AppState::new(config).unwrap();
        let err = state.get_extract_job(Uuid::new_v4()).await.unwrap_err();
        assert!(matches!(err, CrwError::NotFound(_)));
    }

    #[tokio::test]
    async fn get_extract_job_removes_and_errors_on_an_expired_job() {
        let config: AppConfig = toml::from_str("[crawler]\njob_ttl_secs = 1\n").unwrap();
        let state = AppState::new(config).unwrap();
        let id = Uuid::new_v4();
        state.extract_jobs.write().await.insert(
            id,
            completed_record(Instant::now() - Duration::from_secs(5)),
        );

        let err = state.get_extract_job(id).await.unwrap_err();
        assert!(matches!(err, CrwError::NotFound(_)));
        assert!(!state.extract_jobs.read().await.contains_key(&id));
    }
}
