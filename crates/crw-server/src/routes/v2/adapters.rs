//! Response shapers: internal engine types → Firecrawl v2 wire shapes.
//!
//! These are pure functions over the existing `ScrapeData` / `CrawlState` so the
//! v1 wire shapes stay untouched — every v2-only field (`metadata.proxyUsed`,
//! `cacheState`, `creditsUsed`, `scrapeId`, crawl `next`/`expiresAt`) is
//! synthesized here, not added to the core types.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crw_core::types::{ChangeTrackingResult, CrawlState, CrawlStatus, LlmUsage, ScrapeData};

/// Firecrawl v2 `Document`. Field order/casing matches the live API.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct V2Document {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_html: Option<String>,
    /// Inside a Document, `links` is a flat string array (only `/v2/map` returns
    /// link objects — see `V2Link`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<String>>,
    /// Firecrawl-compat: `images` is a flat array of URL strings (the native
    /// `/v1` surface returns `{url, alt}` objects; we flatten to URLs here).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_tracking: Option<ChangeTrackingResult>,
    /// Page screenshot as a `data:image/png;base64,<...>` URL (Firecrawl-compat
    /// `screenshot` field). Present only when the `screenshot` format was asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// Token usage for any LLM call this scrape triggered.
    ///
    /// NOT a Firecrawl field, and it is here for money. The SaaS prices a
    /// managed-LLM scrape by reserving a worst-case estimate up front and
    /// settling it against this telemetry; with the field absent it cannot
    /// settle, so it holds the reserve and the caller pays the worst case on
    /// every request. `/v1` has always carried it (`ScrapeData::llm_usage`)
    /// and `/v2` dropped it silently here — so the same page, same schema, cost
    /// 5 credits on v1 and 91 on v2.
    ///
    /// That hit real customers hardest: `next.config.ts` serves this surface at
    /// the domain root so the firecrawl-py SDK is a drop-in target, which means
    /// every migrating Firecrawl user lands on v2 by default.
    ///
    /// `skip_serializing_if` keeps it invisible on non-LLM scrapes, so the
    /// frozen Firecrawl shape is unchanged for callers who never trigger one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_usage: Option<LlmUsage>,
    pub metadata: V2Metadata,
}

/// Firecrawl v2 `Document.metadata`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct V2Metadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(rename = "sourceURL")]
    pub source_url: String,
    pub url: String,
    pub status_code: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Resolved proxy tier ("basic" | "stealth"). v2 always reports one.
    pub proxy_used: String,
    /// crw has no read-through cache yet — always "miss".
    pub cache_state: String,
    /// Firecrawl-compat: whether the request was throttled by a concurrency
    /// cap. A self-host engine doesn't concurrency-limit individual requests,
    /// so this is always `false`.
    pub concurrency_limited: bool,
    pub credits_used: u32,
    pub scrape_id: String,
    /// Page count for paginated documents (PDF). Omitted for web pages.
    /// Serialized as `numPages` to match Firecrawl.
    #[serde(rename = "numPages", skip_serializing_if = "Option::is_none")]
    pub page_count: Option<usize>,
    /// Original filename for uploaded documents (via /v2/parse). Omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_filename: Option<String>,
}

/// Map an engine `ScrapeData` to a v2 `Document`. `proxy_used` is the resolved
/// proxy tier; `scrape_id` is a per-document UUID.
pub fn to_v2_document(data: ScrapeData, proxy_used: &str, scrape_id: String) -> V2Document {
    let m = &data.metadata;
    let metadata = V2Metadata {
        title: m.title.clone(),
        description: m.description.clone(),
        language: m.language.clone(),
        source_url: m.source_url.clone(),
        url: m.source_url.clone(),
        status_code: m.status_code,
        content_type: data.content_type.clone(),
        proxy_used: proxy_used.to_string(),
        cache_state: "miss".to_string(),
        concurrency_limited: false,
        // Engine does not price requests (the SaaS layer bills); surface
        // whatever the engine attributed, defaulting to 1 like the live API.
        // A blocked page is 0: nobody is charged for it, the envelope total at
        // `build_crawl_status` already excludes it, and this field disagreeing
        // was the only place a refused page still advertised a credit.
        credits_used: match (data.block.is_some(), data.credit_cost) {
            (true, _) => 0,
            (false, 0) => 1,
            (false, c) => c,
        },
        scrape_id,
        page_count: m.page_count,
        source_filename: m.source_filename.clone(),
    };
    V2Document {
        markdown: data.markdown,
        html: data.html,
        raw_html: data.raw_html,
        links: data.links,
        // Flatten native {url, alt} objects to Firecrawl's flat string[] of URLs.
        images: data
            .images
            .map(|imgs| imgs.into_iter().map(|i| i.url).collect()),
        json: data.json,
        summary: data.summary,
        change_tracking: data.change_tracking,
        screenshot: data.screenshot,
        // `V2Document` has no `block`, so a URL retained only as a placeholder
        // would otherwise reach a /v2 caller as an empty document with nothing
        // to explain it. `warning` is part of the frozen shape and is exactly
        // where Firecrawl surfaces a per-document problem.
        warning: data
            .warning
            .or_else(|| data.block.as_ref().map(|b| b.reason.clone())),
        llm_usage: data.llm_usage,
        metadata,
    }
}

/// Firecrawl v2 crawl / batch-scrape status. Shared shape for `GET /v2/crawl/{id}`
/// and `GET /v2/batch/scrape/{id}` (the live API returns an identical envelope).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct V2CrawlStatus {
    pub success: bool,
    pub status: &'static str,
    pub total: u32,
    pub completed: u32,
    /// How many of `completed` came back a block or an origin error page.
    /// Additive to the Firecrawl envelope (their SDKs ignore unknown keys) and
    /// load-bearing: the SaaS bills off `completed`, and without this the
    /// blocked-page billing fix would be inert on every `/v2` status surface.
    pub blocked: u32,
    pub credits_used: u32,
    pub expires_at: String,
    /// Pagination cursor; `null` once the job is `completed` and no further
    /// pages remain.
    pub next: Option<String>,
    pub data: Vec<V2Document>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn status_str(s: CrawlStatus) -> &'static str {
    match s {
        CrawlStatus::InProgress => "scraping",
        CrawlStatus::Completed => "completed",
        CrawlStatus::Failed => "failed",
        CrawlStatus::Cancelled => "cancelled",
    }
}

/// Default page size for crawl/batch status pagination (documents per page).
pub const DEFAULT_PAGE_LIMIT: usize = 100;
/// Soft byte cap per status page. We stop adding documents to a page once the
/// accumulated markdown/html bytes exceed this, so a completed large crawl
/// paginates instead of serializing one oversized response (Firecrawl uses
/// ~10 MiB; we mirror it).
pub const PAGE_BYTE_CAP: usize = 10 * 1024 * 1024;

/// Build a v2 status response from a `CrawlState` snapshot, paginating from a
/// 0-based document offset `skip`.
///
/// `path_prefix` is `/v2/crawl` or `/v2/batch/scrape`; `base` is the
/// scheme+host the `next` URL should use (caller derives it from the inbound
/// `Host` header or a configured public base).
#[allow(clippy::too_many_arguments)]
pub fn build_crawl_status(
    state: &CrawlState,
    created_at: Instant,
    job_ttl_secs: u64,
    skip: usize,
    limit: usize,
    base: &str,
    path_prefix: &str,
    id: Uuid,
    proxy_used: &str,
) -> V2CrawlStatus {
    let total_docs = state.data.len();
    let limit = limit.max(1);

    // Slice [skip, skip+limit) with a soft byte cap so a single page can't grow
    // unbounded.
    let mut docs = Vec::new();
    let mut bytes = 0usize;
    let mut emitted = 0usize;
    if skip < total_docs {
        for d in state.data[skip..].iter().take(limit) {
            let doc_bytes = d.markdown.as_ref().map(String::len).unwrap_or(0)
                + d.html.as_ref().map(String::len).unwrap_or(0)
                + d.raw_html.as_ref().map(String::len).unwrap_or(0);
            if emitted > 0 && bytes + doc_bytes > PAGE_BYTE_CAP {
                break;
            }
            bytes += doc_bytes;
            emitted += 1;
            let sid = Uuid::new_v4().to_string();
            docs.push(to_v2_document(d.clone(), proxy_used, sid));
        }
    }

    let next_skip = skip + emitted;
    // Emit `next` when more buffered pages remain, OR while the job is still
    // running (so the SDK keeps polling forward even at a momentary page edge).
    let more_buffered = next_skip < total_docs;
    let running = matches!(state.status, CrawlStatus::InProgress);
    let next = if more_buffered || running {
        Some(format!("{base}{path_prefix}/{id}?skip={next_skip}"))
    } else {
        None
    };

    // A blocked page is not billed, so it must not be counted here either. A
    // batch URL whose scrape returns `Err` now pushes a placeholder carrying
    // `block` and bumps `blocked`, so it is excluded here for the same reason a
    // wall is, and `completed - blocked` no longer over-counts it. The two are
    // still not identical: this sum prices a multi-page PDF per page, while
    // `completed - blocked` counts documents.
    let credits_used: u32 = state
        .data
        .iter()
        .filter(|d| d.block.is_none())
        .map(|d| if d.credit_cost == 0 { 1 } else { d.credit_cost })
        .sum();

    V2CrawlStatus {
        success: !matches!(state.status, CrawlStatus::Failed),
        status: status_str(state.status),
        total: state.total.max(total_docs as u32),
        completed: state.completed,
        blocked: state.blocked,
        credits_used,
        expires_at: expires_at_rfc3339(created_at, job_ttl_secs),
        next,
        data: docs,
        error: state.error.clone(),
    }
}

/// Job expiry as an RFC3339 UTC timestamp: `now + (ttl − elapsed)`.
pub fn expires_at_rfc3339(created_at: Instant, job_ttl_secs: u64) -> String {
    let remaining = job_ttl_secs.saturating_sub(created_at.elapsed().as_secs());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339_utc(now + remaining)
}

/// Format a persisted absolute job expiry. Unlike `expires_at_rfc3339`, this
/// does not derive a new wall-clock value at read time.
pub fn system_time_rfc3339(expires_at: SystemTime) -> String {
    let unix_secs = expires_at
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    rfc3339_utc(unix_secs)
}

/// Format a Unix-epoch second count as `YYYY-MM-DDTHH:MM:SS.000Z` (UTC).
/// Hand-rolled (Howard Hinnant's `civil_from_days`) to avoid a chrono/time
/// dependency.
pub fn rfc3339_utc(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let sod = unix_secs % 86_400;
    let (hh, mm, ss) = (sod / 3600, (sod % 3600) / 60, sod % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let mth = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if mth <= 2 { y + 1 } else { y };

    format!("{year:04}-{mth:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.000Z")
}

/// Firecrawl v2 `/v2/map` link object.
#[derive(Debug, Serialize)]
pub struct V2Link {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crw_core::types::PageMetadata;

    /// A managed-LLM scrape on /v2 must carry its token usage.
    ///
    /// The SaaS settles a worst-case credit reserve against this field. When
    /// `to_v2_document` dropped it, the settle had nothing to settle against
    /// and every v2 caller paid the worst case forever: the same page and
    /// schema billed 5 credits on /v1 and 91 on /v2. Because
    /// `next.config.ts` serves /v2 at the domain root for firecrawl-py
    /// compatibility, that was every migrating Firecrawl customer.
    #[test]
    fn v2_document_carries_llm_usage_for_the_saas_to_bill_on() {
        let mut data = fake_doc("https://example.com/p");
        data.json = Some(serde_json::json!({"name": "x"}));
        data.llm_usage = Some(LlmUsage {
            input_tokens: 631,
            output_tokens: 45,
            total_tokens: 676,
            estimated_cost_usd: None,
            model: "DeepSeek-V4-Pro".to_string(),
            provider: "openai-compatible".to_string(),
            cache_hit_input_tokens: None,
            cache_miss_input_tokens: None,
            truncated: false,
            calls: 1,
            executed_summaries: 0,
            answer_executed: false,
        });

        let doc = to_v2_document(data, "basic", "sid".to_string());
        let usage = doc
            .llm_usage
            .as_ref()
            .expect("llm_usage must survive the v2 mapping");
        assert_eq!(usage.input_tokens, 631);
        assert_eq!(usage.output_tokens, 45);

        // And it must reach the wire under the key the SaaS reads.
        let wire = serde_json::to_value(&doc).unwrap();
        assert_eq!(wire["llmUsage"]["inputTokens"], 631);
        assert_eq!(wire["llmUsage"]["outputTokens"], 45);
    }

    /// The frozen Firecrawl shape is unchanged when no LLM ran: `skip_serializing_if`
    /// must keep the key out entirely rather than emitting `"llmUsage": null`.
    #[test]
    fn v2_document_omits_llm_usage_when_no_llm_ran() {
        let doc = to_v2_document(
            fake_doc("https://example.com/p"),
            "basic",
            "sid".to_string(),
        );
        let wire = serde_json::to_value(&doc).unwrap();
        assert!(
            wire.get("llmUsage").is_none(),
            "non-LLM scrapes must not grow a new key"
        );
    }

    #[test]
    fn rfc3339_matches_known_epoch() {
        // Unix epoch.
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00.000Z");
        // Widely-referenced round value: 1_700_000_000 == 2023-11-14T22:13:20Z.
        assert_eq!(rfc3339_utc(1_700_000_000), "2023-11-14T22:13:20.000Z");
    }

    fn fake_doc(url: &str) -> ScrapeData {
        ScrapeData {
            markdown: Some("# hi".to_string()),
            source_hash: None,
            html: None,
            raw_html: None,
            plain_text: None,
            links: None,
            images: None,
            json: None,
            summary: None,
            llm_usage: None,
            chunks: None,
            warning: None,
            warnings: vec![],
            render_decision: None,
            credit_cost: 1,
            basis: None,
            basis_warnings: Vec::new(),
            llm_input_hash: None,
            metadata: PageMetadata {
                title: Some("T".into()),
                description: None,
                og_title: None,
                og_description: None,
                og_image: None,
                canonical_url: None,
                source_url: url.to_string(),
                language: None,
                status_code: 200,
                rendered_with: None,
                elapsed_ms: 0,
                page_count: None,
                source_filename: None,
                extra: Default::default(),
            },
            debug_extraction: None,
            content_type: Some("text/html".into()),
            change_tracking: None,
            screenshot: None,
            block: None,
            truncated: false,
        }
    }

    fn state(status: CrawlStatus, total: u32, completed: u32, n: usize) -> CrawlState {
        CrawlState {
            id: Uuid::nil(),
            success: true,
            status,
            total,
            completed,
            blocked: 0,
            data: (0..n)
                .map(|i| fake_doc(&format!("https://x/{i}")))
                .collect(),
            error: None,
        }
    }

    #[test]
    fn pagination_skip_next_and_credits() {
        let s = state(CrawlStatus::Completed, 250, 250, 250);
        let now = Instant::now();

        let p0 = build_crawl_status(
            &s,
            now,
            86_400,
            0,
            100,
            "https://api.example",
            "/v2/crawl",
            Uuid::nil(),
            "basic",
        );
        assert_eq!(p0.data.len(), 100);
        assert_eq!(p0.total, 250);
        assert_eq!(p0.completed, 250);
        assert_eq!(p0.credits_used, 250);
        assert_eq!(
            p0.next.as_deref(),
            Some("https://api.example/v2/crawl/00000000-0000-0000-0000-000000000000?skip=100")
        );

        // Last page of a completed job → next is null.
        let p2 = build_crawl_status(
            &s,
            now,
            86_400,
            200,
            100,
            "https://api.example",
            "/v2/crawl",
            Uuid::nil(),
            "basic",
        );
        assert_eq!(p2.data.len(), 50);
        assert!(p2.next.is_none());

        // skip past the end → empty page, next null.
        let p3 = build_crawl_status(
            &s,
            now,
            86_400,
            300,
            100,
            "https://api.example",
            "/v2/crawl",
            Uuid::nil(),
            "basic",
        );
        assert_eq!(p3.data.len(), 0);
        assert!(p3.next.is_none());
    }

    #[test]
    fn running_job_emits_next_even_at_buffer_edge() {
        // 10 buffered docs, job still running (total unknown-ish at 50).
        let s = state(CrawlStatus::InProgress, 50, 10, 10);
        let p = build_crawl_status(
            &s,
            Instant::now(),
            86_400,
            0,
            100,
            "https://b",
            "/v2/crawl",
            Uuid::nil(),
            "basic",
        );
        assert_eq!(p.data.len(), 10);
        assert_eq!(p.status, "scraping");
        // SDK must keep polling forward even though we returned all buffered docs.
        assert!(p.next.is_some());
    }

    #[test]
    fn v2_images_flatten_to_url_strings() {
        // Firecrawl-compat: native {url, alt} objects flatten to a flat string[]
        // of URLs on the v2 surface (alt is dropped).
        let mut data = fake_doc("https://example.com");
        data.images = Some(vec![
            crw_core::types::ScrapedImage {
                url: "https://example.com/a.png".into(),
                alt: Some("A".into()),
            },
            crw_core::types::ScrapedImage {
                url: "https://example.com/b.png".into(),
                alt: None,
            },
        ]);
        let doc = to_v2_document(data, "basic", "id".to_string());
        assert_eq!(
            doc.images,
            Some(vec![
                "https://example.com/a.png".to_string(),
                "https://example.com/b.png".to_string(),
            ])
        );
        // Serialized shape is a plain string array, not objects.
        let v = serde_json::to_value(&doc).unwrap();
        assert_eq!(v["images"][0], "https://example.com/a.png");
        assert!(v["images"][0].is_string());
    }

    // ── to_v2_document: field-by-field shape pinning ───────────────────────
    // `/v2` is a deprecated Firecrawl-compat alias whose wire shape must not
    // drift silently, so every field name and every optional-field omission
    // rule is pinned explicitly here.

    #[test]
    fn all_optional_fields_present_round_trip_unchanged() {
        let mut data = fake_doc("https://example.com/full");
        data.html = Some("<p>hi</p>".to_string());
        data.raw_html = Some("<html><p>hi</p></html>".to_string());
        data.links = Some(vec!["https://a".to_string(), "https://b".to_string()]);
        data.json = Some(serde_json::json!({"a": 1}));
        data.summary = Some("a summary".to_string());
        data.warning = Some("careful".to_string());
        data.screenshot = Some("data:image/png;base64,AAAA".to_string());
        data.metadata.description = Some("desc".to_string());
        data.metadata.language = Some("en".to_string());
        data.metadata.page_count = Some(3);
        data.metadata.source_filename = Some("doc.pdf".to_string());
        data.change_tracking = Some(ChangeTrackingResult {
            status: crw_core::types::ChangeStatus::Same,
            first_observation: true,
            content_hash: "h".to_string(),
            snapshot: None,
            diff: None,
            judgment: None,
            tag: None,
            truncated: false,
        });

        let doc = to_v2_document(data, "basic", "sid-1".to_string());
        let v = serde_json::to_value(&doc).unwrap();

        assert_eq!(v["markdown"], "# hi");
        assert_eq!(v["html"], "<p>hi</p>");
        assert_eq!(v["rawHtml"], "<html><p>hi</p></html>");
        assert_eq!(v["links"], serde_json::json!(["https://a", "https://b"]));
        assert_eq!(v["json"], serde_json::json!({"a": 1}));
        assert_eq!(v["summary"], "a summary");
        assert_eq!(v["warning"], "careful");
        assert_eq!(v["screenshot"], "data:image/png;base64,AAAA");
        assert_eq!(v["changeTracking"]["status"], "same");
        assert_eq!(v["metadata"]["description"], "desc");
        assert_eq!(v["metadata"]["language"], "en");
        assert_eq!(v["metadata"]["numPages"], 3);
        assert_eq!(v["metadata"]["sourceFilename"], "doc.pdf");
    }

    #[test]
    fn all_optional_fields_none_are_omitted_from_json() {
        // fake_doc() ships a content_type, so clear it: this test is about the
        // None case being omitted, and the Some case is covered separately by
        // metadata_status_code_and_content_type_pass_through.
        let mut src = fake_doc("https://example.com");
        src.content_type = None;
        let doc = to_v2_document(src, "basic", "sid".to_string());
        let v = serde_json::to_value(&doc).unwrap();
        for key in [
            "html",
            "rawHtml",
            "links",
            "images",
            "json",
            "summary",
            "changeTracking",
            "screenshot",
            "warning",
        ] {
            assert!(
                v.get(key).is_none(),
                "expected `{key}` to be omitted, found {:?}",
                v.get(key)
            );
        }
        for key in [
            "description",
            "language",
            "contentType",
            "numPages",
            "sourceFilename",
        ] {
            assert!(
                v["metadata"].get(key).is_none(),
                "expected metadata.`{key}` to be omitted"
            );
        }
    }

    #[test]
    fn metadata_source_url_is_camel_case_with_capital_url() {
        let doc = to_v2_document(fake_doc("https://x.example"), "basic", "sid".to_string());
        let v = serde_json::to_value(&doc).unwrap();
        // Firecrawl's field is `sourceURL`, not `sourceUrl`.
        assert_eq!(v["metadata"]["sourceURL"], "https://x.example");
        assert!(v["metadata"].get("sourceUrl").is_none());
        assert_eq!(v["metadata"]["url"], "https://x.example");
    }

    #[test]
    fn metadata_status_code_and_content_type_pass_through() {
        let mut data = fake_doc("https://x");
        data.metadata.status_code = 404;
        data.content_type = Some("application/pdf".to_string());
        let doc = to_v2_document(data, "basic", "sid".to_string());
        let v = serde_json::to_value(&doc).unwrap();
        assert_eq!(v["metadata"]["statusCode"], 404);
        assert_eq!(v["metadata"]["contentType"], "application/pdf");
    }

    #[test]
    fn metadata_proxy_used_reflects_the_argument() {
        let doc = to_v2_document(fake_doc("https://x"), "stealth", "sid".to_string());
        assert_eq!(doc.metadata.proxy_used, "stealth");
        let doc2 = to_v2_document(fake_doc("https://x"), "basic", "sid".to_string());
        assert_eq!(doc2.metadata.proxy_used, "basic");
    }

    #[test]
    fn metadata_cache_state_is_always_miss() {
        let doc = to_v2_document(fake_doc("https://x"), "basic", "sid".to_string());
        assert_eq!(doc.metadata.cache_state, "miss");
    }

    #[test]
    fn metadata_concurrency_limited_is_always_false() {
        let doc = to_v2_document(fake_doc("https://x"), "basic", "sid".to_string());
        assert!(!doc.metadata.concurrency_limited);
    }

    #[test]
    fn credits_used_defaults_to_one_when_engine_reports_zero() {
        let mut data = fake_doc("https://x");
        data.credit_cost = 0;
        let doc = to_v2_document(data, "basic", "sid".to_string());
        assert_eq!(doc.metadata.credits_used, 1);
    }

    /// `block` is set on an anti-bot wall, an origin error page, and now on a
    /// URL the crawl could not read at all. None of the three is billed, so
    /// none of them may report a credit on the document either.
    #[test]
    fn a_blocked_document_reports_zero_credits() {
        let mut data = fake_doc("https://x");
        data.credit_cost = 0;
        data.block = Some(crw_core::types::BlockOutcome {
            vendor: crw_core::types::HTTP_ERROR_VENDOR.to_string(),
            reason: "CDN could not reach origin".to_string(),
        });
        let doc = to_v2_document(data, "basic", "sid".to_string());
        assert_eq!(doc.metadata.credits_used, 0);
    }

    #[test]
    fn credits_used_passes_through_nonzero_engine_cost() {
        let mut data = fake_doc("https://x");
        data.credit_cost = 5;
        let doc = to_v2_document(data, "basic", "sid".to_string());
        assert_eq!(doc.metadata.credits_used, 5);
    }

    #[test]
    fn scrape_id_is_exactly_the_supplied_string() {
        let doc = to_v2_document(fake_doc("https://x"), "basic", "unique-id-123".to_string());
        assert_eq!(doc.metadata.scrape_id, "unique-id-123");
    }

    #[test]
    fn empty_images_vec_still_serializes_as_empty_array() {
        // `skip_serializing_if` only fires on `None`, not on an empty `Some(vec![])`.
        let mut data = fake_doc("https://x");
        data.images = Some(vec![]);
        let doc = to_v2_document(data, "basic", "sid".to_string());
        let v = serde_json::to_value(&doc).unwrap();
        assert_eq!(v["images"], serde_json::json!([]));
    }

    #[test]
    fn links_preserve_order() {
        let mut data = fake_doc("https://x");
        data.links = Some(vec!["c".into(), "a".into(), "b".into()]);
        let doc = to_v2_document(data, "basic", "sid".to_string());
        assert_eq!(
            doc.links,
            Some(vec!["c".to_string(), "a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn unicode_image_urls_flatten_correctly() {
        let mut data = fake_doc("https://x");
        data.images = Some(vec![crw_core::types::ScrapedImage {
            url: "https://example.com/日本語.png".into(),
            alt: Some("日本語 alt".into()),
        }]);
        let doc = to_v2_document(data, "basic", "sid".to_string());
        assert_eq!(
            doc.images,
            Some(vec!["https://example.com/日本語.png".to_string()])
        );
    }

    // ── status_str: every CrawlStatus variant pinned ───────────────────────

    #[test]
    fn status_str_covers_every_variant() {
        assert_eq!(status_str(CrawlStatus::InProgress), "scraping");
        assert_eq!(status_str(CrawlStatus::Completed), "completed");
        assert_eq!(status_str(CrawlStatus::Failed), "failed");
        assert_eq!(status_str(CrawlStatus::Cancelled), "cancelled");
    }

    // ── build_crawl_status: success flag, error, totals, blocked pages ─────

    #[test]
    fn success_is_false_only_for_failed_status() {
        for (status, expect_success) in [
            (CrawlStatus::InProgress, true),
            (CrawlStatus::Completed, true),
            (CrawlStatus::Cancelled, true),
            (CrawlStatus::Failed, false),
        ] {
            let s = state(status, 1, 1, 1);
            let p = build_crawl_status(
                &s,
                Instant::now(),
                86_400,
                0,
                10,
                "https://api",
                "/v2/crawl",
                Uuid::nil(),
                "basic",
            );
            assert_eq!(p.success, expect_success, "status={status:?}");
        }
    }

    #[test]
    fn job_error_message_passes_through() {
        let mut s = state(CrawlStatus::Failed, 1, 0, 0);
        s.error = Some("upstream exploded".to_string());
        let p = build_crawl_status(
            &s,
            Instant::now(),
            86_400,
            0,
            10,
            "https://api",
            "/v2/crawl",
            Uuid::nil(),
            "basic",
        );
        assert_eq!(p.error.as_deref(), Some("upstream exploded"));
    }

    #[test]
    fn total_is_the_max_of_reported_total_and_buffered_docs() {
        // More docs buffered than the job's own `total` counter (can happen
        // transiently) — the response must never under-report.
        let s = state(CrawlStatus::InProgress, 3, 5, 5);
        let p = build_crawl_status(
            &s,
            Instant::now(),
            86_400,
            0,
            10,
            "https://api",
            "/v2/crawl",
            Uuid::nil(),
            "basic",
        );
        assert_eq!(p.total, 5);
    }

    #[test]
    fn blocked_pages_are_excluded_from_credits_used_but_still_returned() {
        let mut s = state(CrawlStatus::Completed, 2, 2, 2);
        s.blocked = 1;
        s.data[0].block = Some(crw_core::types::BlockOutcome {
            vendor: "cloudflare".to_string(),
            reason: "challenge".to_string(),
        });
        s.data[0].credit_cost = 1;
        s.data[1].credit_cost = 1;
        let p = build_crawl_status(
            &s,
            Instant::now(),
            86_400,
            0,
            10,
            "https://api",
            "/v2/crawl",
            Uuid::nil(),
            "basic",
        );
        assert_eq!(p.blocked, 1);
        assert_eq!(p.credits_used, 1); // only the non-blocked doc is billed
        assert_eq!(p.data.len(), 2); // both docs are still returned to the caller
    }

    #[test]
    fn limit_zero_is_clamped_to_one() {
        let s = state(CrawlStatus::Completed, 3, 3, 3);
        let p = build_crawl_status(
            &s,
            Instant::now(),
            86_400,
            0,
            0,
            "https://api",
            "/v2/crawl",
            Uuid::nil(),
            "basic",
        );
        assert_eq!(p.data.len(), 1);
    }

    #[test]
    fn next_url_uses_the_given_path_prefix_and_id() {
        let s = state(CrawlStatus::Completed, 5, 5, 5);
        let id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let p = build_crawl_status(
            &s,
            Instant::now(),
            86_400,
            0,
            2,
            "https://custom.host",
            "/v2/batch/scrape",
            id,
            "basic",
        );
        assert_eq!(
            p.next.as_deref(),
            Some("https://custom.host/v2/batch/scrape/11111111-1111-1111-1111-111111111111?skip=2")
        );
    }

    #[test]
    fn running_job_with_skip_past_total_still_emits_next() {
        // Nothing left to buffer, but the job is still running, so the SDK
        // must keep polling forward instead of treating this as terminal.
        let s = state(CrawlStatus::InProgress, 10, 10, 10);
        let p = build_crawl_status(
            &s,
            Instant::now(),
            86_400,
            999,
            10,
            "https://api",
            "/v2/crawl",
            Uuid::nil(),
            "basic",
        );
        assert_eq!(p.data.len(), 0);
        assert!(p.next.is_some());
    }

    #[test]
    fn completed_job_exactly_at_the_end_has_no_next() {
        let s = state(CrawlStatus::Completed, 10, 10, 10);
        let p = build_crawl_status(
            &s,
            Instant::now(),
            86_400,
            10, // == total_docs
            10,
            "https://api",
            "/v2/crawl",
            Uuid::nil(),
            "basic",
        );
        assert_eq!(p.data.len(), 0);
        assert!(p.next.is_none());
    }

    #[test]
    fn byte_cap_still_emits_at_least_one_oversized_document_alone() {
        // `emitted > 0 && bytes + doc_bytes > CAP` only breaks AFTER the first
        // doc, so a single doc bigger than the cap is still returned rather
        // than starving the page entirely.
        let mut s = state(CrawlStatus::Completed, 2, 2, 2);
        s.data[0].markdown = Some("x".repeat(PAGE_BYTE_CAP + 1));
        let p = build_crawl_status(
            &s,
            Instant::now(),
            86_400,
            0,
            10,
            "https://api",
            "/v2/crawl",
            Uuid::nil(),
            "basic",
        );
        assert_eq!(p.data.len(), 1, "oversized doc emitted alone");
    }

    #[test]
    fn byte_cap_stops_before_a_second_doc_that_would_exceed_it() {
        let mut s = state(CrawlStatus::Completed, 2, 2, 2);
        s.data[0].markdown = Some("x".repeat(PAGE_BYTE_CAP));
        s.data[1].markdown = Some("y".repeat(100));
        let p = build_crawl_status(
            &s,
            Instant::now(),
            86_400,
            0,
            10,
            "https://api",
            "/v2/crawl",
            Uuid::nil(),
            "basic",
        );
        assert_eq!(p.data.len(), 1);
    }

    // ── expires_at_rfc3339 / system_time_rfc3339 ────────────────────────────

    const RFC3339_SHAPE: &str = r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.000Z$";

    fn matches_rfc3339_shape(s: &str) -> bool {
        // Hand-rolled check (no regex dependency): fixed-width fields at fixed
        // positions, matching `RFC3339_SHAPE` above.
        let bytes = s.as_bytes();
        bytes.len() == 24
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes[10] == b'T'
            && bytes[13] == b':'
            && bytes[16] == b':'
            && bytes[19] == b'.'
            && &bytes[20..] == b"000Z"
            && bytes[..4].iter().all(u8::is_ascii_digit)
    }

    #[test]
    fn expires_at_elapsed_beyond_ttl_saturates_to_now_not_negative() {
        // `created_at` far enough in the past that `ttl - elapsed` would go
        // negative; `saturating_sub` must floor it at 0 rather than panic or wrap.
        let long_ago = Instant::now() - std::time::Duration::from_secs(10_000);
        let s = expires_at_rfc3339(long_ago, 10);
        assert!(
            matches_rfc3339_shape(&s),
            "{s} does not match {RFC3339_SHAPE}"
        );
    }

    #[test]
    fn expires_at_fresh_job_produces_well_formed_timestamp() {
        let s = expires_at_rfc3339(Instant::now(), 86_400);
        assert!(
            matches_rfc3339_shape(&s),
            "{s} does not match {RFC3339_SHAPE}"
        );
    }

    #[test]
    fn system_time_rfc3339_matches_the_known_epoch() {
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        assert_eq!(system_time_rfc3339(t), "2023-11-14T22:13:20.000Z");
    }

    #[test]
    fn system_time_rfc3339_before_epoch_falls_back_to_epoch_zero() {
        // `duration_since(UNIX_EPOCH)` errors for a time before the epoch;
        // `unwrap_or(0)` must not panic.
        let before_epoch = UNIX_EPOCH - std::time::Duration::from_secs(1);
        assert_eq!(
            system_time_rfc3339(before_epoch),
            "1970-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn rfc3339_leap_day_2000_is_computed_correctly() {
        // 2000 is a leap year (divisible by 400). Independently derived from
        // the well-known anchor 2000-01-01T00:00:00Z == 946684800: +31 days
        // (Jan) +28 days (to reach the 29th) = 951782400.
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn rfc3339_non_leap_year_rolls_feb28_into_mar1() {
        // 2001 is not a leap year. Derived from 2001-01-01T00:00:00Z ==
        // 978307200 (946684800 + 366 days of leap-year 2000).
        assert_eq!(rfc3339_utc(983_318_400), "2001-02-28T00:00:00.000Z");
        assert_eq!(rfc3339_utc(983_404_800), "2001-03-01T00:00:00.000Z");
    }

    #[test]
    fn v2_link_serializes_with_optional_fields_omitted() {
        let link = V2Link {
            url: "https://x".to_string(),
            title: None,
            description: None,
        };
        let v = serde_json::to_value(&link).unwrap();
        assert_eq!(v["url"], "https://x");
        assert!(v.get("title").is_none());
        assert!(v.get("description").is_none());
    }

    #[test]
    fn v2_link_serializes_with_optional_fields_present() {
        let link = V2Link {
            url: "https://x".to_string(),
            title: Some("Title".to_string()),
            description: Some("Desc".to_string()),
        };
        let v = serde_json::to_value(&link).unwrap();
        assert_eq!(v["title"], "Title");
        assert_eq!(v["description"], "Desc");
    }
}
