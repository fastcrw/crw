use crw_core::config::*;

#[test]
fn server_config_default_values() {
    let config = ServerConfig::default();
    assert_eq!(config.host, "0.0.0.0");
    assert_eq!(config.port, 3000);
    assert_eq!(config.request_timeout_secs, 60);
}

#[test]
fn renderer_config_default_values() {
    let config = RendererConfig::default();
    assert_eq!(config.mode, RendererMode::Auto);
    assert_eq!(config.page_timeout_ms, 30000);
    assert_eq!(config.pool_size, 4);
    assert!(config.lightpanda.is_none());
    assert!(config.playwright.is_none());
    assert!(config.chrome.is_none());
    // The impersonated section is default-ON: an absent section means "on
    // with defaults" in any build compiled with the feature.
    assert!(config.impersonated.enabled);
    assert_eq!(config.impersonated.timeout_ms, None);
}

#[test]
fn renderer_config_impersonated_disable() {
    let config: RendererConfig = toml::from_str("[impersonated]\nenabled = false\n").unwrap();
    assert!(!config.impersonated.enabled);
    // This test binary has no `impersonated` feature, so the cfg!-folded
    // predicate is false either way; the runtime kill switch is the feature-on
    // half, exercised in crw-renderer's feature-on test run.
    assert!(!config.impersonated_in_chain());
}

#[test]
fn renderer_config_impersonated_deadline_math() {
    // Timeout falls back to the HTTP tier timeout, then page_timeout_ms.
    let config = RendererConfig::default();
    assert_eq!(config.impersonated_timeout(), config.http_timeout());
    assert_eq!(config.impersonated_timeout(), 30_000);
    let config: RendererConfig =
        toml::from_str("\nhttp_timeout_ms = 12000\n[impersonated]\ntimeout_ms = 7000\n").unwrap();
    assert_eq!(config.impersonated_timeout(), 7_000);
    // In a feature-less build the tier never contributes to the ladder
    // deadline sum (cfg!-folded like camoufox/cloak): HTTP contribution only,
    // the 7s impersonated timeout is NOT added.
    assert_eq!(config.min_deadline_for_full_ladder_ms(), 12_000);
}

#[test]
fn crawler_config_default_values() {
    let config = CrawlerConfig::default();
    assert_eq!(config.max_concurrency, 10);
    assert!((config.requests_per_second - 10.0).abs() < f64::EPSILON);
    assert!(config.respect_robots_txt);
    assert!(config.user_agent.contains("Chrome/"));
    assert_eq!(config.default_max_depth, 2);
    assert_eq!(config.default_max_pages, 100);
    assert!(config.proxy.is_none());
    assert_eq!(config.job_ttl_secs, 3600);
}

#[test]
fn extraction_config_default_values() {
    let config = ExtractionConfig::default();
    assert!(config.only_main_content);
    assert!(config.llm.is_none());
}

#[test]
fn auth_config_default_empty() {
    let config = AuthConfig::default();
    assert!(config.api_keys.is_empty());
}

#[test]
fn server_config_deserialize_partial() {
    let toml_str = r#"
        port = 8080
    "#;
    let config: ServerConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.port, 8080);
    // host should fallback to default
    assert_eq!(config.host, "0.0.0.0");
    assert_eq!(config.request_timeout_secs, 60);
}

#[test]
fn crawler_config_deserialize_partial() {
    let toml_str = r#"
        max_concurrency = 20
        requests_per_second = 5.0
    "#;
    let config: CrawlerConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.max_concurrency, 20);
    assert!((config.requests_per_second - 5.0).abs() < f64::EPSILON);
    // defaults for the rest
    assert!(config.respect_robots_txt);
    assert!(config.user_agent.contains("Chrome/"));
}

#[test]
fn app_config_deserialize_empty() {
    let toml_str = "";
    let config: AppConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.server.port, 3000);
    assert_eq!(config.renderer.mode, RendererMode::Auto);
    assert_eq!(config.crawler.max_concurrency, 10);
}

#[test]
fn auth_config_api_keys_toml_array() {
    let toml_str = r#"
        api_keys = ["key1", "key2"]
    "#;
    let config: AuthConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.api_keys, vec!["key1", "key2"]);
}

#[test]
fn auth_config_api_keys_empty() {
    let toml_str = "";
    let config: AuthConfig = toml::from_str(toml_str).unwrap();
    assert!(config.api_keys.is_empty());
}

#[test]
fn auth_config_api_keys_json_string() {
    // JSON array as string (what env vars pass)
    let toml_str = r#"api_keys = "[\"key1\", \"key2\"]""#;
    let config: AuthConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.api_keys, vec!["key1", "key2"]);
}

#[test]
fn auth_config_api_keys_comma_separated() {
    // Comma-separated string
    let toml_str = r#"api_keys = "key1,key2""#;
    let config: AuthConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.api_keys, vec!["key1", "key2"]);
}

#[test]
fn auth_config_api_keys_comma_with_spaces() {
    // Comma-separated with spaces
    let toml_str = r#"api_keys = "key1, key2 , key3""#;
    let config: AuthConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.api_keys, vec!["key1", "key2", "key3"]);
}
