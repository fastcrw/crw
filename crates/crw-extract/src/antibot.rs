//! Anti-bot detection — port of crawl4ai's `antibot_detector.py`.
//!
//! Layered detection:
//! - Tier 1 patterns (structural markers) trigger on any page size.
//! - Tier 2 patterns (generic terms) trigger on short pages or 4xx/5xx.
//! - Tier 3 structural integrity catches silent blocks / empty shells.
//! - Status-aware: 429 → RateLimited, 403/503 → block unless body is data.
//!
//! Detection philosophy: false positives are cheap (the fallback mechanism
//! rescues them); false negatives mean garbage in the output.

use once_cell::sync::Lazy;
use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AntibotSignal {
    None,
    Cloudflare,
    Datadome,
    PerimeterX,
    Akamai,
    Imperva,
    Sucuri,
    Kasada,
    Vercel,
    NetworkSecurity,
    RateLimited,
    GenericBlock,
    StructuralFailure,
}

impl AntibotSignal {
    pub fn class_name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Cloudflare => "cloudflare",
            Self::Datadome => "datadome",
            Self::PerimeterX => "perimeterx",
            Self::Akamai => "akamai",
            Self::Imperva => "imperva",
            Self::Sucuri => "sucuri",
            Self::Kasada => "kasada",
            Self::Vercel => "vercel",
            Self::NetworkSecurity => "network_security",
            Self::RateLimited => "rate_limited",
            Self::GenericBlock => "generic_block",
            Self::StructuralFailure => "structural_failure",
        }
    }

    pub fn is_blocked(&self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Debug, Clone)]
pub struct AntibotResult {
    pub signal: AntibotSignal,
    pub reason: String,
}

impl AntibotResult {
    pub fn none() -> Self {
        Self {
            signal: AntibotSignal::None,
            reason: String::new(),
        }
    }
    fn block(signal: AntibotSignal, reason: impl Into<String>) -> Self {
        Self {
            signal,
            reason: reason.into(),
        }
    }
}

const TIER2_MAX_SIZE: usize = 10_000;
const STRUCTURAL_MAX_SIZE: usize = 50_000;
const BLOCK_PAGE_MAX_SIZE: usize = 5_000;
const EMPTY_CONTENT_THRESHOLD: usize = 100;
const TIER1_HEAD_BYTES: usize = 15_000;
const DEEP_SCAN_HEAD_BYTES: usize = 500_000;
const DEEP_SCAN_SNIPPET_BYTES: usize = 30_000;

type SignalPattern = (Regex, AntibotSignal, &'static str);

static TIER1_PATTERNS: Lazy<Vec<SignalPattern>> = Lazy::new(|| {
    vec![
        (
            // "Reference Error: 0.333b2f17.1793287426.7e59a54" is the same
            // ID on the block page adidas serves.
            Regex::new(r"(?i)Reference\s*(?:#|Error\s*:)\s*\d+\.[0-9a-f]+\.\d+\.[0-9a-f]+")
                .unwrap(),
            AntibotSignal::Akamai,
            "Akamai block (Reference #)",
        ),
        // Akamai's interstitial: a proof-of-work script that reloads into the
        // page when it passes, or shows a behavioral challenge when it does not.
        (
            Regex::new(r#"(?i)id=["']sec-if-cpt-container|sec-cpt-if"#).unwrap(),
            AntibotSignal::Akamai,
            "Akamai challenge (sec-cpt)",
        ),
        (
            Regex::new(r"(?i)Pardon\s+Our\s+Interruption").unwrap(),
            AntibotSignal::Akamai,
            "Akamai challenge (Pardon Our Interruption)",
        ),
        (
            Regex::new(r"(?is)challenge-form.*?__cf_chl_f_tk=").unwrap(),
            AntibotSignal::Cloudflare,
            "Cloudflare challenge form",
        ),
        (
            Regex::new(r#"(?i)<span\s+class="cf-error-code">\d{4}</span>"#).unwrap(),
            AntibotSignal::Cloudflare,
            "Cloudflare firewall block",
        ),
        (
            Regex::new(r"(?i)/cdn-cgi/challenge-platform/\S+orchestrate").unwrap(),
            AntibotSignal::Cloudflare,
            "Cloudflare JS challenge",
        ),
        (
            // Not `window._pxAppId =`: PerimeterX's sensor sets it on every
            // page it protects (Walmart, Wayfair, Trulia), so it flagged
            // pages that had loaded. Its block page is the one with the
            // press-and-hold captcha and this title.
            Regex::new(
                r#"(?i)id=["']px-captcha["']|<title>\s*Access\s+to\s+this\s+page\s+has\s+been\s+denied"#,
            )
            .unwrap(),
            AntibotSignal::PerimeterX,
            "PerimeterX block",
        ),
        (
            Regex::new(r"(?i)captcha\.px-cdn\.net").unwrap(),
            AntibotSignal::PerimeterX,
            "PerimeterX captcha",
        ),
        (
            Regex::new(r"(?i)captcha-delivery\.com").unwrap(),
            AntibotSignal::Datadome,
            "DataDome captcha",
        ),
        (
            Regex::new(r"(?i)_Incapsula_Resource").unwrap(),
            AntibotSignal::Imperva,
            "Imperva/Incapsula block",
        ),
        (
            Regex::new(r"(?i)Incapsula\s+incident\s+ID").unwrap(),
            AntibotSignal::Imperva,
            "Imperva/Incapsula incident",
        ),
        (
            Regex::new(r"(?i)Sucuri\s+WebSite\s+Firewall").unwrap(),
            AntibotSignal::Sucuri,
            "Sucuri firewall block",
        ),
        (
            Regex::new(r"(?i)KPSDK\.scriptStart\s*=\s*KPSDK\.now\(\)").unwrap(),
            AntibotSignal::Kasada,
            "Kasada challenge",
        ),
        (
            Regex::new(r"(?i)blocked\s+by\s+network\s+security").unwrap(),
            AntibotSignal::NetworkSecurity,
            "Network security block",
        ),
        // Vercel's bot-check interstitial ("Vercel Security Checkpoint —
        // We're verifying your browser" / "Failed to verify your browser").
        // Require both the heading AND the verifying/failed phrase within a
        // bounded window so a page merely mentioning "Vercel" (a common
        // hosting platform name) in prose cannot trip this alone.
        (
            Regex::new(
                r"(?i)Vercel\s+Security\s+Checkpoint[\s\S]{0,300}(?:verifying|Failed\s+to\s+verify)\s+your\s+browser",
            )
            .unwrap(),
            AntibotSignal::Vercel,
            "Vercel security checkpoint",
        ),
        // Google's rate-limit page ("Error 429 (Too Many Requests)!!1"). Google
        // serves it with a real 429 over HTTP, but lightpanda/CDP renderers
        // report the navigation as HTTP 200 with the error page as the body —
        // so the `status == Some(429)` check below never fires and the page
        // slips through as a successful render. Match the body so the failover
        // loop escalates (lightpanda -> chrome -> chrome_proxy/residential).
        (
            Regex::new(r"(?i)you\s+have\s+sent\s+too\s+many\s+requests\s+to\s+us").unwrap(),
            AntibotSignal::RateLimited,
            "Google rate limit (sent too many requests)",
        ),
        (
            Regex::new(r"(?i)Error\s+429\s*\(Too\s+Many\s+Requests").unwrap(),
            AntibotSignal::RateLimited,
            "Google rate limit (Error 429 page)",
        ),
        // Reddit's reCAPTCHA wall, served with HTTP 200 under its own title.
        (
            Regex::new(r"(?i)<title>[^<]*Prove\s+your\s+humanity").unwrap(),
            AntibotSignal::GenericBlock,
            "Captcha wall (Prove your humanity)",
        ),
        // Google's bot wall served with HTTP 200 (the /sorry reCAPTCHA page).
        (
            Regex::new(r"(?i)unusual\s+traffic\s+from\s+your\s+computer\s+network").unwrap(),
            AntibotSignal::GenericBlock,
            "Google bot wall (unusual traffic)",
        ),
    ]
});

static TIER2_PATTERNS: Lazy<Vec<SignalPattern>> = Lazy::new(|| {
    vec![
        (
            Regex::new(r"(?i)Access\s+Denied").unwrap(),
            AntibotSignal::GenericBlock,
            "Access Denied on short page",
        ),
        (
            Regex::new(r"(?i)Checking\s+your\s+browser").unwrap(),
            AntibotSignal::Cloudflare,
            "Cloudflare browser check",
        ),
        (
            Regex::new(r"(?i)<title>\s*Just\s+a\s+moment").unwrap(),
            AntibotSignal::Cloudflare,
            "Cloudflare interstitial",
        ),
        (
            Regex::new(r#"(?i)class=["']g-recaptcha["']"#).unwrap(),
            AntibotSignal::GenericBlock,
            "reCAPTCHA on block page",
        ),
        (
            Regex::new(r#"(?i)class=["']h-captcha["']"#).unwrap(),
            AntibotSignal::GenericBlock,
            "hCaptcha on block page",
        ),
        (
            Regex::new(r"(?i)Access\s+to\s+This\s+Page\s+Has\s+Been\s+Blocked").unwrap(),
            AntibotSignal::PerimeterX,
            "PerimeterX block page",
        ),
        (
            Regex::new(r"(?i)blocked\s+by\s+security").unwrap(),
            AntibotSignal::GenericBlock,
            "Blocked by security",
        ),
        (
            Regex::new(r"(?i)Request\s+unsuccessful").unwrap(),
            AntibotSignal::Imperva,
            "Request unsuccessful (Imperva)",
        ),
    ]
});

static SCRIPT_BLOCK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<script\b[\s\S]*?</script>").unwrap());
static STYLE_BLOCK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<style\b[\s\S]*?</style>").unwrap());
/// Inline base64 `data:` URIs (e.g. an embedded logo) can be tens of KB of
/// opaque payload that pushes real block-page text past the deep-scan window.
/// Stripping them keeps the scanned snippet dense with meaningful markup.
static DATA_URI_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)data:[a-z0-9.+-]*/[a-z0-9.+-]*;base64,[a-z0-9+/=]+").unwrap());
static TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"<[^>]+>").unwrap());
static BODY_OPEN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)<body\b").unwrap());
static BODY_INNER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<body\b[^>]*>([\s\S]*)</body>").unwrap());
static SCRIPT_OPEN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)<script\b").unwrap());
static CONTENT_ELEMENTS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)<(?:p|h[1-6]|article|section|li|td|a|pre)\b").unwrap());
static JSON_PRE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?is)<body[^>]*>\s*<pre[^>]*>\s*[{\[]"#).unwrap());

/// Strip the three high-volume / low-signal regions — `<script>` and `<style>`
/// blocks plus inline base64 `data:` URIs — so the deep-scan snippet window
/// reaches real block-page text instead of being exhausted by embedded assets.
fn strip_noise(src: &str) -> String {
    let stripped = SCRIPT_BLOCK_RE.replace_all(src, "");
    let stripped = STYLE_BLOCK_RE.replace_all(&stripped, "");
    let stripped = DATA_URI_RE.replace_all(&stripped, "");
    stripped.into_owned()
}

fn looks_like_data(html: &str) -> bool {
    let stripped = html.trim();
    if stripped.is_empty() {
        return false;
    }
    let first = stripped.as_bytes()[0];
    if first == b'{' || first == b'[' {
        return true;
    }
    let head_lower: String = stripped
        .chars()
        .take(10)
        .flat_map(char::to_lowercase)
        .collect();
    if head_lower.starts_with("<html") || head_lower.starts_with("<!") {
        let prefix_end = stripped
            .char_indices()
            .nth(500)
            .map(|(i, _)| i)
            .unwrap_or(stripped.len());
        return JSON_PRE_RE.is_match(&stripped[..prefix_end]);
    }
    first == b'<'
}

fn structural_integrity_check(html: &str) -> AntibotResult {
    let html_len = html.len();
    if html_len > STRUCTURAL_MAX_SIZE || looks_like_data(html) {
        return AntibotResult::none();
    }

    if !BODY_OPEN_RE.is_match(html) {
        return AntibotResult::block(
            AntibotSignal::StructuralFailure,
            format!("Structural: no <body> tag ({html_len} bytes)"),
        );
    }

    let body_content = BODY_INNER_RE
        .captures(html)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
        .unwrap_or_else(|| html.to_string());
    let stripped = SCRIPT_BLOCK_RE.replace_all(&body_content, "");
    let stripped = STYLE_BLOCK_RE.replace_all(&stripped, "");
    let visible_text = TAG_RE.replace_all(&stripped, "");
    let visible_text = visible_text.trim();
    let visible_len = visible_text.chars().count();

    let mut signals: Vec<&'static str> = Vec::new();
    if visible_len < 50 {
        signals.push("minimal_text");
    }
    let content_count = CONTENT_ELEMENTS_RE.find_iter(html).count();
    if content_count == 0 {
        signals.push("no_content_elements");
    }
    let script_count = SCRIPT_OPEN_RE.find_iter(html).count();
    if script_count > 0 && content_count == 0 && visible_len < 100 {
        signals.push("script_heavy_shell");
    }

    if signals.len() >= 2 {
        return AntibotResult::block(
            AntibotSignal::StructuralFailure,
            format!(
                "Structural: {} ({} bytes, {} chars visible)",
                signals.join(", "),
                html_len,
                visible_len
            ),
        );
    }
    if signals.len() == 1 && html_len < BLOCK_PAGE_MAX_SIZE {
        return AntibotResult::block(
            AntibotSignal::StructuralFailure,
            format!(
                "Structural: {} on small page ({} bytes, {} chars visible)",
                signals[0], html_len, visible_len
            ),
        );
    }

    AntibotResult::none()
}

/// Classify a fetch result. Maps to crawl4ai's `is_blocked()` but returns a typed signal.
pub fn classify(status: Option<u16>, html: &str) -> AntibotResult {
    let html_len = html.len();

    if status == Some(429) {
        return AntibotResult::block(AntibotSignal::RateLimited, "HTTP 429 Too Many Requests");
    }
    if status == Some(521) {
        return AntibotResult::block(
            AntibotSignal::Cloudflare,
            "HTTP 521 Web server is down (Cloudflare)",
        );
    }

    let head_end = html
        .char_indices()
        .nth(TIER1_HEAD_BYTES)
        .map(|(i, _)| i)
        .unwrap_or(html.len());
    let snippet = &html[..head_end];
    if !snippet.is_empty() {
        for (pat, sig, reason) in TIER1_PATTERNS.iter() {
            if pat.is_match(snippet) {
                return AntibotResult::block(*sig, *reason);
            }
        }
    }

    if html_len > TIER1_HEAD_BYTES {
        let deep_end = html
            .char_indices()
            .nth(DEEP_SCAN_HEAD_BYTES)
            .map(|(i, _)| i)
            .unwrap_or(html.len());
        let deep_src = &html[..deep_end];
        let stripped = strip_noise(deep_src);
        let snippet_end = stripped
            .char_indices()
            .nth(DEEP_SCAN_SNIPPET_BYTES)
            .map(|(i, _)| i)
            .unwrap_or(stripped.len());
        let deep_snippet = &stripped[..snippet_end];
        for (pat, sig, reason) in TIER1_PATTERNS.iter() {
            if pat.is_match(deep_snippet) {
                return AntibotResult::block(*sig, *reason);
            }
        }
    }

    if matches!(status, Some(403) | Some(503)) && !looks_like_data(html) {
        if html_len < EMPTY_CONTENT_THRESHOLD {
            let s = status.unwrap();
            return AntibotResult::block(
                AntibotSignal::GenericBlock,
                format!("HTTP {s} with near-empty response ({html_len} bytes)"),
            );
        }
        let check_snippet: String = if html_len > TIER2_MAX_SIZE {
            let deep_end = html
                .char_indices()
                .nth(DEEP_SCAN_HEAD_BYTES)
                .map(|(i, _)| i)
                .unwrap_or(html.len());
            let stripped = strip_noise(&html[..deep_end]);
            let snippet_end = stripped
                .char_indices()
                .nth(DEEP_SCAN_SNIPPET_BYTES)
                .map(|(i, _)| i)
                .unwrap_or(stripped.len());
            stripped[..snippet_end].to_string()
        } else {
            snippet.to_string()
        };
        for (pat, sig, reason) in TIER2_PATTERNS.iter() {
            if pat.is_match(&check_snippet) {
                let s = status.unwrap();
                return AntibotResult::block(
                    *sig,
                    format!("{reason} (HTTP {s}, {html_len} bytes)"),
                );
            }
        }
        let s = status.unwrap();
        return AntibotResult::block(
            AntibotSignal::GenericBlock,
            format!("HTTP {s} with HTML content ({html_len} bytes)"),
        );
    }

    if let Some(s) = status
        && s >= 400
        && html_len < TIER2_MAX_SIZE
    {
        for (pat, sig, reason) in TIER2_PATTERNS.iter() {
            if pat.is_match(snippet) {
                return AntibotResult::block(
                    *sig,
                    format!("{reason} (HTTP {s}, {html_len} bytes)"),
                );
            }
        }
    }

    if status == Some(200) {
        let trimmed = html.trim();
        if trimmed.len() < EMPTY_CONTENT_THRESHOLD && !looks_like_data(html) {
            return AntibotResult::block(
                AntibotSignal::StructuralFailure,
                format!("Near-empty content ({} bytes) with HTTP 200", trimmed.len()),
            );
        }
    }

    structural_integrity_check(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloudflare_challenge_form_detected() {
        let html = r#"<html><body><form id="challenge-form" action="/cdn-cgi/?__cf_chl_f_tk=abc"></form></body></html>"#;
        let r = classify(Some(403), html);
        assert_eq!(r.signal, AntibotSignal::Cloudflare);
    }

    #[test]
    fn cloudflare_error_code_span_detected() {
        let html = r#"<html><body><span class="cf-error-code">1020</span></body></html>"#;
        let r = classify(Some(403), html);
        assert_eq!(r.signal, AntibotSignal::Cloudflare);
    }

    #[test]
    fn cloudflare_521_status_alone_detected() {
        let r = classify(Some(521), "");
        assert_eq!(r.signal, AntibotSignal::Cloudflare);
    }

    #[test]
    fn datadome_captcha_detected() {
        let html = r#"<html><body><script src="https://captcha-delivery.com/c.js"></script></body></html>"#;
        let r = classify(Some(403), html);
        assert_eq!(r.signal, AntibotSignal::Datadome);
    }

    #[test]
    fn perimeterx_block_page_detected() {
        let html = r#"<html><head><title>Access to this page has been denied</title></head><body><script>window._pxAppId = "PXabc";</script><div id="px-captcha"></div></body></html>"#;
        let r = classify(Some(403), html);
        assert_eq!(r.signal, AntibotSignal::PerimeterX);
    }

    #[test]
    fn perimeterx_sensor_on_a_page_that_loaded_is_not_a_block() {
        let body =
            "<p>Wayfair.com - Online Home Store for Furniture, Decor, Outdoors &amp; More</p>"
                .repeat(20);
        let html = format!(
            r#"<html><head><script>window._pxAppId = "PXabc";</script></head><body>{body}</body></html>"#
        );
        assert_eq!(classify(Some(200), &html).signal, AntibotSignal::None);
    }

    #[test]
    fn akamai_reference_error_detected() {
        let html = "<html><body><p>Unfortunately we are unable to give you access to our site at this time.</p><p>Reference Error: 0.333b2f17.1793287426.7e59a54</p></body></html>";
        assert_eq!(classify(Some(200), html).signal, AntibotSignal::Akamai);
    }

    #[test]
    fn reddit_captcha_wall_detected() {
        let html = "<html><head><title>Reddit - Prove your humanity</title></head><body><h1>Prove your humanity</h1><p>Complete the challenge below and let us know you’re a real person.</p></body></html>";
        assert_eq!(
            classify(Some(200), html).signal,
            AntibotSignal::GenericBlock
        );
    }

    #[test]
    fn akamai_interstitial_detected() {
        let html = r#"<html><body><div id="sec-if-cpt-container" role="main" style="display: none"><div class="behavioral-content"></div></div></body></html>"#;
        assert_eq!(classify(Some(200), html).signal, AntibotSignal::Akamai);
    }

    #[test]
    fn akamai_reference_detected() {
        let html = "<html><body>Reference #18.2d351ab8.1557333295.a4e16ab</body></html>";
        let r = classify(Some(403), html);
        assert_eq!(r.signal, AntibotSignal::Akamai);
    }

    #[test]
    fn imperva_incapsula_detected() {
        let html = "<html><body>Request unsuccessful. Incapsula incident ID: 123</body></html>";
        let r = classify(Some(403), html);
        assert_eq!(r.signal, AntibotSignal::Imperva);
    }

    #[test]
    fn sucuri_detected() {
        let html = "<html><body>Sucuri WebSite Firewall - CloudProxy</body></html>";
        let r = classify(Some(403), html);
        assert_eq!(r.signal, AntibotSignal::Sucuri);
    }

    #[test]
    fn kasada_detected() {
        let html = "<html><body><script>KPSDK.scriptStart = KPSDK.now()</script></body></html>";
        let r = classify(Some(200), html);
        assert_eq!(r.signal, AntibotSignal::Kasada);
    }

    #[test]
    fn vercel_security_checkpoint_detected() {
        let html = "<html><body><h1>Vercel Security Checkpoint</h1>\
            <p>We're verifying your browser</p></body></html>";
        let r = classify(Some(200), html);
        assert_eq!(r.signal, AntibotSignal::Vercel);
        assert_eq!(r.signal.class_name(), "vercel");
    }

    #[test]
    fn vercel_security_checkpoint_failed_variant_detected() {
        let html = "<html><body><h1>Vercel Security Checkpoint</h1>\
            <p>Failed to verify your browser</p><p>Code 21</p></body></html>";
        let r = classify(Some(200), html);
        assert_eq!(r.signal, AntibotSignal::Vercel);
    }

    #[test]
    fn vercel_mention_alone_is_not_enough() {
        // Negative: a page that merely mentions Vercel (a common hosting
        // platform) in prose, with no checkpoint heading, must not be flagged.
        let html = "<html><body><article><p>This site is deployed on Vercel, \
            a popular platform for hosting frontend applications and static \
            sites with automatic previews on every pull request.</p></article>\
            </body></html>";
        let r = classify(Some(200), html);
        assert_eq!(r.signal, AntibotSignal::None);
    }

    #[test]
    fn network_security_block_detected() {
        // Reddit-class WAF page: served with HTTP 200, no vendor signature.
        let html =
            "<html><body>You've been blocked by network security. Contact support.</body></html>";
        let r = classify(Some(200), html);
        assert_eq!(r.signal, AntibotSignal::NetworkSecurity);
    }

    #[test]
    fn network_security_block_detected_behind_large_data_uri() {
        // Reddit-class WAF page in the wild: the "blocked by network security"
        // text sits *after* a ~90KB inline base64 data-URI (an embedded logo).
        // Without stripping the data-URI the phrase lands past the deep-scan
        // snippet window and the block goes undetected — the bug this guards.
        let data_uri = format!("data:image/png;base64,{}", "A".repeat(90_000));
        let html = format!(
            "<html><body><img src=\"{data_uri}\"/>\
             <p>You've been blocked by network security. Contact support.</p>\
             </body></html>"
        );
        assert!(html.len() > DEEP_SCAN_SNIPPET_BYTES);
        let r = classify(Some(200), &html);
        assert_eq!(r.signal, AntibotSignal::NetworkSecurity);
    }

    #[test]
    fn short_legit_page_stays_none() {
        // ~340 chars of real content on a 200 — must not trip structural
        // failure. Boundary case for the in-loop classifier wiring.
        let html = format!(
            "<!doctype html><html><head><title>Note</title></head>\
             <body><article><p>{}</p></article></body></html>",
            "This is a short but legitimate article paragraph with enough words. ".repeat(5)
        );
        let r = classify(Some(200), &html);
        assert_eq!(r.signal, AntibotSignal::None);
    }

    #[test]
    fn rate_limited_429() {
        let r = classify(Some(429), "<html><body>slow down</body></html>");
        assert_eq!(r.signal, AntibotSignal::RateLimited);
    }

    #[test]
    fn google_429_page_served_with_200_is_blocked() {
        // Real Google rate-limit page as returned by lightpanda/CDP: the HTTP
        // status surfaces as 200 but the body is Google's 429 error page.
        // Without body matching this slips through as a successful render and
        // the failover loop never escalates to chrome_proxy.
        let html = "<html lang=\"en\" dir=\"ltr\"><head><meta charset=\"utf-8\">\
            <title>Error 429 (Too Many Requests)!!1</title></head><body>\
            <main id=\"af-error-container\" role=\"main\"><a href=\"//www.google.com\">\
            </a><p><b>429.</b> That\u{2019}s an error.</p><p>We're sorry, but you \
            have sent too many requests to us recently. Please try again later. \
            That\u{2019}s all we know.</p></main></body></html>";
        let r = classify(Some(200), html);
        assert_eq!(r.signal, AntibotSignal::RateLimited);
    }

    #[test]
    fn google_unusual_traffic_bot_wall_is_blocked() {
        let html = "<html><head><title>Sorry...</title></head><body>\
            <p>Our systems have detected unusual traffic from your computer \
            network. This page checks to see if it's really you sending the \
            requests.</p></body></html>";
        let r = classify(Some(200), html);
        assert_eq!(r.signal, AntibotSignal::GenericBlock);
    }

    #[test]
    fn cloudflare_just_a_moment_short_page() {
        let mut html = String::from("<html><head><title>Just a moment...</title></head><body>");
        html.push_str(&"<p>checking</p>".repeat(20));
        html.push_str("</body></html>");
        let r = classify(Some(403), &html);
        assert_eq!(r.signal, AntibotSignal::Cloudflare);
    }

    #[test]
    fn normal_article_returns_none() {
        let html = r#"<!doctype html><html><head><title>Article</title></head>
            <body><article><h1>Hello</h1>
            <p>This is a normal article with plenty of meaningful text content
            describing something interesting at length so it will not trigger
            any structural failure heuristics in the antibot detector.</p>
            <p>Another paragraph adds more body text so visible chars exceed
            the structural threshold easily.</p></article></body></html>"#;
        let r = classify(Some(200), html);
        assert_eq!(r.signal, AntibotSignal::None);
    }

    #[test]
    fn json_data_response_not_blocked_on_403() {
        let html = r#"{"error":"forbidden","code":403}"#;
        let r = classify(Some(403), html);
        assert_eq!(r.signal, AntibotSignal::None);
    }

    #[test]
    fn empty_200_flagged_as_structural() {
        let r = classify(Some(200), "");
        assert_eq!(r.signal, AntibotSignal::StructuralFailure);
    }

    #[test]
    fn structural_no_body_detected() {
        let html = "<html><head><title>x</title></head></html>";
        let r = classify(Some(200), html);
        assert_eq!(r.signal, AntibotSignal::StructuralFailure);
    }

    #[test]
    fn class_name_round_trip() {
        assert_eq!(AntibotSignal::Cloudflare.class_name(), "cloudflare");
        assert_eq!(AntibotSignal::None.class_name(), "none");
    }

    /// `BlockOutcome::message()` words a structural failure differently from a
    /// real vendor wall, and it selects on this exact string. The producer lives
    /// here and the consumer lives in crw-core, which cannot depend on this
    /// crate — so nothing but this test stops the two drifting apart. If they
    /// do, the branch silently never fires and thin pages go back to being
    /// reported as "Blocked by anti-bot".
    #[test]
    fn structural_failure_vendor_matches_classifier() {
        assert_eq!(
            AntibotSignal::StructuralFailure.class_name(),
            crw_core::types::STRUCTURAL_FAILURE_VENDOR
        );
    }
}
