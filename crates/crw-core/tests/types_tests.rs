use crw_core::types::*;
use serde_json::json;

#[test]
fn output_format_serde_roundtrip() {
    let variants = vec![
        (OutputFormat::Markdown, "\"markdown\""),
        (OutputFormat::Html, "\"html\""),
        (OutputFormat::RawHtml, "\"rawHtml\""),
        (OutputFormat::PlainText, "\"plainText\""),
        (OutputFormat::Links, "\"links\""),
        (OutputFormat::Json, "\"json\""),
        (OutputFormat::Summary, "\"summary\""),
        (OutputFormat::ChangeTracking, "\"changeTracking\""),
    ];

    for (variant, expected_json) in variants {
        let serialized = serde_json::to_string(&variant).unwrap();
        assert_eq!(
            serialized, expected_json,
            "Serialize failed for {variant:?}"
        );

        let deserialized: OutputFormat = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, variant, "Deserialize failed for {variant:?}");
    }
}

#[test]
fn scrape_request_default_formats() {
    let req: ScrapeRequest = serde_json::from_str(r#"{"url":"https://example.com"}"#).unwrap();
    assert_eq!(req.url, "https://example.com");
    assert_eq!(req.formats, vec![OutputFormat::Markdown]);
    assert_eq!(req.only_main_content, None);
    assert!(req.render_js.is_none());
    assert!(req.wait_for.is_none());
    assert!(req.include_tags.is_empty());
    assert!(req.exclude_tags.is_empty());
    assert!(req.json_schema.is_none());
    assert!(req.headers.is_empty());
}

#[test]
fn api_response_ok_serialization() {
    let resp = ApiResponse::ok("hello");
    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["success"], true);
    assert_eq!(json["data"], "hello");
    assert!(json.get("error").is_none());
    assert!(json.get("warning").is_none());
}

#[test]
fn api_response_err_serialization() {
    let resp = ApiResponse::<()>::err("something went wrong");
    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["success"], false);
    assert!(json.get("data").is_none());
    assert_eq!(json["error"], "something went wrong");
    assert!(json.get("warning").is_none());
}

#[test]
fn crawl_status_serde_rename() {
    let cases = vec![
        (CrawlStatus::InProgress, "\"scraping\""),
        (CrawlStatus::Completed, "\"completed\""),
        (CrawlStatus::Failed, "\"failed\""),
    ];

    for (status, expected) in cases {
        let serialized = serde_json::to_string(&status).unwrap();
        assert_eq!(serialized, expected);
        let deserialized: CrawlStatus = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, status);
    }
}

#[test]
fn page_metadata_skip_serializing_none() {
    let meta = PageMetadata {
        title: Some("Test".into()),
        description: None,
        og_title: None,
        og_description: None,
        og_image: None,
        canonical_url: None,
        source_url: "https://example.com".into(),
        final_url: None,
        language: None,
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 100,
        page_count: None,
        source_filename: None,
        extra: Default::default(),
    };

    let json = serde_json::to_value(&meta).unwrap();
    assert_eq!(json["title"], "Test");
    // None fields with skip_serializing_if should not be present
    assert!(json.get("ogTitle").is_none());
    assert!(json.get("ogDescription").is_none());
    assert!(json.get("ogImage").is_none());
    assert!(json.get("canonicalUrl").is_none());
    assert!(json.get("language").is_none());
    assert!(json.get("renderedWith").is_none());
    // Required fields always present
    assert_eq!(json["sourceURL"], "https://example.com");
    assert_eq!(json["statusCode"], 200);
}

#[test]
fn page_metadata_extra_flattens_and_roundtrips() {
    let mut extra = std::collections::BTreeMap::new();
    extra.insert(
        "twitter:creator".to_string(),
        serde_json::Value::String("@x".into()),
    );
    let meta = PageMetadata {
        title: Some("T".into()),
        description: None,
        og_title: None,
        og_description: None,
        og_image: None,
        canonical_url: None,
        source_url: "https://example.com".into(),
        final_url: None,
        language: None,
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 0,
        page_count: None,
        source_filename: None,
        extra,
    };
    let json = serde_json::to_value(&meta).unwrap();
    // Extra meta tags flatten onto the metadata object (Firecrawl-style), not
    // nested under an "extra" key.
    assert_eq!(json["twitter:creator"], "@x");
    assert!(json.get("extra").is_none());
    // And unknown top-level keys deserialize back into `extra`.
    let back: PageMetadata = serde_json::from_value(json).unwrap();
    assert_eq!(
        back.extra.get("twitter:creator"),
        Some(&serde_json::Value::String("@x".into()))
    );
}

#[test]
fn crawl_request_default_formats() {
    let req: CrawlRequest = serde_json::from_str(r#"{"url":"https://example.com"}"#).unwrap();
    assert_eq!(req.formats, vec![OutputFormat::Markdown]);
    assert_eq!(req.only_main_content, None);
    assert!(req.max_depth.is_none());
    assert!(req.max_pages.is_none());
}

#[test]
fn crawl_request_accepts_limit_alias() {
    let req: CrawlRequest =
        serde_json::from_str(r#"{"url":"https://example.com","limit":3}"#).unwrap();
    assert_eq!(req.max_pages, Some(3));
}

#[test]
fn crawl_start_response_serialization() {
    let resp = CrawlStartResponse {
        success: true,
        id: "abc-123".into(),
    };
    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["success"], true);
    assert_eq!(json["id"], "abc-123");
}

#[test]
fn map_request_defaults() {
    let req: MapRequest = serde_json::from_str(r#"{"url":"https://example.com"}"#).unwrap();
    assert!(req.use_sitemap);
    assert!(req.crawl_fallback);
    assert!(req.max_depth.is_none());
}

#[test]
fn scrape_data_skip_serializing_none() {
    let data = ScrapeData {
        markdown: Some("# Hello".into()),
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
        warnings: Vec::new(),
        render_decision: None,
        credit_cost: 0,
        basis: None,
        basis_warnings: Vec::new(),
        llm_input_hash: None,
        metadata: PageMetadata {
            title: None,
            description: None,
            og_title: None,
            og_description: None,
            og_image: None,
            canonical_url: None,
            source_url: "https://example.com".into(),
            final_url: None,
            language: None,
            status_code: 200,
            rendered_with: None,
            elapsed_ms: 50,
            page_count: None,
            source_filename: None,
            extra: Default::default(),
        },
        debug_extraction: None,
        content_type: None,
        change_tracking: None,
        screenshot: None,
        block: None,
        truncated: false,
        cached: false,
    };

    let json = serde_json::to_value(&data).unwrap();
    assert_eq!(json["markdown"], "# Hello");
    assert!(json.get("html").is_none());
    assert!(json.get("rawHtml").is_none());
    assert!(json.get("plainText").is_none());
    assert!(json.get("links").is_none());
    assert!(json.get("json").is_none());
}

#[test]
fn scrape_request_with_all_fields() {
    let input = json!({
        "url": "https://example.com",
        "formats": ["markdown", "html", "links"],
        "onlyMainContent": false,
        "renderJs": true,
        "waitFor": 2000,
        "includeTags": ["article"],
        "excludeTags": ["nav"],
        "jsonSchema": {"type": "object"},
        "headers": {"X-Custom": "value"}
    });

    let req: ScrapeRequest = serde_json::from_value(input).unwrap();
    assert_eq!(req.formats.len(), 3);
    assert_eq!(req.only_main_content, Some(false));
    assert_eq!(req.render_js, Some(true));
    assert_eq!(req.wait_for, Some(2000));
    assert_eq!(req.include_tags, vec!["article"]);
    assert_eq!(req.exclude_tags, vec!["nav"]);
    assert!(req.json_schema.is_some());
    assert_eq!(req.headers.get("X-Custom").unwrap(), "value");
}

#[test]
fn debug_field_round_trip() {
    let input = json!({
        "url": "https://example.com",
        "debug": true,
    });
    let req: ScrapeRequest = serde_json::from_value(input).unwrap();
    assert_eq!(req.debug, Some(true));
    let req_default: ScrapeRequest =
        serde_json::from_value(json!({"url": "https://example.com"})).unwrap();
    assert_eq!(req_default.debug, None);
}

#[test]
fn debug_extraction_camel_case_wire_format() {
    let de = DebugExtraction {
        attempts: vec![DebugAttempt {
            renderer: "http".into(),
            extracted_via: "readability".into(),
            candidate_features: Some(json!({"linkDensity": 0.42})),
            candidates: vec![DebugCandidate {
                kind: "readability".into(),
                text: Some("body".into()),
                text_excerpt: Some("body".into()),
                cap_chars: Some(200),
                score: 0.7,
            }],
        }],
    };
    let v = serde_json::to_value(&de).unwrap();
    let attempt = &v["attempts"][0];
    assert!(
        attempt.get("extractedVia").is_some(),
        "wire key must be camelCase"
    );
    assert!(attempt.get("candidateFeatures").is_some());
    let cand = &attempt["candidates"][0];
    assert!(cand.get("textExcerpt").is_some());
    assert!(cand.get("capChars").is_some());
}

#[test]
fn scrape_data_serializes_debug_extraction_as_camel_case() {
    let mut data = ScrapeData {
        markdown: None,
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
        warnings: Vec::new(),
        render_decision: None,
        credit_cost: 0,
        basis: None,
        basis_warnings: Vec::new(),
        llm_input_hash: None,
        metadata: PageMetadata {
            title: None,
            description: None,
            og_title: None,
            og_description: None,
            og_image: None,
            canonical_url: None,
            source_url: "https://example.com".into(),
            final_url: None,
            language: None,
            status_code: 200,
            rendered_with: None,
            elapsed_ms: 0,
            page_count: None,
            source_filename: None,
            extra: Default::default(),
        },
        debug_extraction: None,
        content_type: None,
        change_tracking: None,
        screenshot: None,
        block: None,
        truncated: false,
        cached: false,
    };
    let v = serde_json::to_value(&data).unwrap();
    assert!(v.get("debugExtraction").is_none(), "absent when None");
    data.debug_extraction = Some(DebugExtraction::default());
    let v = serde_json::to_value(&data).unwrap();
    assert!(v.get("debugExtraction").is_some(), "present when Some");
}

// ── Change-tracking wire-shape locks (Firecrawl parity) ────────────────────

#[test]
fn change_tracking_format_deserialize_aliases() {
    // Both "changeTracking" and "change-tracking" decode to the same variant.
    let a: OutputFormat = serde_json::from_str("\"changeTracking\"").unwrap();
    let b: OutputFormat = serde_json::from_str("\"change-tracking\"").unwrap();
    assert_eq!(a, OutputFormat::ChangeTracking);
    assert_eq!(b, OutputFormat::ChangeTracking);
}

#[test]
fn change_tracking_mode_deserialize_aliases() {
    let g1: ChangeTrackingMode = serde_json::from_str("\"gitDiff\"").unwrap();
    let g2: ChangeTrackingMode = serde_json::from_str("\"git-diff\"").unwrap();
    let j: ChangeTrackingMode = serde_json::from_str("\"json\"").unwrap();
    assert_eq!(g1, ChangeTrackingMode::GitDiff);
    assert_eq!(g2, ChangeTrackingMode::GitDiff);
    assert_eq!(j, ChangeTrackingMode::Json);
    // Serialize emits the canonical token.
    assert_eq!(serde_json::to_string(&g1).unwrap(), "\"gitDiff\"");
    assert_eq!(serde_json::to_string(&j).unwrap(), "\"json\"");
}

#[test]
fn judgment_wire_shape_matches_firecrawl() {
    // Exactly {meaningful, confidence, reason, meaningfulChanges}; confidence is
    // the string enum "high"/"medium"/"low"; meaningfulChanges are objects;
    // llm_usage is internal-only and never serialized.
    let j = ChangeJudgment {
        meaningful: true,
        confidence: ChangeConfidence::High,
        reason: "Starter price changed".into(),
        meaningful_changes: vec![MeaningfulChange {
            change_type: "changed".into(),
            before: Some("$19/mo".into()),
            after: Some("$24/mo".into()),
            reason: "The Starter plan price changed.".into(),
        }],
        llm_usage: None,
    };
    let v = serde_json::to_value(&j).unwrap();
    assert_eq!(v["confidence"], json!("high"));
    assert_eq!(v["meaningful"], json!(true));
    assert!(v.get("meaningfulChanges").is_some(), "camelCase key");
    assert_eq!(v["meaningfulChanges"][0]["type"], json!("changed"));
    assert_eq!(v["meaningfulChanges"][0]["after"], json!("$24/mo"));
    assert!(v.get("llmUsage").is_none(), "llm_usage must not serialize");
    assert!(v.get("llm_usage").is_none());
}

#[test]
fn change_tracking_result_diff_envelope_shape() {
    // Markdown (gitDiff) mode: diff.json carries the parse-diff AST (has `files`).
    let result = ChangeTrackingResult {
        status: ChangeStatus::Changed,
        first_observation: false,
        content_hash: "abc".into(),
        snapshot: Some(ChangeTrackingSnapshot {
            markdown: Some("Starter $24".into()),
            json: None,
            content_hash: "abc".into(),
            captured_at: None,
        }),
        diff: Some(ChangeDiff {
            text: Some("--- previous\n+++ current\n".into()),
            json: Some(json!({"files": [], "additions": 1, "deletions": 1})),
        }),
        judgment: None,
        tag: Some("target-1".into()),
        truncated: false,
    };
    let v = serde_json::to_value(&result).unwrap();
    assert_eq!(v["status"], json!("changed"));
    assert_eq!(v["firstObservation"], json!(false));
    assert!(v["diff"]["text"].is_string());
    assert!(v["diff"]["json"]["files"].is_array());
    assert_eq!(v["tag"], json!("target-1"));
    // round-trips back
    let back: ChangeTrackingResult = serde_json::from_value(v).unwrap();
    assert_eq!(back.status, ChangeStatus::Changed);
}

#[test]
fn parsers_accepts_string_and_object_forms() {
    use crw_core::types::ScrapeRequest;
    // String form: parsers: ["pdf"]
    let req: ScrapeRequest =
        serde_json::from_str(r#"{"url":"https://x.com","parsers":["pdf"]}"#).unwrap();
    let parsers = req.parsers.unwrap();
    assert_eq!(parsers.len(), 1);
    assert_eq!(parsers[0].parser_type, "pdf");
    assert_eq!(parsers[0].max_pages, None);

    // Object form: parsers: [{"type":"pdf","maxPages":3}]
    let req2: ScrapeRequest =
        serde_json::from_str(r#"{"url":"https://x.com","parsers":[{"type":"pdf","maxPages":3}]}"#)
            .unwrap();
    let p2 = req2.parsers.unwrap();
    assert_eq!(p2[0].parser_type, "pdf");
    assert_eq!(p2[0].max_pages, Some(3));

    // Omitted → None (auto-parse default).
    let req3: ScrapeRequest = serde_json::from_str(r#"{"url":"https://x.com"}"#).unwrap();
    assert!(req3.parsers.is_none());
}
