//! Prometheus metrics for renderer routing, host preferences, and circuit breakers.
//!
//! Counters and gauges are registered lazily on the default registry. Use
//! [`gather_text`] to render the current snapshot for `/metrics`.

use prometheus::{
    Encoder, Histogram, HistogramVec, IntCounter, IntCounterVec, IntGauge, Registry, TextEncoder,
    exponential_buckets, histogram_opts, register_histogram_vec_with_registry,
    register_histogram_with_registry, register_int_counter_vec_with_registry,
    register_int_counter_with_registry, register_int_gauge_with_registry,
};
use std::sync::OnceLock;

pub struct Metrics {
    pub registry: Registry,
    /// Outcome of routing decision per request: chosen renderer + decision kind.
    pub render_route_decision_total: IntCounterVec,
    /// Circuit breaker transitions to Open state, labeled by renderer.
    pub circuit_breaker_open_total: IntCounterVec,
    /// Host promoted to a heavier renderer by the auto-mode learner.
    pub host_preferences_promotions_total: IntCounterVec,
    /// Admin reset operations on the host preferences cache.
    pub admin_preferences_reset_total: IntCounterVec,
    /// User-pinned renderer requests (bypasses auto-mode learning).
    pub user_pin_total: IntCounterVec,
    /// Current size of the host preferences cache.
    pub host_preferences_size: IntGauge,
    /// Hosts currently latched to proxy-first egress after a hard block.
    /// A runaway value means we are paying for proxy bandwidth we may not need.
    pub egress_latched_hosts: IntGauge,
    /// Fetches that started on the proxy because the host was latched.
    pub egress_latch_hit_total: IntCounter,
    /// Chrome navigation budget truncations, labeled by snapshot outcome
    /// (`ok` = partial DOM extracted, `empty` = nothing snapshotted).
    pub chrome_budget_truncated_total: IntCounterVec,
    /// Requests blocked by the chrome interception blocklist, labeled by
    /// reason (`resource_type`, `host`).
    pub chrome_blocked_requests_total: IntCounterVec,
    /// Outcomes ignored by the renderer circuit breaker (deadline-clamped,
    /// truncated-but-OK, etc.) — labeled by renderer and reason.
    pub breaker_ignored_total: IntCounterVec,
    /// CDP `pending` map size summed across all live connections at the
    /// last sampler tick. Should return to 0 between scrapes; sustained
    /// growth indicates a cancellation/cleanup leak.
    pub cdp_pending_requests: IntGauge,
    /// Number of CDP connections currently registered as live.
    /// Per-fetch lifecycle, so this should track concurrency, not pool size.
    pub cdp_live_connections: IntGauge,
    /// Target lifecycle events, labeled by renderer + phase
    /// (`created` | `closed` | `leaked`). `leaked` is incremented when
    /// `Target.closeTarget` times out — the engine moves on, but the page
    /// likely stays alive in chrome.
    pub target_lifecycle_total: IntCounterVec,
    /// Renderer recycle events, labeled by renderer + reason
    /// (`age` | `count`). Reserved for the optional page-sweep work in B.1.
    pub renderer_recycle_total: IntCounterVec,
    /// /map URL filter: URLs dropped entirely (Tier A action-URL filter or
    /// parse-error pass-through bookkeeping). Labels: `reason`.
    pub map_filter_dropped_total: IntCounterVec,
    /// /map URL filter: query params stripped (Tier B / coarse mode). Labels: `reason`.
    pub map_filter_stripped_total: IntCounterVec,
    /// /map URL filter: param/URL preserved by a rule that beats the deny-list
    /// (host override, `.gov` TLD, ALWAYS_PRESERVE). Labels: `reason`.
    pub map_filter_preserved_total: IntCounterVec,
    /// /map URL filter: count of rules loaded at server startup. Labels: `kind`.
    pub map_filter_rules_loaded: IntCounterVec,
    /// Chrome WS connect duration, labeled by outcome.
    /// Tier 0 (Browser Context Pool plan): isolates connect+handshake cost.
    /// `outcome` ∈ {ok, ws_dial_fail, ws_handshake_timeout, version_probe_fail}.
    pub chrome_connect_seconds: HistogramVec,
    /// Chrome `Target.createTarget` round-trip duration. Tier 0 sub-metric.
    pub chrome_target_create_seconds: Histogram,
    /// Chrome navigation duration: from `Page.navigate` send to first
    /// `Page.loadEventFired`. Tier 0 sub-metric.
    pub chrome_navigate_seconds: Histogram,
    /// Chrome HTML snapshot duration: `Runtime.evaluate(outerHTML)` round-trip.
    /// Tier 0 sub-metric.
    pub chrome_snapshot_seconds: Histogram,
    // -------- Browser Context Pool (Tier 1+) --------
    /// Configured pool size.
    pub chrome_pool_size: IntGauge,
    /// Current idle slot count (sampled).
    pub chrome_pool_idle: IntGauge,
    /// Current `CheckedOut` slot count.
    pub chrome_pool_inflight: IntGauge,
    /// Wait time to acquire a reserved concurrency lane, by `lane`
    /// ∈ {extract, pdf, llm, host, render} and `class` ∈ {interactive, batch}.
    /// The proof metric for interactive isolation: interactive p99 should stay
    /// flat while batch saturates a lane.
    pub reserved_lane_wait_seconds: HistogramVec,
    /// Current in-flight `/v2/batch/scrape` URL-pipelines (aggregate cap gauge).
    pub batch_pipelines_inflight: IntGauge,
    /// Time spent in `pool.acquire()` (waiting for a permit + slot creation).
    pub chrome_pool_acquire_seconds: Histogram,
    /// Acquire-path accounting.
    /// `outcome` ∈ {hit_idle, created_new, errored, shutdown_refused}.
    pub chrome_pool_acquires_total: IntCounterVec,
    /// Per-phase release cost.
    /// `phase` ∈ {close_target, dispose_ctx, create_ctx}.
    pub chrome_pool_recycle_seconds: HistogramVec,
    /// Terminal recycle outcomes.
    /// `outcome` ∈ {success, dead_conn, idle_timeout, emergency_drop,
    ///              nav_count_recycle, health_fail, shutdown_raced,
    ///              unexpected_state}.
    pub chrome_pool_recycle_total: IntCounterVec,
    /// Failure-only counter, partitioned by failed stage.
    /// `stage` ∈ {close_target_timeout, dispose_ctx_fail, create_ctx_fail,
    ///            missed_release}.
    pub chrome_pool_recycle_failures_total: IntCounterVec,
    /// Idle-slot health probe outcomes.
    /// `outcome` ∈ {ok, failed}.
    pub chrome_pool_health_check_total: IntCounterVec,
    /// How long each context survives between create and dispose. Informs
    /// v2 context-lifetime decisions.
    pub chrome_context_lifetime_seconds: Histogram,
    /// Pre-navigation overhead per Chrome request — the B2 gate metric.
    /// `pool` ∈ {on, off}; `acquire_source` ∈ {hit_idle, created_new,
    /// health_checked, n/a}. Pool-off requests use `acquire_source="n/a"`.
    pub chrome_request_handshake_seconds: HistogramVec,
    /// Vendor-specific anti-bot block detections in renderer responses.
    /// Labeled by `vendor` (cloudflare, akamai, perimeterx, datadome,
    /// imperva, sucuri, kasada, cloudfront). Distinct from the catch-all
    /// generic-bot-wall path — only fires when a durable vendor signature
    /// matches.
    pub vendor_block_total: IntCounterVec,
    /// Anti-bot blocks flagged by the `crw_extract::antibot` classifier
    /// inside the renderer failover loop. Labeled by `signal` (cloudflare,
    /// datadome, network_security, generic_block, structural_failure, …).
    /// Emitted even when `antibot.escalate_in_failover = false`, so the
    /// dashboard shows escalation pressure before the switch is flipped.
    pub antibot_escalation_total: IntCounterVec,
    // -------- Change tracking (monitor) --------
    /// Wall-clock duration of one `compute_change_tracking` call, labeled by
    /// mode (`gitDiff` | `json` | `mixed` | `binary`).
    pub change_tracking_duration_seconds: HistogramVec,
    /// Size in bytes of the current snapshot retained per change-tracking call
    /// (markdown + json), labeled by mode. Informs storage/retention sizing.
    pub change_tracking_snapshot_bytes: HistogramVec,
    /// LLM meaningful-change judge calls, labeled by outcome
    /// (`ok` | `error` | `skipped`).
    pub judge_calls_total: IntCounterVec,
    /// LLM judge token usage, labeled by kind (`input` | `output`).
    pub judge_tokens_total: IntCounterVec,
    /// Structured extractions where the model's first tool call failed the
    /// caller's schema and we re-asked with the validation errors.
    ///
    /// This is the ONLY place a retry is visible in aggregate: the retried
    /// attempt's tokens are deliberately not billed and therefore never reach
    /// `UsageEvent`, so a retry storm would double our provider spend without
    /// moving any of the cost dashboards. Watch this counter instead.
    pub structured_retries_total: IntCounter,
    // -------- Document (PDF) parsing --------
    /// Document conversions, labeled by outcome (`ok` | `empty` | `error`).
    pub document_conversions_total: IntCounterVec,
    /// Wall-clock duration of one document conversion, labeled by format.
    pub document_conversion_duration_seconds: HistogramVec,
    /// Pages processed across document conversions, labeled by format.
    pub document_pages_total: IntCounterVec,
    /// Document classification outcomes, labeled by class
    /// (`text` | `scanned` | `encrypted` | `corrupt`).
    pub document_classification_total: IntCounterVec,
}

static METRICS: OnceLock<Metrics> = OnceLock::new();

pub fn metrics() -> &'static Metrics {
    METRICS.get_or_init(Metrics::new)
}

/// Eagerly register all metrics at boot. Forces `OnceLock` initialisation so
/// alert rules referencing series that have never emitted are evaluated
/// against present (zero-valued) series instead of absent ones.
pub fn init() {
    let _ = metrics();
}

impl Metrics {
    fn new() -> Self {
        let registry = Registry::new();
        let render_route_decision_total = register_int_counter_vec_with_registry!(
            "crw_render_route_decision_total",
            "Routing decisions by chosen renderer and decision kind",
            &["renderer", "decision"],
            registry
        )
        .unwrap();
        let circuit_breaker_open_total = register_int_counter_vec_with_registry!(
            "crw_circuit_breaker_open_total",
            "Circuit breaker transitions to Open, labeled by renderer and scope",
            &["renderer", "scope"],
            registry
        )
        .unwrap();
        let host_preferences_promotions_total = register_int_counter_vec_with_registry!(
            "crw_host_preferences_promotions_total",
            "Host preference promotions to a heavier renderer",
            &["from", "to"],
            registry
        )
        .unwrap();
        let admin_preferences_reset_total = register_int_counter_vec_with_registry!(
            "crw_admin_preferences_reset_total",
            "Admin resets of host preference state",
            &["scope"],
            registry
        )
        .unwrap();
        let user_pin_total = register_int_counter_vec_with_registry!(
            "crw_user_pin_total",
            "User-pinned renderer requests",
            &["renderer"],
            registry
        )
        .unwrap();
        let host_preferences_size = register_int_gauge_with_registry!(
            "crw_host_preferences_size",
            "Current size of the host preferences cache",
            registry
        )
        .unwrap();
        let egress_latched_hosts = register_int_gauge_with_registry!(
            "crw_egress_latched_hosts",
            "Hosts currently latched to proxy-first egress after a hard block",
            registry
        )
        .unwrap();
        let egress_latch_hit_total = register_int_counter_with_registry!(
            "crw_egress_latch_hit_total",
            "Fetches that started on the proxy because the host was latched",
            registry
        )
        .unwrap();
        let chrome_budget_truncated_total = register_int_counter_vec_with_registry!(
            "crw_chrome_budget_truncated_total",
            "Chrome nav-budget truncations by snapshot outcome",
            &["outcome"],
            registry
        )
        .unwrap();
        let chrome_blocked_requests_total = register_int_counter_vec_with_registry!(
            "crw_chrome_blocked_requests_total",
            "Chrome request-interception outcomes, labeled by reason",
            &["reason"],
            registry
        )
        .unwrap();
        let breaker_ignored_total = register_int_counter_vec_with_registry!(
            "crw_breaker_ignored_total",
            "Renderer outcomes ignored by the circuit breaker (deadline-clamped, truncated, etc.)",
            &["renderer", "reason"],
            registry
        )
        .unwrap();
        let cdp_pending_requests = register_int_gauge_with_registry!(
            "crw_cdp_pending_requests",
            "CDP pending request map size summed across all live connections (sampler tick)",
            registry
        )
        .unwrap();
        let cdp_live_connections = register_int_gauge_with_registry!(
            "crw_cdp_live_connections",
            "Number of CDP connections currently registered as live",
            registry
        )
        .unwrap();
        let target_lifecycle_total = register_int_counter_vec_with_registry!(
            "crw_target_lifecycle_total",
            "CDP target lifecycle events by renderer and phase (created/closed/leaked)",
            &["renderer", "phase"],
            registry
        )
        .unwrap();
        let renderer_recycle_total = register_int_counter_vec_with_registry!(
            "crw_renderer_recycle_total",
            "Renderer recycle events by renderer and reason",
            &["renderer", "reason"],
            registry
        )
        .unwrap();
        let map_filter_dropped_total = register_int_counter_vec_with_registry!(
            "crw_map_filter_dropped_total",
            "URLs dropped by /map filter (action-URL or parse-error pass-through)",
            &["reason"],
            registry
        )
        .unwrap();
        let map_filter_stripped_total = register_int_counter_vec_with_registry!(
            "crw_map_filter_stripped_total",
            "Query params stripped by /map filter",
            &["reason"],
            registry
        )
        .unwrap();
        let map_filter_preserved_total = register_int_counter_vec_with_registry!(
            "crw_map_filter_preserved_total",
            "Params/URLs preserved by /map filter rules (host override, gov TLD, always-preserve)",
            &["reason"],
            registry
        )
        .unwrap();
        let map_filter_rules_loaded = register_int_counter_vec_with_registry!(
            "crw_map_filter_rules_loaded",
            "/map filter rules loaded at server startup",
            &["kind"],
            registry
        )
        .unwrap();
        // Tier 0 latency histograms for the Browser Context Pool plan.
        // Buckets: 10ms × 2^k for k=0..11 → 10ms, 20, 40, 80, 160, 320, 640,
        // 1.28s, 2.56s, 5.12s, 10.24s, 20.48s. Covers fast-path 50ms WS
        // connects and slow-path 5s+ navigations.
        let lat_buckets = exponential_buckets(0.01, 2.0, 12).unwrap();
        let chrome_connect_seconds = register_histogram_vec_with_registry!(
            histogram_opts!(
                "crw_chrome_connect_seconds",
                "Chrome WS connect duration by outcome",
                lat_buckets.clone()
            ),
            &["outcome"],
            registry
        )
        .unwrap();
        let chrome_target_create_seconds = register_histogram_with_registry!(
            histogram_opts!(
                "crw_chrome_target_create_seconds",
                "Chrome Target.createTarget round-trip duration",
                lat_buckets.clone()
            ),
            registry
        )
        .unwrap();
        let chrome_navigate_seconds = register_histogram_with_registry!(
            histogram_opts!(
                "crw_chrome_navigate_seconds",
                "Chrome navigation duration: Page.navigate send to loadEventFired",
                lat_buckets.clone()
            ),
            registry
        )
        .unwrap();
        let chrome_snapshot_seconds = register_histogram_with_registry!(
            histogram_opts!(
                "crw_chrome_snapshot_seconds",
                "Chrome HTML snapshot duration (Runtime.evaluate outerHTML)",
                lat_buckets.clone()
            ),
            registry
        )
        .unwrap();
        // -------- Browser Context Pool (Tier 1+) --------
        let chrome_pool_size = register_int_gauge_with_registry!(
            "crw_chrome_pool_size",
            "Configured browser context pool size",
            registry
        )
        .unwrap();
        let chrome_pool_idle = register_int_gauge_with_registry!(
            "crw_chrome_pool_idle",
            "Idle slot count in the browser context pool (sampled)",
            registry
        )
        .unwrap();
        let chrome_pool_inflight = register_int_gauge_with_registry!(
            "crw_chrome_pool_inflight",
            "CheckedOut slot count in the browser context pool",
            registry
        )
        .unwrap();
        let reserved_lane_wait_seconds = register_histogram_vec_with_registry!(
            histogram_opts!(
                "crw_reserved_lane_wait_seconds",
                "Wait to acquire a reserved concurrency lane, by lane and class",
                lat_buckets.clone()
            ),
            &["lane", "class"],
            registry
        )
        .unwrap();
        let batch_pipelines_inflight = register_int_gauge_with_registry!(
            "crw_batch_pipelines_inflight",
            "Current in-flight /v2/batch/scrape URL-pipelines",
            registry
        )
        .unwrap();
        let chrome_pool_acquire_seconds = register_histogram_with_registry!(
            histogram_opts!(
                "crw_chrome_pool_acquire_seconds",
                "Time spent in pool.acquire() — permit wait + slot bring-up",
                lat_buckets.clone()
            ),
            registry
        )
        .unwrap();
        let chrome_pool_acquires_total = register_int_counter_vec_with_registry!(
            "crw_chrome_pool_acquires_total",
            "Pool acquire outcomes (hit_idle | created_new | errored | shutdown_refused)",
            &["outcome"],
            registry
        )
        .unwrap();
        let chrome_pool_recycle_seconds = register_histogram_vec_with_registry!(
            histogram_opts!(
                "crw_chrome_pool_recycle_seconds",
                "Per-phase release cost (close_target | dispose_ctx | create_ctx)",
                lat_buckets.clone()
            ),
            &["phase"],
            registry
        )
        .unwrap();
        let chrome_pool_recycle_total = register_int_counter_vec_with_registry!(
            "crw_chrome_pool_recycle_total",
            "Terminal recycle outcomes",
            &["outcome"],
            registry
        )
        .unwrap();
        let chrome_pool_recycle_failures_total = register_int_counter_vec_with_registry!(
            "crw_chrome_pool_recycle_failures_total",
            "Recycle failures partitioned by failed stage",
            &["stage"],
            registry
        )
        .unwrap();
        let chrome_pool_health_check_total = register_int_counter_vec_with_registry!(
            "crw_chrome_pool_health_check_total",
            "Pool idle-slot health probe outcomes (ok | failed)",
            &["outcome"],
            registry
        )
        .unwrap();
        let chrome_context_lifetime_seconds = register_histogram_with_registry!(
            histogram_opts!(
                "crw_chrome_context_lifetime_seconds",
                "Lifetime of each browser context, create to dispose",
                lat_buckets.clone()
            ),
            registry
        )
        .unwrap();
        let chrome_request_handshake_seconds = register_histogram_vec_with_registry!(
            histogram_opts!(
                "crw_chrome_request_handshake_seconds",
                "Pre-navigation overhead per Chrome request (B2 gate metric)",
                lat_buckets
            ),
            &["pool", "acquire_source"],
            registry
        )
        .unwrap();
        let vendor_block_total = register_int_counter_vec_with_registry!(
            "crw_vendor_block_total",
            "Vendor-specific anti-bot block detections by vendor name",
            &["vendor"],
            registry
        )
        .unwrap();
        let antibot_escalation_total = register_int_counter_vec_with_registry!(
            "crw_antibot_escalation_total",
            "Anti-bot blocks flagged by the antibot classifier in the failover loop, by signal",
            &["signal"],
            registry
        )
        .unwrap();
        // -------- Change tracking (monitor) --------
        // Diff compute is sub-millisecond to low-ms; reuse the 10ms×2^k ladder.
        let ct_lat_buckets = exponential_buckets(0.001, 2.0, 12).unwrap();
        let change_tracking_duration_seconds = register_histogram_vec_with_registry!(
            histogram_opts!(
                "crw_change_tracking_duration_seconds",
                "Duration of one compute_change_tracking call by mode",
                ct_lat_buckets
            ),
            &["mode"],
            registry
        )
        .unwrap();
        // Snapshot sizes: 256 B × 4^k → 256B .. ~256 MB.
        let snapshot_byte_buckets = exponential_buckets(256.0, 4.0, 10).unwrap();
        let change_tracking_snapshot_bytes = register_histogram_vec_with_registry!(
            histogram_opts!(
                "crw_change_tracking_snapshot_bytes",
                "Retained snapshot size in bytes per change-tracking call, by mode",
                snapshot_byte_buckets
            ),
            &["mode"],
            registry
        )
        .unwrap();
        let judge_calls_total = register_int_counter_vec_with_registry!(
            "crw_judge_calls_total",
            "LLM meaningful-change judge calls by outcome (ok | error | skipped)",
            &["outcome"],
            registry
        )
        .unwrap();
        let judge_tokens_total = register_int_counter_vec_with_registry!(
            "crw_judge_tokens_total",
            "LLM judge token usage by kind (input | output)",
            &["kind"],
            registry
        )
        .unwrap();
        let structured_retries_total = register_int_counter_with_registry!(
            "crw_structured_retries_total",
            "Structured extractions re-asked after the first tool call failed the caller's schema",
            registry
        )
        .unwrap();
        let document_conversions_total = register_int_counter_vec_with_registry!(
            "crw_document_conversions_total",
            "Document conversions by outcome (ok | empty | error)",
            &["outcome"],
            registry
        )
        .unwrap();
        // Conversion latency: 5ms × 4^k → 5ms .. ~80s.
        let doc_lat_buckets = exponential_buckets(0.005, 4.0, 9).unwrap();
        let document_conversion_duration_seconds = register_histogram_vec_with_registry!(
            histogram_opts!(
                "crw_document_conversion_duration_seconds",
                "Duration of one document conversion by format",
                doc_lat_buckets
            ),
            &["format"],
            registry
        )
        .unwrap();
        let document_pages_total = register_int_counter_vec_with_registry!(
            "crw_document_pages_total",
            "Pages processed across document conversions by format",
            &["format"],
            registry
        )
        .unwrap();
        let document_classification_total = register_int_counter_vec_with_registry!(
            "crw_document_classification_total",
            "Document classification outcomes by class (text | scanned | encrypted | corrupt)",
            &["class"],
            registry
        )
        .unwrap();
        Self {
            registry,
            render_route_decision_total,
            circuit_breaker_open_total,
            host_preferences_promotions_total,
            admin_preferences_reset_total,
            user_pin_total,
            host_preferences_size,
            egress_latched_hosts,
            egress_latch_hit_total,
            chrome_budget_truncated_total,
            chrome_blocked_requests_total,
            breaker_ignored_total,
            cdp_pending_requests,
            cdp_live_connections,
            target_lifecycle_total,
            renderer_recycle_total,
            map_filter_dropped_total,
            map_filter_stripped_total,
            map_filter_preserved_total,
            map_filter_rules_loaded,
            chrome_connect_seconds,
            chrome_target_create_seconds,
            chrome_navigate_seconds,
            chrome_snapshot_seconds,
            chrome_pool_size,
            chrome_pool_idle,
            chrome_pool_inflight,
            reserved_lane_wait_seconds,
            batch_pipelines_inflight,
            chrome_pool_acquire_seconds,
            chrome_pool_acquires_total,
            chrome_pool_recycle_seconds,
            chrome_pool_recycle_total,
            chrome_pool_recycle_failures_total,
            chrome_pool_health_check_total,
            chrome_context_lifetime_seconds,
            chrome_request_handshake_seconds,
            vendor_block_total,
            antibot_escalation_total,
            change_tracking_duration_seconds,
            change_tracking_snapshot_bytes,
            judge_calls_total,
            judge_tokens_total,
            structured_retries_total,
            document_conversions_total,
            document_conversion_duration_seconds,
            document_pages_total,
            document_classification_total,
        }
    }
}

/// Render the current metrics snapshot in Prometheus text exposition format.
pub fn gather_text() -> String {
    let metric_families = metrics().registry.gather();
    let encoder = TextEncoder::new();
    let mut buf = Vec::new();
    encoder.encode(&metric_families, &mut buf).ok();
    String::from_utf8(buf).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tier 0 metrics must be registered eagerly so alert rules see zero
    /// series (not absent series) before the first observation.
    #[test]
    fn tier0_chrome_latency_histograms_registered() {
        let m = metrics();
        m.chrome_connect_seconds
            .with_label_values(&["ok"])
            .observe(0.05);
        m.chrome_target_create_seconds.observe(0.02);
        m.chrome_navigate_seconds.observe(0.4);
        m.chrome_snapshot_seconds.observe(0.01);
        let text = gather_text();
        assert!(
            text.contains("crw_chrome_connect_seconds"),
            "missing connect_seconds; got: {text}"
        );
        assert!(text.contains("crw_chrome_target_create_seconds"));
        assert!(text.contains("crw_chrome_navigate_seconds"));
        assert!(text.contains("crw_chrome_snapshot_seconds"));
        assert!(text.contains(r#"outcome="ok""#));
    }

    #[test]
    fn change_tracking_metrics_registered() {
        let m = metrics();
        m.change_tracking_duration_seconds
            .with_label_values(&["gitDiff"])
            .observe(0.002);
        m.change_tracking_snapshot_bytes
            .with_label_values(&["json"])
            .observe(4096.0);
        m.judge_calls_total.with_label_values(&["ok"]).inc();
        m.judge_tokens_total
            .with_label_values(&["input"])
            .inc_by(1234);
        let text = gather_text();
        assert!(text.contains("crw_change_tracking_duration_seconds"));
        assert!(text.contains("crw_change_tracking_snapshot_bytes"));
        assert!(text.contains("crw_judge_calls_total"));
        assert!(text.contains("crw_judge_tokens_total"));
    }
}
