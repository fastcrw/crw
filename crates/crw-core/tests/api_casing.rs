//! Convention guard: every field a public API response serializes MUST be
//! camelCase (see `CONVENTIONS.md`).
//!
//! This test serializes the primary response types and asserts no JSON key is
//! snake_case (contains `_`). It exists because the historical `error_code`
//! (now `errorCode`) slipped through — the OpenAPI drift check only proves the
//! spec matches the code, not that the convention holds.
//!
//! Out of scope by construction: the dynamic `metadata` meta-tag map
//! (`og:site_name`, `twitter:creator`, …) holds verbatim external keys, not
//! typed fields, and is left empty here.
//!
//! When you add a new public response type, add an instance to `camel_samples`.

use crw_core::types::{ApiResponse, LlmUsage, PageMetadata, ScrapeData};
use serde_json::Value;

/// Recursively collect every object key in a JSON value.
fn collect_keys(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                out.push(k.clone());
                collect_keys(val, out);
            }
        }
        Value::Array(arr) => arr.iter().for_each(|x| collect_keys(x, out)),
        _ => {}
    }
}

/// Keys that are snake_case (an underscore between word chars). Intentional
/// camelCase like `sourceURL` / `numPages` has no underscore, so it passes.
fn snake_case_keys(v: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    collect_keys(v, &mut keys);
    keys.into_iter().filter(|k| k.contains('_')).collect()
}

/// A fully-populated ScrapeData so every optional nested field is exercised.
fn sample_scrape_data() -> ScrapeData {
    ScrapeData {
        markdown: Some("# Hi".into()),
        source_hash: Some("abc".into()),
        html: Some("<p>hi</p>".into()),
        raw_html: Some("<html></html>".into()),
        plain_text: Some("hi".into()),
        links: Some(vec!["https://example.com".into()]),
        images: Some(vec![crw_core::types::ScrapedImage {
            url: "https://example.com/a.png".into(),
            alt: Some("a".into()),
        }]),
        json: None,
        summary: Some("s".into()),
        llm_usage: Some(LlmUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            estimated_cost_usd: Some(0.001),
            model: "m".into(),
            provider: "p".into(),
            cache_hit_input_tokens: Some(1),
            cache_miss_input_tokens: Some(9),
            truncated: true,
            calls: 2,
            executed_summaries: 1,
            answer_executed: true,
        }),
        chunks: None,
        warning: Some("w".into()),
        warnings: vec!["w2".into()],
        render_decision: None,
        credit_cost: 1,
        basis: None,
        basis_warnings: Vec::new(),
        llm_input_hash: None,
        metadata: PageMetadata {
            title: Some("t".into()),
            description: Some("d".into()),
            og_title: Some("ot".into()),
            og_description: Some("od".into()),
            og_image: Some("oi".into()),
            canonical_url: Some("https://example.com".into()),
            source_url: "https://example.com".into(),
            final_url: None,
            language: Some("en".into()),
            status_code: 200,
            rendered_with: Some("http".into()),
            elapsed_ms: 42,
            page_count: Some(3),
            source_filename: Some("f.pdf".into()),
            extra: Default::default(),
        },
        debug_extraction: None,
        content_type: Some("text/html".into()),
        change_tracking: None,
        screenshot: Some("data:image/png;base64,AAAA".into()),
        block: None,
        unclearable_wall: true,
        truncated: false,
        cached: false,
    }
}

#[test]
fn api_response_error_shape_is_camel_case() {
    let resp: ApiResponse<()> = ApiResponse {
        success: false,
        data: None,
        error: Some("boom".into()),
        error_code: Some("timeout".into()),
        code: None,
        warning: None,
    };
    let v = serde_json::to_value(&resp).unwrap();
    // The historical snake_case `error_code` must serialize as `errorCode`.
    assert_eq!(v["errorCode"], "timeout");
    assert!(v.get("error_code").is_none(), "legacy snake key leaked");
    assert!(
        snake_case_keys(&v).is_empty(),
        "snake_case keys in error response: {:?}",
        snake_case_keys(&v)
    );
}

#[test]
fn scrape_response_is_camel_case() {
    let resp = ApiResponse {
        success: true,
        data: Some(sample_scrape_data()),
        error: None,
        error_code: None,
        code: None,
        warning: None,
    };
    let v = serde_json::to_value(&resp).unwrap();
    let snakes = snake_case_keys(&v);
    assert!(
        snakes.is_empty(),
        "scrape response serializes snake_case keys (should be camelCase): {:?}",
        snakes
    );
}

/// `unclearable_wall` only steers `/v2`'s verdict inside the engine. If it
/// reached the wire, both the native and the Firecrawl-compatible scrape shapes
/// would gain a key, and the v2 shape must not change.
#[test]
fn unclearable_wall_is_never_serialized() {
    let data = sample_scrape_data();
    assert!(data.unclearable_wall);
    let v = serde_json::to_value(&data).unwrap();
    assert!(v.get("unclearableWall").is_none(), "camelCase key leaked");
    assert!(v.get("unclearable_wall").is_none(), "snake_case key leaked");
}
