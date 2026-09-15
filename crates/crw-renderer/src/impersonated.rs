//! Chrome-impersonation HTTP tier: a wreq-based fetcher that presents a real
//! Chrome TLS/JA3/HTTP2 fingerprint with no browser and no JS execution.
//!
//! Two roles, both wired in `lib.rs`:
//!
//! * the auto-chain hop between the plain reqwest HTTP tier and the browser
//!   ladder, fired only when the plain tier hit a wall-shaped response (the
//!   Amazon interstitial class) or a transport error, never on SPA/thin/empty
//!   shapes that need JS;
//! * the explicit `renderer = "impersonated-http"` pin.
//!
//! Whole module is `#[cfg(feature = "impersonated")]`: a lean build never
//! links the wreq/btls TLS stack.

use async_trait::async_trait;
use crw_core::Deadline;
use crw_core::error::{CrwError, CrwResult};
use crw_core::types::FetchResult;
use std::collections::HashMap;
use std::time::Instant;

use crate::http_only;
use crate::traits::PageFetcher;

/// The wreq preset this build impersonates. Pinned, not
/// `wreq_util::Profile::default()` (that is Chrome100, too old for modern
/// walls). Bump deliberately when wreq ships newer presets AND a live wall
/// verifies the newer one still passes (see the `#[ignore]` live tests at the
/// bottom of this file). Spelled via the `Emulation::Chrome149` associated
/// const (wreq-util 0.2 exports the preset list as consts on `Emulation`
/// whose type is `Profile`; `Profile` itself implements wreq's
/// `IntoEmulation`). Not a config knob on purpose: only the Chrome family is
/// verified against live walls, so a `profile` key would ship an untested
/// promise. Adding a second verified preset is a two-line change here plus a
/// config field.
const CHROME_PRESET: wreq_util::Profile = wreq_util::Emulation::Chrome149;

/// Chrome-impersonating HTTP fetcher. Never executes JS.
pub struct ImpersonatedFetcher {
    client: wreq::Client,
}

impl ImpersonatedFetcher {
    /// Build with the pinned Chrome preset. **Hard error on build failure**,
    /// mirroring `http_only::build_client`'s contract: never fall back to a
    /// non-impersonating client, which would silently hand the wall a plain
    /// reqwest fingerprint.
    pub fn new(timeout: std::time::Duration) -> CrwResult<Self> {
        Self::build(None, timeout)
    }

    /// Fail-closed proxy twin, mirroring `HttpFetcher::with_proxy`: a bad
    /// proxy URL is a hard error, never a silent direct connection that would
    /// leak the host's real IP to the wall. Used by the warm per-proxy cache
    /// in `FallbackRenderer::impersonated_fetcher_for_request` so a set
    /// `REQUEST_PROXY` is honored by this tier like every other one.
    pub fn with_proxy(proxy_url: &str, timeout: std::time::Duration) -> CrwResult<Self> {
        Self::build(Some(proxy_url), timeout)
    }

    fn build(proxy: Option<&str>, timeout: std::time::Duration) -> CrwResult<Self> {
        let mut builder = wreq::Client::builder()
            // Owns UA + sec-ch-* + accept-encoding so the header set stays
            // coherent with the emulated JA3. The configured CRW user_agent is
            // deliberately NOT injected here.
            .emulation(CHROME_PRESET)
            .connect_timeout(http_only::HTTP_CONNECT_TIMEOUT)
            .timeout(timeout)
            .redirect(safe_redirect_policy_wreq());
        if let Some(proxy_url) = proxy {
            let p = wreq::Proxy::all(proxy_url).map_err(|_| {
                // NEVER interpolate `proxy_url` itself: it carries `user:pass@`.
                let redacted = crw_core::redact_proxy_url(proxy_url);
                CrwError::ConfigError(format!(
                    "invalid proxy URL '{redacted}' for the impersonated tier"
                ))
            })?;
            builder = builder.proxy(p);
        }
        let client = builder
            .build()
            .map_err(|e| CrwError::ConfigError(format!("impersonated client: {e}")))?;
        Ok(Self { client })
    }
}

async fn read_body_capped(resp: wreq::Response, max: usize, url: &str) -> CrwResult<Vec<u8>> {
    let (body, truncated) = crw_core::body::read_capped(resp.bytes_stream(), max)
        .await
        .map_err(|e| CrwError::HttpError(format!("{url}: {e}")))?;
    if truncated {
        return Err(CrwError::HttpError(format!(
            "Response too large: exceeds {max} bytes"
        )));
    }
    Ok(body)
}

/// wreq twin of `crw_core::url_safety::safe_redirect_policy`. The SSRF
/// invariant has ONE owner: `validate_safe_url_blocking_resolved` in crw-core,
/// which both policies call. Only the transport adapter lives here; putting it
/// in crw-core would make core link wreq.
///
/// The hop cap mirrors the reqwest twin's `previous().len() >= 10` check
/// exactly (the custom policy CRW ships, not reqwest's default `limited`).
fn safe_redirect_policy_wreq() -> wreq::redirect::Policy {
    wreq::redirect::Policy::custom(|attempt| {
        if attempt.previous.len() >= 10 {
            attempt.error("too many redirects")
        } else {
            // `http::Uri` has no `as_str()`; Display renders scheme +
            // authority + path, which is what `url::Url::parse` needs.
            match url::Url::parse(&attempt.uri.to_string()) {
                Ok(u) => match crw_core::url_safety::validate_safe_url_blocking_resolved(&u) {
                    Ok(()) => attempt.follow(),
                    Err(e) => attempt.error(format!("redirect blocked: {e}")),
                },
                Err(e) => attempt.error(format!("invalid redirect target: {e}")),
            }
        }
    })
}

#[async_trait]
impl PageFetcher for ImpersonatedFetcher {
    async fn fetch(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        _wait_for_ms: Option<u64>,
        deadline: Deadline,
    ) -> CrwResult<FetchResult> {
        if deadline.expired() {
            return Err(CrwError::HttpError(format!(
                "deadline expired before impersonated-HTTP fetch of {url}"
            )));
        }
        let start = Instant::now();

        // Caller-supplied headers override the preset's exactly like the
        // http tier's precedence (per-request beats client default): auth,
        // cookies and the like still apply. Fingerprint-relevant headers are
        // DROPPED, not overridden: the emulation preset owns UA + sec-ch-* +
        // accept-encoding, and a caller UA over a Chrome149 JA3 is precisely
        // the incoherence TLS-fingerprint walls test for (the per-request
        // stealth path injects a rotated pool UA; honoring it here would get
        // the pinned tier blocked where the untouched preset passes).
        let mut req = self.client.get(url);
        for (k, v) in headers {
            let lower = k.to_ascii_lowercase();
            let fingerprint_owned =
                lower == "user-agent" || lower == "accept-encoding" || lower.starts_with("sec-ch-");
            if !fingerprint_owned {
                req = req.header(k.as_str(), v.as_str());
            }
        }

        let remaining = deadline.remaining();
        if remaining.is_zero() {
            return Err(CrwError::Timeout(deadline.requested_ms()));
        }
        let resp = match tokio::time::timeout(remaining, req.send()).await {
            Ok(r) => r.map_err(|e| CrwError::HttpError(format!("{url}: {e}")))?,
            Err(_) => return Err(CrwError::Timeout(deadline.requested_ms())),
        };
        let status = resp.status().as_u16();

        if let Some(len) = resp.content_length()
            && len as usize > http_only::MAX_RESPONSE_BYTES
        {
            return Err(CrwError::HttpError(format!(
                "Response too large: {len} bytes (max {})",
                http_only::MAX_RESPONSE_BYTES
            )));
        }
        let content_type_header = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        // Same challenge-header read the plain HTTP tier stamps into
        // `warning`: reqwest's `header::HeaderMap` is a re-export of
        // `http::HeaderMap`, which is what wreq returns, so the helper works
        // unchanged. (`http_only::challenge_header` is the single owner.)
        let challenge = http_only::challenge_header(resp.headers());

        let final_url_str = resp.uri().to_string();

        let bytes = match tokio::time::timeout(
            deadline.remaining().max(crate::MIN_TIER_BUDGET),
            read_body_capped(resp, http_only::MAX_RESPONSE_BYTES, url),
        )
        .await
        {
            Ok(r) => r?,
            Err(_) => {
                return Err(CrwError::Timeout(start.elapsed().as_millis().max(1) as u64));
            }
        };

        // Shared response tail with the plain tier (size cap, PDF sniff,
        // binary rejection, charset-aware decode, FetchResult assembly): the
        // Chrome preset advertises an Accept-Encoding the client decompresses
        // itself, so the bytes handed over are already plain.
        http_only::build_http_fetch_result(
            url,
            status,
            content_type_header.as_deref(),
            challenge,
            &final_url_str,
            &bytes,
            start.elapsed().as_millis() as u64,
            "impersonated-http",
        )
    }

    fn name(&self) -> &str {
        "impersonated-http"
    }

    fn supports_js(&self) -> bool {
        false
    }

    async fn is_available(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crw_core::Deadline;

    // Same lock http_only's tests use: env mutation is process-wide, so the
    // two modules must serialise against EACH OTHER, not just within themselves.
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    fn fetcher() -> CrwResult<ImpersonatedFetcher> {
        ImpersonatedFetcher::new(std::time::Duration::from_secs(10))
    }

    #[test]
    fn client_builds_with_chrome_preset() {
        assert!(fetcher().is_ok());
    }

    #[test]
    fn client_build_is_fail_closed_with_bad_proxy() {
        let f =
            ImpersonatedFetcher::with_proxy("not a valid uri", std::time::Duration::from_secs(10));
        assert!(f.is_err(), "a malformed proxy must be a hard error");
    }

    #[test]
    fn name_and_js_support() {
        let f = fetcher().unwrap();
        assert_eq!(f.name(), "impersonated-http");
        assert!(!f.supports_js());
    }

    /// wiremock round trip: a clean 200 is accepted with the right
    /// `rendered_with`, `final_url`, and no challenge warning.
    #[tokio::test]
    async fn wiremock_clean_200_is_accepted() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/page"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .set_body_string(
                        "<html><body><h1>Nene Toys store page</h1>\
                         <p>Wooden fire truck with magnetic tubes, hydrant and a 360 degree \
                         swivel scale, educational toy for kids.</p></body></html>",
                    ),
            )
            .mount(&server)
            .await;
        let f = fetcher().unwrap();
        let r = f
            .fetch(
                &format!("{}/page", server.uri()),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(10_000),
            )
            .await
            .unwrap();
        assert_eq!(r.rendered_with.as_deref(), Some("impersonated-http"));
        assert_eq!(r.status_code, 200);
        assert!(r.html.contains("Nene Toys"));
        assert!(r.warning.is_none());
        assert!(r.warnings.is_empty());
        assert!(r.final_url.is_none());
    }

    /// An interstitial-shaped 200 (Amazon-style) comes back as a plain
    /// FetchResult; the ACCEPT decision belongs to the caller's gate, not to
    /// the fetcher. The fetcher only stamps the challenge-header marker when
    /// a header announces one, which this fixture has none of.
    #[tokio::test]
    async fn wiremock_interstitial_body_is_returned_verbatim() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_string(
                        "<html><body>Fai clic sul pulsante qui sotto per continuare \
                         a fare acquisti</body></html>",
                    ),
            )
            .mount(&server)
            .await;
        let f = fetcher().unwrap();
        let r = f
            .fetch(
                &server.uri(),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(10_000),
            )
            .await
            .unwrap();
        assert!(r.html.contains("fare acquisti"));
        assert!(r.warning.is_none());
    }

    /// SSRF: a redirect whose target violates the shared URL policy is
    /// blocked. The fixture uses an over-length target (2048+ chars) because
    /// that check runs BEFORE the test loopback escape hatch, so the outcome
    /// does not depend on which sibling test currently holds the env var.
    /// Proves the wreq policy really calls the
    /// `validate_safe_url_blocking_resolved` owner.
    #[tokio::test]
    async fn wiremock_ssrf_redirect_is_blocked() {
        let server = wiremock::MockServer::start().await;
        let oversized_target = format!("http://example.com/{}", "a".repeat(2100));
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(302).insert_header("location", oversized_target),
            )
            .mount(&server)
            .await;
        let f = fetcher().unwrap();
        let r = f
            .fetch(
                &server.uri(),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(10_000),
            )
            .await;
        match r {
            Err(CrwError::HttpError(msg)) => {
                assert!(
                    msg.contains("redirect blocked"),
                    "expected the SSRF policy to block, got: {msg}"
                );
            }
            other => panic!("expected redirect-blocked error, got: {other:?}"),
        }
    }

    /// A same-origin redirect chain is followed and `final_url` reflects it.
    /// Needs the loopback escape hatch, same as `http_only_tests.rs`.
    #[tokio::test]
    // Holds ENV_LOCK across awaits on purpose: serialises against the other
    // env-mutating tests while this one flips a process-wide env var.
    #[allow(clippy::await_holding_lock)]
    async fn wiremock_follows_safe_redirect_and_reports_final_url() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("CRW_ALLOW_LOOPBACK_FOR_TESTS", "1") };
        let server = wiremock::MockServer::start().await;
        let dest = format!("{}/final", server.uri());
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/start"))
            .respond_with(wiremock::ResponseTemplate::new(302).insert_header("location", dest))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/final"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_string("<html><body>moved fine</body></html>"),
            )
            .mount(&server)
            .await;
        let f = fetcher().unwrap();
        let res = f
            .fetch(
                &format!("{}/start", server.uri()),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(10_000),
            )
            .await;
        unsafe { std::env::remove_var("CRW_ALLOW_LOOPBACK_FOR_TESTS") };
        let r = res.expect("redirect must be followed once the escape hatch is set");
        assert_eq!(r.status_code, 200);
        assert_eq!(
            r.final_url.as_deref(),
            Some(format!("{}/final", server.uri()).as_str())
        );
    }

    /// Challenge-header stamping: a `cf-mitigated: challenge` response
    /// carries the same `warning` marker the auto arm matches on.
    #[tokio::test]
    async fn wiremock_challenge_header_is_stamped_into_warning() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(403)
                    .insert_header("cf-mitigated", "challenge")
                    .insert_header("content-type", "text/html")
                    .set_body_string("<html>challenge</html>"),
            )
            .mount(&server)
            .await;
        let f = fetcher().unwrap();
        let r = f
            .fetch(
                &server.uri(),
                &HashMap::new(),
                None,
                Deadline::from_request_ms(10_000),
            )
            .await
            .unwrap();
        assert_eq!(r.warning.as_deref(), Some("cloudflare_mitigated"));
        assert_eq!(r.status_code, 403);
    }

    /// Fingerprint-relevant caller headers are DROPPED: the emulation preset
    /// owns UA / sec-ch-* / accept-encoding, and a caller UA over a Chrome149
    /// JA3 is the exact incoherence TLS-fingerprint walls test for. The mock
    /// only answers a Chrome-looking UA, so the fetch succeeding and the mock
    /// verifying prove the preset's UA survived and the caller's did not.
    #[tokio::test]
    async fn wiremock_drops_fingerprint_relevant_caller_headers() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::header_regex(
                "user-agent",
                r"Chrome/\d+",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_string(
                        "<html><body><p>A perfectly ordinary page with more than fifty \
                         characters of visible text, so the accept gate has something \
                         to work with.</p></body></html>",
                    ),
            )
            .expect(1)
            .mount(&server)
            .await;
        let f = fetcher().unwrap();
        let mut headers = HashMap::new();
        headers.insert("User-Agent".to_string(), "my-scraper/0.1".to_string());
        headers.insert("X-Custom-Header".to_string(), "kept".to_string());
        let r = f
            .fetch(
                &server.uri(),
                &headers,
                None,
                Deadline::from_request_ms(10_000),
            )
            .await
            .unwrap();
        assert_eq!(r.status_code, 200);
        server.verify().await;
    }

    #[tokio::test]
    async fn capped_reader_rejects_chunked_body_over_limit() {
        let chunk = "a".repeat(1024);
        let body = format!("400\r\n{chunk}\r\n400\r\n{chunk}\r\n0\r\n\r\n").into_bytes();
        let url = http_only::serve_raw(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            body,
        );
        let response = fetcher().unwrap().client.get(&url).send().await.unwrap();
        let error = read_body_capped(response, 1500, &url).await.unwrap_err();
        assert!(matches!(error, CrwError::HttpError(message) if message.contains("too large")));
    }

    #[tokio::test]
    async fn capped_reader_measures_decoded_gzip_bytes() {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&vec![b'a'; 1024 * 1024]).unwrap();
        let compressed = encoder.finish().unwrap();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            compressed.len()
        );
        let url = http_only::serve_raw(head, compressed);
        let response = fetcher().unwrap().client.get(&url).send().await.unwrap();
        let error = read_body_capped(response, 128 * 1024, &url)
            .await
            .unwrap_err();
        assert!(matches!(error, CrwError::HttpError(message) if message.contains("too large")));
    }

    // ── Live network verification (#[ignore], run explicitly) ───────────

    /// The tier's reason to exist: the Amazon product page must come back
    /// with the real title, not the TLS-fingerprint interstitial.
    #[tokio::test]
    #[ignore]
    async fn live_amazon_it_product_page_serves_real_content() {
        let f = fetcher().unwrap();
        let r = f
            .fetch(
                "https://www.amazon.it/dp/B0FHQGLXBP",
                &HashMap::new(),
                None,
                Deadline::from_request_ms(30_000),
            )
            .await
            .unwrap();
        assert_eq!(r.rendered_with.as_deref(), Some("impersonated-http"));
        let lower = r.html.to_lowercase();
        assert!(
            lower.contains("nene toys"),
            "expected the product title in the body"
        );
        assert!(
            !lower.contains("fai clic sul pulsante qui sotto"),
            "got the interstitial, not the product page"
        );
    }

    /// example.com plus an ebay.it search URL via the PLAIN HttpFetcher:
    /// the auto chain must keep serving clean sites with rendered_with
    /// "http" (the new tier never widens its trigger).
    #[tokio::test]
    #[ignore]
    async fn live_plain_http_keeps_clean_sites_on_http() {
        let http = http_only::HttpFetcher::new(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36",
            None,
            false,
        );
        for url in [
            "https://example.com",
            "https://www.ebay.it/sch/i.html?_nkw=lego",
        ] {
            let r = http
                .fetch(
                    url,
                    &HashMap::new(),
                    None,
                    Deadline::from_request_ms(30_000),
                )
                .await
                .unwrap();
            assert_eq!(
                r.rendered_with.as_deref(),
                Some("http"),
                "clean site {url} must stay on the plain tier"
            );
            assert!(
                !crate::detector::looks_like_generic_bot_wall(&r.html, r.truncated),
                "clean site {url} must not read as a bot wall"
            );
        }
    }

    /// Probe: what does the plain reqwest tier get from the Amazon URL on
    /// this network right now? Decides what the e2e test below may assert:
    /// the wall is served inconsistently to some networks/egresses.
    #[tokio::test]
    #[ignore]
    async fn live_probe_plain_http_amazon_shape() {
        let http = http_only::HttpFetcher::new("crw/0.34", None, false);
        let r = http
            .fetch(
                "https://www.amazon.it/dp/B0FHQGLXBP",
                &HashMap::new(),
                None,
                Deadline::from_request_ms(30_000),
            )
            .await
            .unwrap();
        let wall = crate::detector::looks_like_generic_bot_wall(&r.html, r.truncated);
        let signal = crw_extract::antibot::classify(Some(r.status_code), &r.html).signal;
        println!(
            "plain status={} len={} wall={} signal={:?} snippet={:?}",
            r.status_code,
            r.html.len(),
            wall,
            signal,
            r.html.chars().take(200).collect::<String>()
        );
    }

    /// End-to-end AUTO chain against the real wall: plain HTTP first (serves
    /// the interstitial), the impersonated hop clears it, no browser tier
    /// exists in this renderer (mode=none), so the hop MUST be what wins.
    /// Also the design's suggested periodic live probe: when Amazon rotates
    /// its wall past Chrome149, this is the test that goes red.
    #[tokio::test]
    #[ignore]
    async fn live_auto_chain_serves_amazon_via_the_hop() {
        use crw_core::types::{FailoverErrorKind, RenderDecision, RendererKind};

        let cfg = crw_core::config::RendererConfig::default(); // impersonated ON, no JS tiers
        let ua = "crw/0.34";
        let renderer = crate::FallbackRenderer::new(
            &cfg,
            ua,
            None,
            &crw_core::config::StealthConfig::default(),
        )
        .unwrap_or_else(|e| panic!("renderer build failed: {e}"));
        let r = renderer
            .fetch(
                "https://www.amazon.it/dp/B0FHQGLXBP",
                &HashMap::new(),
                None, // auto
                None,
                None,
                Deadline::from_request_ms(45_000),
            )
            .await
            .unwrap_or_else(|e| panic!("auto fetch failed: {e}"));
        assert!(
            r.html.to_lowercase().contains("nene toys"),
            "neither tier served the product page"
        );
        // The wall is served PER REQUEST (rate/egress dependent): the same
        // network gets the interstitial on one fetch and the real page on the
        // next, so the chain may legitimately land on either tier. Both
        // outcomes are asserted for internal consistency.
        match r.rendered_with.as_deref() {
            Some("impersonated-http") => {
                // The plain tier hit the wall and the hop cleared it.
                assert_eq!(
                    r.render_decision,
                    Some(RenderDecision::Failover {
                        chain: vec![RendererKind::Http, RendererKind::ImpersonatedHttp],
                        reason: FailoverErrorKind::VendorBlock,
                    })
                );
            }
            Some("http") => {
                // The plain tier was served the real page; the clean-site
                // contract holds (hop never consulted, decision AutoDefault).
                assert_eq!(
                    r.render_decision,
                    Some(RenderDecision::AutoDefault {
                        chosen: RendererKind::Http
                    })
                );
                assert!(
                    !crate::detector::looks_like_generic_bot_wall(&r.html, r.truncated),
                    "the plain tier's result IS the wall; the hop should have fired"
                );
            }
            other => panic!("unexpected rendered_with: {other:?}"),
        }
    }
}
