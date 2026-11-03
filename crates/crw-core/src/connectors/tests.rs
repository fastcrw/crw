//! End-to-end tests of `call_data_tool` against a local server that plays both
//! the fastCRW API and the upstream source.

use super::*;
use base64::Engine;
use std::collections::HashMap;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CRW_KEY: &str = "crw-test-key";
/// A connector key with characters that encode differently in each form.
const USER_KEY: &str = "uk y+1/\"é";
const UA: &str = "crw-test (mailto:dev@example.org)";
/// What our own API sees; the sources see [`UA`] instead.
const API_UA: &str = concat!("crw/", env!("CARGO_PKG_VERSION"));

impl DataToolContext {
    fn for_tests(
        api_url: &str,
        telemetry: bool,
        entries: Vec<(&str, keys::ConnectorEntry)>,
    ) -> Self {
        let entries: HashMap<String, keys::ConnectorEntry> = entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        Self::build(
            api_url.to_string(),
            Some(CRW_KEY.to_string()),
            telemetry,
            client::ConnectorHttp::new(client::Guard {
                test_loopback: true,
            }),
            Box::new(move |slug| entries.get(slug).cloned()),
        )
    }
}

fn connector(server: &MockServer, provider: &str) -> Value {
    json!({
        "provider": provider,
        "name": "OpenAlex",
        "description": "Scholarly works",
        "baseUrl": server.uri(),
        "userAgent": UA,
        "auth": {"type": "none", "param": null},
        "keyRequired": false,
        "keyHelpUrl": "https://openalex.org/settings/api",
        "minIntervalMs": null,
        "licence": "CC0 1.0",
        "licenceUrl": "https://creativecommons.org/publicdomain/zero/1.0/",
        "attributionText": null,
        "capabilities": [{
            "capability": "works",
            "label": null,
            "description": null,
            "pathTemplate": "/up/works/{id}",
            "baseUrlOverride": null,
            "forwardParams": ["page"],
            "parameters": []
        }]
    })
}

fn manifest_body(connectors: Vec<Value>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"schemaVersion": 1, "connectors": connectors}))
}

async fn mount_manifest(server: &MockServer, connectors: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path("/v1/data/manifest"))
        .respond_with(manifest_body(connectors))
        .mount(server)
        .await;
}

fn json_ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.to_string(), "application/json")
}

async fn get(ctx: &DataToolContext, provider: &str, options: Value) -> Result<Value, String> {
    call_data_tool(
        "crw_get_data",
        json!({"provider": provider, "capability": "works", "options": options}),
        ctx,
    )
    .await
}

async fn hits(server: &MockServer, p: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == p)
        .count()
}

fn approved(server: &MockServer, auth: &str) -> keys::ConnectorEntry {
    keys::ConnectorEntry {
        api_key: Some(USER_KEY.to_string()),
        origins: vec![server.uri()],
        auth: Some(auth.to_string()),
    }
}

// --- routing ---

#[tokio::test]
async fn connectors_route_manifest_provider_runs_here() {
    let server = MockServer::start().await;
    mount_manifest(&server, vec![connector(&server, "openalex")]).await;
    Mock::given(method("GET"))
        .and(path("/up/works/W1"))
        .and(query_param("page", "2"))
        .and(header("user-agent", UA))
        .respond_with(json_ok(json!({"id": "W1"})))
        .expect(1)
        .mount(&server)
        .await;

    let out = get(
        &DataToolContext::for_tests(&server.uri(), false, vec![]),
        "openalex",
        json!({"id": "W1", "page": 2}),
    )
    .await
    .unwrap();
    assert_eq!(out["provider"], "openalex");
    assert_eq!(out["capability"], "works");
    assert_eq!(out["creditsUsed"], 0);
    assert_eq!(out["data"], json!({"id": "W1"}));
    assert_eq!(out["provenance"]["source"], "OpenAlex");
    assert_eq!(out["provenance"]["licence"], "CC0 1.0");
    assert!(
        out["provenance"]["upstreamUrl"]
            .as_str()
            .unwrap()
            .ends_with("/up/works/W1?page=2")
    );
    assert!(
        out["provenance"]["retrievedAt"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    assert_eq!(
        hits(&server, "/v1/data/openalex/works").await,
        0,
        "nothing forwarded"
    );
}

#[tokio::test]
async fn connectors_route_absent_provider_is_forwarded_and_mapped() {
    let server = MockServer::start().await;
    mount_manifest(&server, vec![]).await;
    Mock::given(method("GET"))
        .and(path("/v1/data/npm/works"))
        .and(query_param("name", "react"))
        .and(header("user-agent", API_UA))
        .and(header("x-crw-max-data-bytes", "262144"))
        .and(header(
            "authorization",
            format!("Bearer {CRW_KEY}").as_str(),
        ))
        .respond_with(json_ok(json!({
            "success": true, "provider": "npm", "capability": "works", "credits_used": 1,
            "request_id": "r1",
            "provenance": {"source": "npm", "upstream_url": "https://registry.npmjs.org/react",
                "licence": "Varies", "licence_url": null, "attribution_text": null,
                "retrieved_at": "2026-10-29T00:00:00.000Z", "adapter": "rest",
                "cache_age_seconds": 0, "is_stale": false},
            "data": {"name": "react"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ctx = DataToolContext::for_tests(&server.uri(), true, vec![]);
    let out = get(&ctx, "npm", json!({"name": "react"})).await.unwrap();
    assert_eq!(
        out,
        json!({
            "provider": "npm", "capability": "works", "creditsUsed": 1,
            "provenance": {"source": "npm", "upstreamUrl": "https://registry.npmjs.org/react",
                "licence": "Varies", "licenceUrl": null, "attributionText": null,
                "retrievedAt": "2026-10-29T00:00:00.000Z"},
            "data": {"name": "react"}
        })
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        hits(&server, "/v1/data/usage").await,
        0,
        "server-run calls send no ping"
    );
}

#[tokio::test]
async fn connectors_route_forwarded_size_cap_is_the_servers() {
    let server = MockServer::start().await;
    mount_manifest(&server, vec![]).await;
    // A current API refuses an oversized result before it charges.
    Mock::given(path("/v1/data/big/works"))
        .respond_with(ResponseTemplate::new(413).set_body_json(json!({
            "success": false, "errorCode": "result_too_large",
            "error": "[result_too_large] The result is 300 KiB, over the 256 KiB limit. Narrow the options and call again."
        })))
        .mount(&server)
        .await;
    // An older API ignores the header; what it billed is delivered, not dropped.
    let blob = "x".repeat(crate::mcp::MAX_DATA_PAYLOAD_BYTES);
    Mock::given(path("/v1/data/old/works"))
        .respond_with(json_ok(json!({
            "success": true, "provider": "old", "capability": "works", "credits_used": 1,
            "provenance": {}, "data": {"blob": blob}
        })))
        .mount(&server)
        .await;
    let ctx = DataToolContext::for_tests(&server.uri(), false, vec![]);

    let err = get(&ctx, "big", json!({})).await.unwrap_err();
    assert!(
        err.starts_with("[result_too_large] The result is 300 KiB"),
        "{err}"
    );
    let out = get(&ctx, "old", json!({})).await.unwrap();
    assert_eq!(out["creditsUsed"], 1);
    assert_eq!(out["data"]["blob"].as_str().map(str::len), Some(blob.len()));
}

#[tokio::test]
async fn connectors_route_server_error_code_wins_over_the_status() {
    let server = MockServer::start().await;
    mount_manifest(&server, vec![]).await;
    let cases = [
        // An upstream 404 on a server-run source is not an unknown source.
        (
            "up404",
            404,
            json!({"errorCode": "upstream_error", "error": "npm returned HTTP 404 for this request."}),
            "[upstream_error] npm returned HTTP 404 for this request.",
        ),
        (
            "up429",
            429,
            json!({"errorCode": "upstream_rate_limited", "error": "[rate_limited] npm is limiting requests."}),
            "[upstream_rate_limited] npm is limiting requests.",
        ),
        // No errorCode: a bracketed code in the text stays, else the status.
        (
            "kept",
            401,
            json!({"error": "[unauthorized] Invalid API key"}),
            "[unauthorized] Invalid API key",
        ),
        (
            "status",
            404,
            json!({"error": "Unknown integration."}),
            "[unknown_data_source] Unknown integration.",
        ),
        (
            "notcode",
            500,
            json!({"errorCode": "Bad Code!", "error": "[see docs] boom"}),
            "[server_error] [see docs] boom",
        ),
    ];
    for (provider, status, body, _) in &cases {
        Mock::given(path(format!("/v1/data/{provider}/works")))
            .respond_with(ResponseTemplate::new(*status).set_body_json(body))
            .mount(&server)
            .await;
    }
    let ctx = DataToolContext::for_tests(&server.uri(), false, vec![]);
    for (provider, _, _, want) in &cases {
        let err = get(&ctx, provider, json!({})).await.unwrap_err();
        assert_eq!(&err, want, "{provider}");
    }
}

#[tokio::test]
async fn connectors_route_409_refetches_manifest_then_runs_here() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/data/manifest"))
        .respond_with(manifest_body(vec![]))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    mount_manifest(&server, vec![connector(&server, "openalex")]).await;
    Mock::given(path("/v1/data/openalex/works"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "success": false, "request_id": "r",
            "error": "[client_execution_required] This source runs inside the local crw MCP.",
            "errorCode": "client_execution_required"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/up/works/W1"))
        .respond_with(json_ok(json!({"id": "W1"})))
        .expect(1)
        .mount(&server)
        .await;

    let ctx = DataToolContext::for_tests(&server.uri(), false, vec![]);
    ctx.manifest().await.unwrap();
    // A copy under a minute old is not refetched on a 409; this one is older.
    ctx.manifest.age_by(RECHECK_AFTER).await;
    let out = get(&ctx, "openalex", json!({"id": "W1"})).await.unwrap();
    assert_eq!(out["creditsUsed"], 0);
    assert_eq!(hits(&server, "/v1/data/manifest").await, 2);
}

#[tokio::test]
async fn connectors_route_409_without_manifest_entry_asks_for_an_upgrade() {
    // The server runs it here, but this crw dropped it from the manifest: its
    // "install the local MCP" advice would be wrong, and a fresh copy is not
    // refetched on every call.
    let server = MockServer::start().await;
    mount_manifest(&server, vec![]).await;
    Mock::given(path("/v1/data/openalex/works"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "success": false, "error": "This source runs inside the local crw MCP.",
            "errorCode": "client_execution_required"
        })))
        .mount(&server)
        .await;
    let ctx = DataToolContext::for_tests(&server.uri(), false, vec![]);
    for _ in 0..2 {
        let err = get(&ctx, "openalex", json!({"id": "W1"}))
            .await
            .unwrap_err();
        assert!(
            err.starts_with(
                "[connector_misconfigured] openalex cannot run on this version of crw. Upgrade crw ("
            ),
            "{err}"
        );
        assert!(!err.contains("crw-mcp install"), "{err}");
    }
    assert_eq!(hits(&server, "/v1/data/manifest").await, 1);
}

#[tokio::test]
async fn connectors_route_revoked_provider_gets_the_server_404() {
    let server = MockServer::start().await;
    mount_manifest(&server, vec![]).await;
    Mock::given(path("/v1/data/gone/works"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "success": false, "request_id": "r",
            "error": "Unknown integration \"gone/works\". GET /v1/data lists what is available."
        })))
        .mount(&server)
        .await;
    let err = get(
        &DataToolContext::for_tests(&server.uri(), false, vec![]),
        "gone",
        json!({}),
    )
    .await
    .unwrap_err();
    assert!(
        err.starts_with("[unknown_data_source] Unknown integration"),
        "{err}"
    );
}

#[tokio::test]
async fn connectors_route_manifest_unavailable_forwards_nothing() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/data/manifest"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let err = get(
        &DataToolContext::for_tests(&server.uri(), false, vec![]),
        "npm",
        json!({"name": "x"}),
    )
    .await
    .unwrap_err();
    assert!(err.starts_with("[data_sources_unavailable]"), "{err}");
    assert_eq!(hits(&server, "/v1/data/npm/works").await, 0);
}

#[tokio::test]
async fn connectors_find_on_an_api_without_data_sources_is_unavailable() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/data/find"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let ctx = DataToolContext::for_tests(&server.uri(), false, vec![]);
    let err = call_data_tool("crw_find_data", json!({}), &ctx)
        .await
        .unwrap_err();
    assert!(err.starts_with("[data_sources_unavailable]"), "{err}");

    // A 404 that names its code keeps it.
    let server = MockServer::start().await;
    Mock::given(path("/v1/data/find"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "errorCode": "unknown_data_source", "error": "No such category."
        })))
        .mount(&server)
        .await;
    let ctx = DataToolContext::for_tests(&server.uri(), false, vec![]);
    let err = call_data_tool("crw_find_data", json!({}), &ctx)
        .await
        .unwrap_err();
    assert_eq!(err, "[unknown_data_source] No such category.");
}

#[tokio::test]
async fn connectors_route_manifest_auth_and_rate_limit_errors_keep_their_codes() {
    let cases = [
        (ResponseTemplate::new(401), "[unauthorized]", "CRW_API_KEY"),
        (ResponseTemplate::new(403), "[unauthorized]", "CRW_API_KEY"),
        (
            ResponseTemplate::new(429).insert_header("retry-after", "7"),
            "[rate_limited]",
            "in 7 seconds",
        ),
        (ResponseTemplate::new(429), "[rate_limited]", "shortly"),
        (
            ResponseTemplate::new(500),
            "[data_sources_unavailable]",
            "500",
        ),
    ];
    for (resp, code, detail) in cases {
        let server = MockServer::start().await;
        Mock::given(path("/v1/data/manifest"))
            .respond_with(resp)
            .mount(&server)
            .await;
        let err = get(
            &DataToolContext::for_tests(&server.uri(), false, vec![]),
            "npm",
            json!({}),
        )
        .await
        .unwrap_err();
        assert!(err.starts_with(code) && err.contains(detail), "{err}");
        assert_eq!(hits(&server, "/v1/data/npm/works").await, 0);
    }
}

#[tokio::test]
async fn connectors_route_unknown_schema_version_is_too_old() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/data/manifest"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"schemaVersion": 2, "connectors": []})),
        )
        .mount(&server)
        .await;
    let err = get(
        &DataToolContext::for_tests(&server.uri(), false, vec![]),
        "npm",
        json!({}),
    )
    .await
    .unwrap_err();
    assert!(err.starts_with("[crw_too_old]"), "{err}");
}

#[tokio::test]
async fn connectors_route_expired_manifest_with_failing_refresh_refuses() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/data/manifest"))
        .respond_with(manifest_body(vec![connector(&server, "openalex")]))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(path("/v1/data/manifest"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    Mock::given(path("/up/works/W1"))
        .respond_with(json_ok(json!({"id": "W1"})))
        .mount(&server)
        .await;

    let ctx = DataToolContext::for_tests(&server.uri(), false, vec![]);
    get(&ctx, "openalex", json!({"id": "W1"})).await.unwrap();
    // Within the hour the cached copy is used without a fetch.
    get(&ctx, "openalex", json!({"id": "W1"})).await.unwrap();
    assert_eq!(hits(&server, "/v1/data/manifest").await, 1);

    ctx.manifest
        .age_by(manifest::MAX_AGE + Duration::from_secs(1))
        .await;
    let err = get(&ctx, "openalex", json!({"id": "W1"}))
        .await
        .unwrap_err();
    assert!(err.starts_with("[data_sources_unavailable]"), "{err}");
    assert_eq!(
        hits(&server, "/up/works/W1").await,
        2,
        "expired copy never runs"
    );
    assert_eq!(hits(&server, "/v1/data/openalex/works").await, 0);
}

#[tokio::test]
async fn connectors_route_copy_from_before_a_long_sleep_is_refetched() {
    let server = MockServer::start().await;
    mount_manifest(&server, vec![connector(&server, "openalex")]).await;
    Mock::given(path("/up/works/W1"))
        .respond_with(json_ok(json!({"id": "W1"})))
        .mount(&server)
        .await;
    let ctx = DataToolContext::for_tests(&server.uri(), false, vec![]);
    get(&ctx, "openalex", json!({"id": "W1"})).await.unwrap();
    ctx.manifest.sleep_by(manifest::MAX_AGE).await;
    get(&ctx, "openalex", json!({"id": "W1"})).await.unwrap();
    assert_eq!(hits(&server, "/v1/data/manifest").await, 2);
}

#[tokio::test]
async fn connectors_route_rejects_bad_slugs_and_options() {
    let ctx = DataToolContext::for_tests("http://127.0.0.1:9", false, vec![]);
    for args in [
        json!({"provider": "Open", "capability": "works"}),
        json!({"provider": "a/b", "capability": "works"}),
        json!({"provider": "a", "capability": ""}),
        json!({"provider": "a", "capability": "b", "options": [1]}),
    ] {
        let err = call_data_tool("crw_get_data", args.clone(), &ctx)
            .await
            .unwrap_err();
        assert!(err.starts_with("[invalid_options]"), "{args}: {err}");
    }
}

// --- local execution ---

async fn local(
    server: &MockServer,
    conn: Value,
    entries: Vec<(&str, keys::ConnectorEntry)>,
) -> DataToolContext {
    mount_manifest(server, vec![conn]).await;
    DataToolContext::for_tests(&server.uri(), false, entries)
}

#[tokio::test]
async fn connectors_exec_same_origin_redirect_is_followed() {
    let server = MockServer::start().await;
    let ctx = local(&server, connector(&server, "openalex"), vec![]).await;
    Mock::given(path("/up/works/R"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/up/final"))
        .mount(&server)
        .await;
    Mock::given(path("/up/final"))
        .respond_with(json_ok(json!({"ok": true})))
        .mount(&server)
        .await;
    let out = get(&ctx, "openalex", json!({"id": "R"})).await.unwrap();
    assert!(
        out["provenance"]["upstreamUrl"]
            .as_str()
            .unwrap()
            .ends_with("/up/final")
    );
}

#[tokio::test]
async fn connectors_exec_refuses_cross_origin_private_and_endless_redirects() {
    let server = MockServer::start().await;
    let port = server.address().port();
    let ctx = local(&server, connector(&server, "openalex"), vec![]).await;
    for (id, location) in [
        ("other", format!("http://localhost:{port}/up/final")),
        ("private", "https://10.0.0.1/x".to_string()),
        ("literal", "https://93.184.216.34/x".to_string()),
        ("loop", "/up/works/loop".to_string()),
    ] {
        Mock::given(path(format!("/up/works/{id}")))
            .respond_with(ResponseTemplate::new(302).insert_header("location", location.as_str()))
            .mount(&server)
            .await;
        let err = get(&ctx, "openalex", json!({"id": id})).await.unwrap_err();
        assert!(err.starts_with("[blocked]"), "{id}: {err}");
    }
    assert_eq!(hits(&server, "/up/final").await, 0);
    assert_eq!(
        hits(&server, "/up/works/loop").await,
        4,
        "first request plus 3 hops"
    );
}

#[tokio::test]
async fn connectors_exec_unusable_responses() {
    let server = MockServer::start().await;
    let ctx = local(&server, connector(&server, "openalex"), vec![]).await;
    let cases = [
        (
            "html",
            ResponseTemplate::new(200).set_body_raw("<html>x</html>", "text/html"),
        ),
        (
            "notjson",
            ResponseTemplate::new(200).set_body_raw("<html>x</html>", "application/json"),
        ),
        (
            "null",
            ResponseTemplate::new(200).set_body_raw("null", "application/json"),
        ),
        (
            "big",
            ResponseTemplate::new(200).set_body_raw(
                format!("\"{}\"", "x".repeat(client::MAX_BODY_BYTES)),
                "application/json",
            ),
        ),
    ];
    for (id, resp) in cases {
        Mock::given(path(format!("/up/works/{id}")))
            .respond_with(resp)
            .mount(&server)
            .await;
        let err = get(&ctx, "openalex", json!({"id": id})).await.unwrap_err();
        assert!(err.starts_with("[invalid_response]"), "{id}: {err}");
    }
}

#[tokio::test]
async fn connectors_exec_upstream_error_body_is_not_passed_through() {
    let server = MockServer::start().await;
    let ctx = local(&server, connector(&server, "openalex"), vec![]).await;
    Mock::given(path("/up/works/E"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_raw("{\"detail\":\"upstream internals\"}", "application/json"),
        )
        .mount(&server)
        .await;
    let err = get(&ctx, "openalex", json!({"id": "E"})).await.unwrap_err();
    assert!(
        err.starts_with("[upstream_error]") && err.contains("404"),
        "{err}"
    );
    assert!(!err.contains("upstream internals"));
}

#[tokio::test]
async fn connectors_exec_missing_option_is_invalid_options() {
    let server = MockServer::start().await;
    let ctx = local(&server, connector(&server, "openalex"), vec![]).await;
    let err = get(&ctx, "openalex", json!({})).await.unwrap_err();
    assert_eq!(err, "[invalid_options] Missing required parameter \"id\"");
}

/// The upstream echoes the key back in all five forms; none may survive in the
/// result, and the provenance URL carries `REDACTED` instead.
#[tokio::test]
async fn connectors_exec_redacts_query_key_everywhere() {
    let server = MockServer::start().await;
    let mut conn = connector(&server, "openalex");
    conn["auth"] = json!({"type": "query", "param": "api_key"});
    conn["keyRequired"] = json!(true);
    let ctx = local(
        &server,
        conn,
        vec![("openalex", approved(&server, "query:api_key"))],
    )
    .await;
    let forms = [
        USER_KEY.to_string(),
        url::encode_uri_component(USER_KEY),
        ::url::form_urlencoded::byte_serialize(USER_KEY.as_bytes()).collect::<String>(),
        serde_json::to_string(USER_KEY)
            .unwrap()
            .trim_matches('"')
            .to_string(),
        base64::engine::general_purpose::STANDARD.encode(format!("{USER_KEY}:")),
    ];
    // `slash` spells the key with `\/` escapes, `lower` with lowercase hex.
    let echoed = format!(
        "{{\"raw\":\"{}\",\"uri\":\"{}\",\"form\":\"{}\",\"slash\":\"{}\",\"lower\":\"{}\",\"basic\":\"{}\"}}",
        forms[3],
        forms[1],
        forms[2],
        forms[3].replace('/', "\\/"),
        forms[1].to_ascii_lowercase(),
        forms[4]
    );
    Mock::given(path("/up/works/W1"))
        .and(query_param("api_key", USER_KEY))
        .respond_with(ResponseTemplate::new(200).set_body_raw(echoed, "application/json"))
        .expect(1)
        .mount(&server)
        .await;

    let out = get(&ctx, "openalex", json!({"id": "W1"})).await.unwrap();
    let text = out.to_string();
    for form in &forms {
        assert!(
            !text.contains(form.as_str()),
            "form {form:?} leaked: {text}"
        );
    }
    assert!(
        out["provenance"]["upstreamUrl"]
            .as_str()
            .unwrap()
            .contains("api_key=REDACTED")
    );
    assert_eq!(out["data"]["uri"], "REDACTED");
    assert_eq!(out["data"]["slash"], "REDACTED");
    assert_eq!(out["data"]["lower"], "REDACTED");
    assert_eq!(out["data"]["basic"], "REDACTED");
}

#[tokio::test]
async fn connectors_exec_header_and_basic_auth() {
    let server = MockServer::start().await;
    let mut hdr = connector(&server, "hdr");
    hdr["auth"] = json!({"type": "header", "param": "X-Api-Key"});
    let mut basic = connector(&server, "basic");
    basic["auth"] = json!({"type": "basic", "param": null});
    basic["capabilities"][0]["pathTemplate"] = json!("/up/basic/{id}");
    mount_manifest(&server, vec![hdr, basic]).await;
    let key = "plain-key-1";
    let entry = |auth: &str| keys::ConnectorEntry {
        api_key: Some(key.into()),
        origins: vec![server.uri()],
        auth: Some(auth.into()),
    };
    let ctx = DataToolContext::for_tests(
        &server.uri(),
        false,
        vec![
            ("hdr", entry("header:X-Api-Key")),
            ("basic", entry("basic")),
        ],
    );
    Mock::given(path("/up/works/H"))
        .and(header("x-api-key", key))
        .respond_with(json_ok(json!({"echo": key})))
        .expect(1)
        .mount(&server)
        .await;
    let basic_value = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{key}:"))
    );
    Mock::given(path("/up/basic/B"))
        .and(header("authorization", basic_value.as_str()))
        .respond_with(json_ok(json!({"ok": 1})))
        .expect(1)
        .mount(&server)
        .await;
    let out = get(&ctx, "hdr", json!({"id": "H"})).await.unwrap();
    assert_eq!(out["data"]["echo"], "REDACTED");
    get(&ctx, "basic", json!({"id": "B"})).await.unwrap();
}

#[tokio::test]
async fn connectors_exec_header_set_sends_every_pair() {
    // The FCA Register 403s unless X-Auth-Email rides beside X-Auth-Key.
    let server = MockServer::start().await;
    let mut conn = connector(&server, "fca");
    conn["auth"] = json!({"type": "header", "param": "X-Auth-Key"});
    let set = r#"{"X-Auth-Email":"me@example.com","X-Auth-Key":"set-key-1"}"#;
    let ctx = local(
        &server,
        conn,
        vec![(
            "fca",
            keys::ConnectorEntry {
                api_key: Some(set.into()),
                origins: vec![server.uri()],
                auth: Some("header:X-Auth-Key".into()),
            },
        )],
    )
    .await;
    Mock::given(path("/up/works/F"))
        .and(header("x-auth-key", "set-key-1"))
        .and(header("x-auth-email", "me@example.com"))
        .respond_with(json_ok(
            json!({"email": "me@example.com", "key": "set-key-1"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let out = get(&ctx, "fca", json!({"id": "F"})).await.unwrap();
    assert_eq!(out["data"]["email"], "REDACTED");
    assert_eq!(out["data"]["key"], "REDACTED");
}

#[tokio::test]
async fn connectors_exec_refused_stored_key_says_so() {
    let server = MockServer::start().await;
    let mut keyed = connector(&server, "hdr");
    keyed["auth"] = json!({"type": "header", "param": "X-Api-Key"});
    let mut open = connector(&server, "open");
    open["capabilities"][0]["pathTemplate"] = json!("/up/open/{id}");
    mount_manifest(&server, vec![keyed, open]).await;
    let ctx = DataToolContext::for_tests(
        &server.uri(),
        false,
        vec![("hdr", approved(&server, "header:X-Api-Key"))],
    );
    Mock::given(path("/up/works/K"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(path("/up/open/K"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let err = get(&ctx, "hdr", json!({"id": "K"})).await.unwrap_err();
    assert_eq!(
        err,
        "[upstream_error] OpenAlex refused the stored key (HTTP 401). Check it or store it again. Ask the user to run this in their own terminal: crw connectors set-key hdr"
    );
    // No key was sent, so a 403 is not about one.
    let err = call_data_tool(
        "crw_get_data",
        json!({"provider": "open", "capability": "works", "options": {"id": "K"}}),
        &ctx,
    )
    .await
    .unwrap_err();
    assert_eq!(
        err,
        "[upstream_error] OpenAlex returned HTTP 403 for this request."
    );
}

#[tokio::test]
async fn connectors_exec_header_set_values_are_trimmed_for_send_and_redaction() {
    let server = MockServer::start().await;
    let mut conn = connector(&server, "fca");
    conn["auth"] = json!({"type": "header", "param": "X-Auth-Key"});
    let ctx = local(
        &server,
        conn,
        vec![(
            "fca",
            keys::ConnectorEntry {
                api_key: Some(r#"{" X-Auth-Key ":"  padded-key-9 "}"#.into()),
                origins: vec![server.uri()],
                auth: Some("header:X-Auth-Key".into()),
            },
        )],
    )
    .await;
    Mock::given(path("/up/works/P"))
        .and(header("x-auth-key", "padded-key-9"))
        .respond_with(json_ok(json!({"echo": "padded-key-9"})))
        .expect(1)
        .mount(&server)
        .await;
    let out = get(&ctx, "fca", json!({"id": "P"})).await.unwrap();
    assert_eq!(out["data"]["echo"], "REDACTED");
}

#[test]
fn connectors_header_set_falls_through_to_a_single_header() {
    // Same vectors as the cloud's parseHeaderSet tests.
    for key in [
        r#"{"X":{"nested":1}}"#,
        r#"{"X":""}"#,
        r#"{"X":"   "}"#,
        r#"{"X-Auth-Key":"k","X-Auth-Email":""}"#,
        r#"{"":"v"}"#,
        "{oops",
        "{}",
        "plain-key",
    ] {
        assert!(execute::header_set(key).is_none(), "{key}");
    }
    assert_eq!(
        execute::header_set(r#" {"A":"1","B":"2"} "#).unwrap(),
        vec![("A".into(), "1".into()), ("B".into(), "2".into())]
    );
}

#[tokio::test]
async fn connectors_exec_header_key_refusals_send_nothing() {
    let server = MockServer::start().await;
    let mut set = connector(&server, "fca");
    set["auth"] = json!({"type": "header", "param": "X-Auth-Key"});
    let mut bad_param = connector(&server, "badparam");
    bad_param["auth"] = json!({"type": "header", "param": "X Api Key"});
    mount_manifest(&server, vec![set, bad_param]).await;
    let entry = |key: &str, auth: &str| keys::ConnectorEntry {
        api_key: Some(key.into()),
        origins: vec![server.uri()],
        auth: Some(auth.into()),
    };
    let ctx = DataToolContext::for_tests(
        &server.uri(),
        false,
        vec![
            ("fca", entry(r#"{"X Bad":"v"}"#, "header:X-Auth-Key")),
            ("badparam", entry("k1", "header:X Api Key")),
        ],
    );
    let err = get(&ctx, "fca", json!({"id": "F"})).await.unwrap_err();
    assert!(
        err.starts_with("[connector_key_invalid]")
            && err.contains("crw connectors set-key fca")
            && !err.contains("X Bad"),
        "{err}"
    );
    let err = get(&ctx, "badparam", json!({"id": "F"})).await.unwrap_err();
    assert!(err.starts_with("[connector_misconfigured]"), "{err}");
    assert_eq!(hits(&server, "/up/works/F").await, 0);
}

#[tokio::test]
async fn connectors_exec_oversized_result_is_a_tool_error() {
    let server = MockServer::start().await;
    let ctx = local(&server, connector(&server, "openalex"), vec![]).await;
    Mock::given(path("/up/works/BIG"))
        .respond_with(json_ok(
            json!({"blob": "x".repeat(crate::mcp::MAX_DATA_PAYLOAD_BYTES)}),
        ))
        .mount(&server)
        .await;
    let err = get(&ctx, "openalex", json!({"id": "BIG"}))
        .await
        .unwrap_err();
    assert!(err.starts_with("[result_too_large]"), "{err}");
}

#[tokio::test]
async fn connectors_exec_unapproved_origin_and_missing_key_send_nothing() {
    let server = MockServer::start().await;
    let mut conn = connector(&server, "openalex");
    conn["auth"] = json!({"type": "query", "param": "api_key"});
    conn["keyRequired"] = json!(true);
    mount_manifest(&server, vec![conn]).await;

    let elsewhere = keys::ConnectorEntry {
        api_key: Some(USER_KEY.into()),
        origins: vec!["https://api.openalex.org".into()],
        auth: Some("query:api_key".into()),
    };
    let ctx = DataToolContext::for_tests(&server.uri(), true, vec![("openalex", elsewhere)]);
    let err = get(&ctx, "openalex", json!({"id": "W1"}))
        .await
        .unwrap_err();
    assert!(err.starts_with("[connector_origin_not_approved]"), "{err}");
    assert!(!err.contains(USER_KEY));

    let ctx = DataToolContext::for_tests(&server.uri(), true, vec![]);
    let err = get(&ctx, "openalex", json!({"id": "W1"}))
        .await
        .unwrap_err();
    assert!(
        err.starts_with("[connector_key_missing]") && err.contains("openalex.org/settings/api"),
        "{err}"
    );

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(hits(&server, "/up/works/W1").await, 0);
    assert_eq!(
        hits(&server, "/v1/data/usage").await,
        0,
        "a refused call is not a run"
    );
}

#[tokio::test]
async fn connectors_exec_paces_per_provider() {
    let server = MockServer::start().await;
    let mut conn = connector(&server, "openalex");
    conn["minIntervalMs"] = json!(300);
    let ctx = local(&server, conn, vec![]).await;
    Mock::given(path("/up/works/P"))
        .respond_with(json_ok(json!({})))
        .mount(&server)
        .await;
    ctx.manifest().await.unwrap();
    let started = std::time::Instant::now();
    let (a, b) = tokio::join!(
        get(&ctx, "openalex", json!({"id": "P"})),
        get(&ctx, "openalex", json!({"id": "P"}))
    );
    a.unwrap();
    b.unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn connectors_exec_honours_short_retry_after_once() {
    let server = MockServer::start().await;
    let ctx = local(&server, connector(&server, "openalex"), vec![]).await;
    Mock::given(path("/up/works/S"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(path("/up/works/S"))
        .respond_with(json_ok(json!({"ok": true})))
        .mount(&server)
        .await;
    Mock::given(path("/up/works/L"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
        .mount(&server)
        .await;

    get(&ctx, "openalex", json!({"id": "S"})).await.unwrap();
    assert_eq!(hits(&server, "/up/works/S").await, 2);
    let err = get(&ctx, "openalex", json!({"id": "L"})).await.unwrap_err();
    assert!(
        err.starts_with("[upstream_rate_limited]") && err.contains("OpenAlex"),
        "{err}"
    );
    assert_eq!(
        hits(&server, "/up/works/L").await,
        1,
        "a long Retry-After is not waited out"
    );
}

#[tokio::test]
async fn connectors_exec_timeout_error_carries_no_key() {
    let server = MockServer::start().await;
    let mut conn = connector(&server, "openalex");
    conn["auth"] = json!({"type": "query", "param": "api_key"});
    let ctx = local(
        &server,
        conn,
        vec![("openalex", approved(&server, "query:api_key"))],
    )
    .await;
    Mock::given(path("/up/works/T"))
        .respond_with(json_ok(json!({})).set_delay(Duration::from_secs(60)))
        .mount(&server)
        .await;
    ctx.manifest().await.unwrap();
    // Virtual time: the 20 s budget elapses without the test waiting for it.
    tokio::time::pause();
    let err = get(&ctx, "openalex", json!({"id": "T"})).await.unwrap_err();
    assert!(err.starts_with("[upstream_timeout]"), "{err}");
    assert!(!err.contains(USER_KEY) && !err.contains("api_key"), "{err}");
}

// --- telemetry ---

async fn usage_bodies(server: &MockServer) -> Vec<Value> {
    for _ in 0..40 {
        let bodies: Vec<Value> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/v1/data/usage")
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect();
        if !bodies.is_empty() {
            return bodies;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Vec::new()
}

#[tokio::test]
async fn connectors_telemetry_one_ping_per_client_call_without_url_or_key() {
    let server = MockServer::start().await;
    let mut conn = connector(&server, "openalex");
    conn["auth"] = json!({"type": "query", "param": "api_key"});
    mount_manifest(&server, vec![conn]).await;
    Mock::given(path("/up/works/W1"))
        .respond_with(json_ok(json!({})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/data/usage"))
        .and(header(
            "authorization",
            format!("Bearer {CRW_KEY}").as_str(),
        ))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let ctx = DataToolContext::for_tests(
        &server.uri(),
        true,
        vec![("openalex", approved(&server, "query:api_key"))],
    );
    get(&ctx, "openalex", json!({"id": "W1", "page": "7"}))
        .await
        .unwrap();

    let bodies = usage_bodies(&server).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(bodies.len(), 1);
    assert_eq!(hits(&server, "/v1/data/usage").await, 1, "exactly one ping");
    let body = &bodies[0];
    let mut keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["capability", "latencyMs", "outcome", "provider"]);
    assert_eq!(body["provider"], "openalex");
    assert_eq!(body["capability"], "works");
    assert_eq!(body["outcome"], "success");
    assert!(body["latencyMs"].is_u64());
    let raw = body.to_string();
    for leak in [USER_KEY, "/up/works", "W1", "api_key", "127.0.0.1"] {
        assert!(!raw.contains(leak), "{leak} in {raw}");
    }
}

#[tokio::test]
async fn connectors_telemetry_reports_failures_and_respects_opt_out() {
    let server = MockServer::start().await;
    mount_manifest(&server, vec![connector(&server, "openalex")]).await;
    Mock::given(path("/up/works/E"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let on = DataToolContext::for_tests(&server.uri(), true, vec![]);
    get(&on, "openalex", json!({"id": "E"})).await.unwrap_err();
    let bodies = usage_bodies(&server).await;
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["outcome"], "upstream_error");

    let off = DataToolContext::for_tests(&server.uri(), false, vec![]);
    get(&off, "openalex", json!({"id": "E"})).await.unwrap_err();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        hits(&server, "/v1/data/usage").await,
        1,
        "opt-out sends nothing"
    );
}

#[tokio::test]
async fn connectors_telemetry_counts_a_capped_result_as_too_large() {
    let server = MockServer::start().await;
    mount_manifest(&server, vec![connector(&server, "openalex")]).await;
    Mock::given(path("/up/works/BIG"))
        .respond_with(json_ok(
            json!({"blob": "x".repeat(crate::mcp::MAX_DATA_PAYLOAD_BYTES)}),
        ))
        .mount(&server)
        .await;
    let ctx = DataToolContext::for_tests(&server.uri(), true, vec![]);
    let err = get(&ctx, "openalex", json!({"id": "BIG"}))
        .await
        .unwrap_err();
    assert!(err.starts_with("[result_too_large]"), "{err}");
    let bodies = usage_bodies(&server).await;
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["outcome"], "result_too_large");
}

#[tokio::test]
async fn connectors_forwarded_429_carries_retry_after() {
    let server = MockServer::start().await;
    mount_manifest(&server, vec![]).await;
    let limited = |wait: Option<&str>| {
        let r = ResponseTemplate::new(429).set_body_json(json!({
            "success": false, "errorCode": "rate_limited", "error": "Too many requests."
        }));
        match wait {
            Some(w) => r.insert_header("retry-after", w),
            None => r,
        }
    };
    Mock::given(path("/v1/data/npm/works"))
        .respond_with(limited(Some("9")))
        .mount(&server)
        .await;
    Mock::given(path("/v1/data/pypi/works"))
        .respond_with(limited(None))
        .mount(&server)
        .await;
    Mock::given(path("/v1/data/find"))
        .respond_with(limited(Some("4")))
        .mount(&server)
        .await;
    let ctx = DataToolContext::for_tests(&server.uri(), false, vec![]);
    assert_eq!(
        get(&ctx, "npm", json!({})).await.unwrap_err(),
        "[rate_limited] Too many requests. Try again in 9 seconds."
    );
    assert_eq!(
        get(&ctx, "pypi", json!({})).await.unwrap_err(),
        "[rate_limited] Too many requests."
    );
    assert_eq!(
        call_data_tool("crw_find_data", json!({}), &ctx)
            .await
            .unwrap_err(),
        "[rate_limited] Too many requests. Try again in 4 seconds."
    );
}

// --- find ---

#[tokio::test]
async fn connectors_find_maps_items_and_hides_execution() {
    let server = MockServer::start().await;
    let mut conn = connector(&server, "openalex");
    conn["auth"] = json!({"type": "query", "param": "api_key"});
    conn["keyRequired"] = json!(true);
    mount_manifest(&server, vec![conn]).await;
    Mock::given(method("GET"))
        .and(path("/v1/data/find"))
        .and(query_param("query", "works"))
        .and(query_param("category", "Science"))
        .and(query_param("provider", "openalex"))
        .and(query_param("limit", "2"))
        .and(query_param("offset", "4"))
        .and(header("authorization", format!("Bearer {CRW_KEY}").as_str()))
        .and(header("user-agent", API_UA))
        .respond_with(json_ok(json!({
            "level": "tools", "total": 9, "mode": "browse",
            "next": {"provider": "fastcrw", "capability": "find-tools", "options": {"offset": 6}},
            "nextOffset": 6,
            "items": [
                {"id": "openalex/works", "provider": "openalex", "capability": "works",
                 "name": "OpenAlex: works", "description": "Works", "creditsCost": 0,
                 "execution": "client", "keyRequired": true, "options": {"required": ["id"], "optional": ["page"]},
                 "provenance": {"source": "OpenAlex", "licence": "CC0 1.0"}},
                {"id": "npm/package", "provider": "npm", "capability": "package",
                 "name": "npm: package", "description": null, "creditsCost": 1,
                 "execution": "server", "options": {"required": ["name"], "optional": []},
                 "provenance": {"source": "npm", "licence": "Varies"}}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ctx = DataToolContext::for_tests(
        &server.uri(),
        false,
        vec![("openalex", approved(&server, "query:api_key"))],
    );
    let out = call_data_tool(
        "crw_find_data",
        json!({"query": " works ", "category": "Science ", "provider": " OpenAlex", "limit": 2, "offset": 4}),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(out["total"], 9);
    assert_eq!(out["nextOffset"], 6, "the server's nextOffset");
    assert!(!out.to_string().contains("execution"));
    assert_eq!(
        out["items"][0],
        json!({"provider": "openalex", "capability": "works", "name": "OpenAlex: works",
               "description": "Works", "creditsCost": 0, "requiredOptions": ["id"],
               "optionalOptions": ["page"], "keyRequired": true, "licence": "CC0 1.0",
               "keyConfigured": true})
    );
    assert_eq!(out["items"][1]["keyRequired"], false);
    assert!(out["items"][1].get("keyConfigured").is_none());
}

#[tokio::test]
async fn connectors_find_error_is_a_bracketed_line() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/data/find"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "success": false, "error": "Invalid API key"
        })))
        .mount(&server)
        .await;
    let ctx = DataToolContext::for_tests(&server.uri(), false, vec![]);
    let err = call_data_tool("crw_find_data", json!({}), &ctx)
        .await
        .unwrap_err();
    assert_eq!(err, "[unauthorized] Invalid API key");
    let req = &server.received_requests().await.unwrap()[0];
    assert_eq!(req.url.query(), Some("limit=10&offset=0"));
}
