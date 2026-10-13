use regex::Regex;
use std::sync::LazyLock;

/// Akamai bot-manager reference ID: `Reference #<digits>.<hex>.<digits>.<hex>`.
/// Used only in [`looks_like_vendor_block`] — see the Akamai arm there.
static AKAMAI_REF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Reference #\d+\.[0-9a-f]+\.\d+\.[0-9a-f]+").expect("static regex")
});

/// Heuristic: does the HTML look like an SPA shell that needs JS rendering?
pub fn needs_js_rendering(html: &str) -> bool {
    // Check up to 500KB — some pages have huge <head> sections (CSS, preloaded data)
    // and the <body> may start well beyond 50KB. Every fixed-cap prefix slice of the
    // page HTML here goes through `floor_char_boundary`: a page longer than the cap
    // can straddle it with a multibyte char, and slicing mid-char panics.
    let was_truncated = html.len() > 500_000;
    let lower = html[..html.floor_char_boundary(500_000)].to_lowercase();
    let body_len = extract_body_text_len(&lower, was_truncated);

    // Very short body text + presence of JS framework indicators.
    // After stripping script/style, most SPA shells have very little actual text.
    if body_len < 200 {
        let spa_indicators = [
            "id=\"root\"",
            "id=\"app\"",
            "id=\"__next\"",
            "id=\"__nuxt\"",
            "id=\"__gatsby\"",
            "id=\"svelte\"",
            "ng-app",
            "data-reactroot",
            "data-reactid",
            "data-remix-run",
            "data-sveltekit",
            "data-astro-",
            "<script src",
            "window.__initial_state__",
            "__next_data__",
            "__nuxt__",
            "__sveltekit_data",
            "window.__remixcontext",
            "window.__astro",
            "gatsby-focus-wrapper",
        ];
        if spa_indicators.iter().any(|ind| lower.contains(ind)) {
            return true;
        }
    }

    // Noscript tag with meaningful content suggests JS is needed.
    if lower.contains("<noscript>") && lower.contains("enable javascript") {
        return true;
    }

    // Framer / Webflow / other site builder markers (often fully JS-rendered)
    if body_len < 500 {
        let builder_indicators = [
            "framerusercontent.com",
            "webflow.io",
            "wixsite.com",
            "squarespace.com/universal",
        ];
        if builder_indicators.iter().any(|ind| lower.contains(ind)) {
            return true;
        }
    }

    // Bundler-heavy modern SPA: short body + many script tags. Catches sites
    // that don't expose a recognizable framework marker but ship most of their
    // content via client-side hydration. Threshold is conservative (5+ scripts,
    // body <1000 chars) — 3 scripts is a normal load (analytics + ads + a font
    // loader) on minimal static pages, so we require more before escalating.
    if body_len < 1000 {
        let script_count = lower.matches("<script").count();
        if script_count >= 5 {
            return true;
        }
        let storybook_indicators = [
            "id=\"storybook-root\"",
            "id=\"storybook-docs\"",
            "__storybook",
            "?path=/docs/",
            "/iframe.html",
        ];
        if storybook_indicators.iter().any(|ind| lower.contains(ind)) {
            return true;
        }
    }

    false
}

/// Detect generic anti-bot interstitials (non-Cloudflare): tiny pages whose
/// visible body text consists of a "verifying you're human" / "security check"
/// message. Matched only on visible body text so a JS bundle containing one
/// of these strings cannot false-positive.
/// `truncated` must be the renderer's own truncation flag for this body. A
/// render cut short by its budget (LightPanda hitting `nav_budget`) has no
/// closing `</body>`, and without that flag the body extraction below returns
/// an empty string, so the phrase list scans nothing and a wall is reported as
/// real content — which shipped the interstitial to callers as a billed
/// success. Pass `false` only when the body is genuinely complete.
pub fn looks_like_generic_bot_wall(html: &str, truncated: bool) -> bool {
    if html.len() > 80_000 {
        return false;
    }
    let lower = html.to_lowercase();
    // Most block shells wrap their text in <body>; extracting body-only text
    // keeps a phrase buried in a JS bundle from false-positiving. A few shells
    // (Wikimedia's Varnish error page) omit <body> entirely — the text sits in
    // <div>s directly under <html>, so body-only extraction returns "" and no
    // phrase can ever match. When an HTML document has no <body> tag, fall back
    // to the whole document with <script>/<style> stripped. The fallback is
    // gated on an <html>/<!doctype html> marker so a directly-scraped JSON / XML
    // / plain-text response (which never carries <body>) is NOT scanned — else a
    // small `{"error":"access_denied"}` payload would trip an existing phrase.
    // The 600-char cap below still guards against real articles.
    let body_stripped = if lower.contains("<body") {
        // A missing `</body>` here means the body was cut off, so read to the
        // end of what we have. The 500 KB scan cap cannot be the cause (this fn
        // bailed above at 80 KB), but the RENDERER can be: a budget-truncated
        // page arrives mid-document. Passing `false` unconditionally made every
        // such body extract as "" and hid the wall.
        body_html_without_scripts_lower(&lower, truncated)
    } else if lower.contains("<html") || lower.contains("<!doctype html") {
        strip_tag_blocks(&strip_tag_blocks(&lower, "script"), "style")
    } else {
        return false;
    };
    let body_text = visible_text_from_stripped_html(&body_stripped);
    if body_text.chars().filter(|c| !c.is_whitespace()).count() > 600 {
        return false;
    }

    let phrases = [
        "performing security verification",
        "verify you are human",
        "checking your browser",
        "enable javascript and cookies",
        "security check",
        "access denied",
        "request blocked",
        // CloudFront / AWS WAF generic block page. Title is the giveaway;
        // body content varies (geo-block, WAF rule, distribution misconfig).
        // All variants render an identical 403 shell with these strings.
        "the request could not be satisfied",
        "generated by cloudfront",
        // Akamai / AWS-style geo-block phrasing — also surfaces on some
        // origin-side firewall pages that don't say "blocked" outright.
        "configured to block access",
        // Wikimedia serves its datacenter-IP ban as an HTTP-200 static error
        // shell (no <body> tag). This canonical footer sentence is unique to
        // that page — a real article never carries it.
        "if you report this error to the wikimedia system administrators",
        // Amazon TLS-fingerprint interstitial (verified live 2026-09, ASIN
        // product pages served to non-browser TLS stacks). Only the FULL
        // verified sentence is listed, and only the it-IT one: the English
        // "click the button below to continue shopping" is ordinary retail
        // copy (empty-cart and order-confirmation pages carry it verbatim
        // under the 600-char cap) and was never verified against a live EN
        // wall, so it must NOT be here; a false wall here escalates to the
        // ladder AND latches the host to proxy egress for 10 minutes. Other
        // marketplaces' wall texts get added only after live verification.
        // Host strings are deliberately not matched; block detection stays
        // host-agnostic.
        "fai clic sul pulsante qui sotto per continuare a fare acquisti",
    ];
    phrases.iter().any(|p| body_text.contains(p))
}

/// Vendor-specific anti-bot block markers. Returns the matched vendor name
/// for logging/metrics, or `None` when no vendor signature is found.
///
/// Markers are curated durable signatures (owned CDN domains, public SDK
/// identifiers, vendor brand strings) — chosen for low false-positive risk
/// and resistance to cosmetic vendor updates. Pair with the catch-all
/// [`looks_like_generic_bot_wall`] phrase list, which handles vendors that
/// haven't been signature-mapped yet.
///
/// Scans only the first 15KB; vendor block pages are small and put their
/// markers in `<head>` or early body. Pages over 200KB return `None` —
/// real content dwarfs vendor block shells.
pub fn looks_like_vendor_block(html: &str) -> Option<&'static str> {
    if html.len() > 200_000 {
        return None;
    }
    let head = &html[..html.floor_char_boundary(15_000)];
    let lower_head = head.to_lowercase();

    // Cloudflare: challenge form with cf-managed token, error code span, or
    // challenge-platform JS loader. All three are unique to CF's anti-bot.
    if (lower_head.contains("challenge-form") && lower_head.contains("__cf_chl_f_tk="))
        || lower_head.contains("cf-error-code")
        || lower_head.contains("/cdn-cgi/challenge-platform/")
    {
        return Some("cloudflare");
    }

    // Akamai: bot-manager reference IDs follow `Reference #<hex>.<hex>.<hex>.<hex>`.
    // "Pardon Our Interruption" is the canonical block page headline.
    if lower_head.contains("pardon our interruption") || AKAMAI_REF_RE.is_match(head) {
        return Some("akamai");
    }

    // PerimeterX: window._pxAppId SDK assignment, owned captcha CDN.
    if lower_head.contains("window._pxappid =") || lower_head.contains("captcha.px-cdn.net") {
        return Some("perimeterx");
    }

    // DataDome: owned captcha delivery domain.
    if lower_head.contains("captcha-delivery.com") {
        return Some("datadome");
    }

    // Imperva / Incapsula: resource marker + incident ID phrasing.
    if lower_head.contains("_incapsula_resource") || lower_head.contains("incapsula incident id") {
        return Some("imperva");
    }

    // Sucuri: WAF block page brand string.
    if lower_head.contains("sucuri website firewall") {
        return Some("sucuri");
    }

    // Kasada: SDK signature.
    if lower_head.contains("kpsdk.scriptstart = kpsdk.now()") {
        return Some("kasada");
    }

    // CloudFront / AWS WAF (geo-block, distribution misconfig). Already
    // partly in the catch-all phrase list — capture as a vendor here for
    // telemetry split.
    if lower_head.contains("generated by cloudfront")
        || lower_head.contains("the request could not be satisfied")
    {
        return Some("cloudfront");
    }

    None
}

/// Returns true when an HTTP response yielded effectively no visible text in
/// the body (post script/style strip). Used by the renderer to decide whether
/// to escalate a "successful" HTTP fetch to JS rendering when no SPA marker
/// was recognized.
///
/// Distinct from [`needs_js_rendering`]: that one is a pre-fetch heuristic
/// on raw markup, looking for framework shells. This one is purely about
/// outcome — does the page have *any* content for an extractor to chew on.
pub fn looks_like_thin_html(html: &str) -> bool {
    let was_truncated = html.len() > 500_000;
    let lower = html[..html.floor_char_boundary(500_000)].to_lowercase();
    extract_body_text_len(&lower, was_truncated) < 200
}

/// Would a headless browser plausibly reveal MORE content than the raw HTTP
/// body? True when the page ships executable JS (an external `<script src=…>`
/// bundle or a non-trivial inline script) OR performs a client-side
/// `<meta http-equiv="refresh">` redirect (which HTTP clients don't follow but
/// a browser does). A thin page with none of these is already complete over
/// HTTP — a headless render reveals nothing and just adds seconds — so the
/// thin-content escalation is gated on this signal.
///
/// Pure-data script blocks (`application/json`, `application/ld+json`,
/// `importmap`, `speculationrules`) never execute, so they do NOT count.
pub fn warrants_browser_retry(html: &str) -> bool {
    let lower = html[..html.floor_char_boundary(500_000)].to_lowercase();

    // Client-side redirect a browser would follow to real content. Matched per
    // <meta> tag (http-equiv refresh + a url target in the SAME tag) so an
    // unrelated string elsewhere can't false-positive.
    for frag in lower.split("<meta").skip(1) {
        let tag = frag.split('>').next().unwrap_or("");
        if tag.contains("http-equiv") && tag.contains("refresh") && tag.contains("url=") {
            return true;
        }
    }

    for frag in lower.split("<script").skip(1) {
        let mut parts = frag.splitn(2, '>');
        let tag = parts.next().unwrap_or("");
        let after = parts.next().unwrap_or("");
        // External bundle → could inject content.
        if tag.contains("src=") {
            return true;
        }
        // Pure-data blocks never execute.
        let is_data = tag.contains("application/json")
            || tag.contains("application/ld+json")
            || tag.contains("importmap")
            || tag.contains("speculationrules");
        if is_data {
            continue;
        }
        // Inline executable script with a non-trivial body.
        let inline = after.split("</script>").next().unwrap_or("");
        if inline.trim().len() > 8 {
            return true;
        }
    }
    false
}

/// Returns true when an extracted markdown is below the floor used by the
/// renderer to decide a fetch produced effectively no extractable content.
/// Pair with [`looks_like_thin_html`] for a full thin-content judgment.
pub fn is_thin_markdown(markdown_len: usize) -> bool {
    markdown_len < 100
}

/// Reason a rendered page is considered a failed render. Returned by
/// [`looks_like_failed_render`] so callers can include the cause in failover
/// warnings or telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailedRenderReason {
    /// Next.js error boundary HTML was injected into the document. Indicates
    /// the framework caught an unhandled exception during hydration or
    /// rendering — the page the user wanted is not present.
    NextJsClientError,
    /// React rendered its production "Minified React error" placeholder. Same
    /// failure class as Next.js, but framework-agnostic.
    ReactMinifiedError,
    /// Next.js root `<div id="__next">` is present but empty (no hydration
    /// took place). Distinct from a generic placeholder because it specifically
    /// indicates an SPA whose JS never executed.
    EmptyNextRoot,
}

impl FailedRenderReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FailedRenderReason::NextJsClientError => "nextjs_client_error",
            FailedRenderReason::ReactMinifiedError => "react_minified_error",
            FailedRenderReason::EmptyNextRoot => "empty_next_root",
        }
    }
}

/// Detect framework-level render failures *in the HTML markup*. Only matches
/// DOM-specific markers (element ids, data attributes) — never visible body
/// text — to avoid false positives on pages that legitimately mention the
/// error string (e.g. a blog post about Next.js debugging).
///
/// Returns `None` when the page looks healthy.
pub fn looks_like_failed_render(html: &str) -> Option<FailedRenderReason> {
    // Bail out fast for very large pages — error boundary markup is small and
    // appears near the body root; scanning megabytes would cost more than it
    // gains. Most failed renders produce <30KB of HTML.
    if html.len() > 200_000 {
        return None;
    }
    let lower = html.to_lowercase();

    // Next.js App Router error boundary. Next renders an error UI with these
    // marker attributes when a client-side exception bubbles to the root.
    // Sources: next.js/packages/next/src/client/components/error-boundary.tsx
    if lower.contains("id=\"__next-error-") || lower.contains("data-nextjs-error") {
        return Some(FailedRenderReason::NextJsClientError);
    }

    // Next.js Pages Router error overlay (visible in dev) and the production
    // "Application error" fallback both render this id. The marker is a DOM
    // element id and only appears when Next chose to render its error path.
    if lower.contains("id=\"__next_error__\"") {
        return Some(FailedRenderReason::NextJsClientError);
    }

    // React production error: surfaces as a minified message with a numeric
    // code linking back to react.dev. Match the canonical anchor href since
    // that combination only exists when React rendered the error explainer.
    if lower.contains("https://react.dev/errors/")
        || lower.contains("https://reactjs.org/docs/error-decoder")
    {
        return Some(FailedRenderReason::ReactMinifiedError);
    }

    // Empty Next.js root shell: <div id="__next"></div> (or with whitespace).
    // The renderer returned the SSR shell but no hydration ran; the
    // user-visible content is missing.
    if let Some(start) = lower.find("id=\"__next\"") {
        let after_id = &lower[start..];
        if let Some(close) = after_id.find('>') {
            let tail = &after_id[close + 1..];
            if let Some(end) = tail.find("</div>") {
                let inner = tail[..end].trim();
                if inner.is_empty() {
                    return Some(FailedRenderReason::EmptyNextRoot);
                }
            }
        }
    }

    None
}

/// Check if rendered HTML is dominated by loading placeholders, spinners,
/// or chat-widget-only content. Used *after* JS rendering to detect cases
/// where the renderer returned early before the real content appeared
/// (common with slow React/Vite SPAs on underpowered renderers).
///
/// Markers are matched against *visible* body text only (tags and attributes
/// stripped) to avoid false positives from e.g. `<img alt="Loading...">` on
/// a page that actually has real content.
pub fn looks_like_loading_placeholder(html: &str) -> bool {
    // Bail out fast for large pages — real content dwarfs loading markers.
    if html.len() > 80_000 {
        return false;
    }
    let lower = html.to_lowercase();
    // Bailed above for `html.len() > 80_000`, so never truncated at the 500 KB cap.
    let body_stripped = body_html_without_scripts_lower(&lower, false);
    let body_text = visible_text_from_stripped_html(&body_stripped);
    let body_text_len = body_text.chars().filter(|c| !c.is_whitespace()).count();

    if body_text_len == 0 {
        return true;
    }

    // Short body + explicit loading text in VISIBLE text.
    if body_text_len < 400 {
        let loading_markers = [
            "loading...",
            "loading…",
            "please wait",
            "just a moment",
            "initializing",
            "preparing",
            "one moment",
        ];
        if loading_markers.iter().any(|m| body_text.contains(m)) {
            return true;
        }
    }

    // Very short body + spinner/loader DOM markers. Matched against body HTML
    // with <script>/<style> stripped, so inline JS like `'class="spinner"'`
    // does not trigger a false positive.
    if body_text_len < 200 {
        let spinner_markers = [
            "class=\"spinner",
            "class=\"loader",
            "class=\"loading",
            "class=\"preloader",
            "id=\"loader",
            "id=\"preloader",
            "aria-label=\"loading\"",
        ];
        if spinner_markers.iter().any(|m| body_stripped.contains(m)) {
            return true;
        }
    }

    false
}

/// Return the `<body>` of a lowercased HTML document with `<script>` and
/// `<style>` blocks removed. Remaining tags (and their attributes) are
/// preserved. Returns an empty string if no `<body>` is found.
fn body_html_without_scripts_lower(lower: &str, was_truncated: bool) -> String {
    let body_start = lower
        .find("<body")
        .and_then(|i| lower[i..].find('>').map(|j| i + j + 1));
    // A missing `</body>` means two different things. On a page that was cut at
    // the 500 KB scan cap, the tag simply sits past the cut and the real body
    // text is right here in the slice — measuring to the end of the slice is
    // correct (a 1.8 MB article was being called "thin" and needlessly escalated
    // to Chrome). On a NON-truncated page, a missing `</body>` is a genuinely
    // malformed / mid-stream-truncated response, and treating it as thin so it
    // escalates to a fresh render is the right recovery — keep that.
    let body_end = match lower.rfind("</body>") {
        Some(end) => Some(end),
        None if was_truncated => Some(lower.len()),
        None => None,
    };

    let body = match (body_start, body_end) {
        (Some(start), Some(end)) if start < end => &lower[start..end],
        _ => return String::new(),
    };

    let stripped = strip_tag_blocks(body, "script");
    strip_tag_blocks(&stripped, "style")
}

/// Strip all HTML tags (open/close, with attributes) from an already
/// script/style-stripped HTML fragment. Whitespace is collapsed.
fn visible_text_from_stripped_html(stripped: &str) -> String {
    let mut text = String::with_capacity(stripped.len());
    let mut in_tag = false;
    let mut prev_ws = true;
    for ch in stripped.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            if ch.is_whitespace() {
                if !prev_ws {
                    text.push(' ');
                    prev_ws = true;
                }
            } else {
                text.push(ch);
                prev_ws = false;
            }
        }
    }
    text
}

/// Rough estimate of non-whitespace text length inside `<body>` of a
/// lowercased HTML document. Returns `1000` as a "probably has content"
/// fallback if no `<body>` is found.
fn extract_body_text_len(lower: &str, was_truncated: bool) -> usize {
    if !lower.contains("<body") {
        return 1000;
    }
    let stripped = body_html_without_scripts_lower(lower, was_truncated);
    visible_text_from_stripped_html(&stripped)
        .chars()
        .filter(|c| !c.is_whitespace())
        .count()
}

/// Remove all `<tag ...>...</tag>` blocks from HTML. The input is assumed
/// to be already lowercased (callers pass lowercased HTML).
fn strip_tag_blocks(html: &str, tag: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut remaining = html;

    while let Some(start) = remaining.find(&open) {
        result.push_str(&remaining[..start]);
        let after_open = &remaining[start..];
        if let Some(end) = after_open.find(&close) {
            remaining = &after_open[end + close.len()..];
        } else {
            remaining = "";
            break;
        }
    }
    result.push_str(remaining);
    result
}

// ── Cloudflare challenge detection ───────────────────────────────────

/// Detect a Cloudflare anti-bot challenge / interstitial in the response.
///
/// Strategy: a single weak marker is not enough — most marketing pages
/// reference Cloudflare somewhere. We require either a *strong* marker
/// (uniquely tied to the challenge interstitial) or a *combination* of
/// two weak markers. This keeps false positives low while still catching
/// the JS-challenge HTML LightPanda fails to solve.
///
/// Pair with [`is_cloudflare_mitigated_header`] which uses the
/// `cf-mitigated` response header — that signal is independent of body
/// content and is the most reliable indicator.
pub fn looks_like_cloudflare_challenge(html: &str) -> bool {
    // Strong markers appear ONLY on the interstitial and can sit deep in the
    // body of a large managed-challenge page — measured at byte ~128k of a 275k
    // Glassdoor "Just a moment" page, inside a
    // `<script src="/cdn-cgi/challenge-platform/…/orchestrate/…">`. So scan them
    // regardless of the 80KB weak-marker cap, bounded to the first 512KB. The
    // markers are fixed-lowercase ASCII CF tokens, so match case-sensitively on
    // the raw bytes (no allocation on the hot per-attempt path; mirrors
    // `crw_crawl::single::classify_block`).
    const STRONG_SCAN_LIMIT: usize = 512 * 1024;
    let strong_src = &html[..html.floor_char_boundary(STRONG_SCAN_LIMIT)];
    // The challenge-platform entry is the ORCHESTRATE path, not the bare
    // directory. Both live under `/cdn-cgi/challenge-platform/`, but they mean
    // opposite things — measured live on 2026-08-18:
    //
    //   interstitial (rocketreach.co, glassdoor.com):
    //       /cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1
    //   ordinary Bot-Management response, INCLUDING a cleared page:
    //       /cdn-cgi/challenge-platform/scripts/jsd/main.js
    //
    // The `/h/` segment is what separates them: the challenge orchestrator is
    // always served from it, the telemetry loader never is.
    //
    // Matching the bare directory therefore fired on pages that had already been
    // solved, which is exactly how `cloak.rs`'s accept gate came to reject its
    // own successful solves: the sidecar returned the real page, this predicate
    // saw the telemetry loader, and the recovery arm reported "still
    // challenged". `crw_crawl::single::classify_block` had already dropped the
    // bare directory for the same reason (a 783k post-solve Glassdoor capture);
    // the two lists had drifted, and only this copy still carried it.
    const STRONG: [&str; 5] = [
        "cf-browser-verification",
        "cf-challenge-running",
        "challenge-platform/h/",
        "_cf_chl_opt", // substring of window._cf_chl_opt / __cf_chl_managed_tk__
        "__cf_chl_managed_tk__",
    ];
    if STRONG.iter().any(|m| strong_src.contains(m)) {
        return true;
    }

    // Weak markers can appear on legitimate Cloudflare-fronted pages, so they
    // keep the size guard: a large page is real content, not an interstitial.
    if html.len() > 80_000 {
        return false;
    }
    let lower = html.to_lowercase();

    // Weak markers: each can appear on legitimate Cloudflare-fronted pages.
    // "ray id:" + "cloudflare" co-occur on most CF-fronted error pages and
    // would false-positive a real page with a CF footer; require challenge-
    // specific phrasing instead.
    let weak = [
        "just a moment",
        "checking your browser",
        "attention required",
        "performance &amp; security by cloudflare",
        "performance & security by cloudflare",
    ];
    let weak_hits = weak.iter().filter(|m| lower.contains(*m)).count();
    weak_hits >= 2
}

/// Returns true when the `cf-mitigated` response header indicates the
/// request was challenged or blocked by Cloudflare. Independent of the
/// HTTP status code — Cloudflare may return 200 with this header set.
///
/// `header_value` is the raw header value (case-sensitive on the right
/// side; we lower-case here for safety).
pub fn is_cloudflare_mitigated_header(header_value: &str) -> bool {
    let lower = header_value.trim().to_ascii_lowercase();
    matches!(lower.as_str(), "challenge" | "block")
}

/// Returns true when the `x-amzn-waf-action` response header indicates AWS WAF
/// Bot Control challenged the request.
///
/// AWS serves a Challenge/CAPTCHA action as **HTTP 202 with a zero-length
/// body**, so no body-based detector can see it — `looks_like_generic_bot_wall`
/// and `antibot::classify` both have nothing to scan. Measured on the prod host:
/// ballotpedia.org, jwa.org and seattletimes.com all answer `202` +
/// `content-length: 0` + this header, and all three return a full page through
/// residential egress. Without this predicate the empty 202 is returned to the
/// caller as a successful scrape with no content.
///
/// Deliberately a SEPARATE predicate from [`is_cloudflare_mitigated_header`]
/// rather than a shared value list: AWS documents `challenge` and `captcha`
/// (a WAF Block action uses its own configured status and body instead, so
/// `block` is not a value of this header), while Cloudflare's `cf-mitigated`
/// uses `challenge` and `block`. One merged match arm would silently either
/// widen CF to `captcha` or drop `block` from it.
pub fn is_aws_waf_action_header(header_value: &str) -> bool {
    let lower = header_value.trim().to_ascii_lowercase();
    matches!(lower.as_str(), "challenge" | "captcha")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thin_static_page_does_not_warrant_browser() {
        // example.com: a genuinely small static doc, zero scripts. Thin, but a
        // browser render would reveal nothing — must NOT be treated as needing JS.
        let html = "<html><head><title>Example Domain</title></head><body>\
            <div><h1>Example Domain</h1><p>This domain is for use in illustrative \
            examples.</p></div></body></html>";
        assert!(looks_like_thin_html(html));
        assert!(!warrants_browser_retry(html));
    }

    #[test]
    fn thin_shell_with_script_bundle_warrants_browser() {
        // Unrecognized-shell case (espn/seattletimes bucket): thin body but ships
        // a JS bundle → still escalates.
        let html =
            r#"<html><body><div id="app"></div><script src="/bundle.js"></script></body></html>"#;
        assert!(looks_like_thin_html(html));
        assert!(warrants_browser_retry(html));
    }

    #[test]
    fn inline_executable_script_warrants_browser() {
        let html =
            r#"<html><body><div></div><script>window.__DATA__={};main();</script></body></html>"#;
        assert!(warrants_browser_retry(html));
    }

    #[test]
    fn json_ld_only_does_not_warrant_browser() {
        // Structured-data blocks don't execute — a static page carrying only
        // JSON-LD must not be pushed to a browser.
        let html = r#"<html><body><p>hi</p><script type="application/ld+json">{"@type":"Thing"}</script></body></html>"#;
        assert!(!warrants_browser_retry(html));
    }

    #[test]
    fn thin_meta_refresh_redirect_warrants_browser() {
        // A thin stub whose only mechanism is a client-side meta-refresh redirect
        // (no script). HTTP clients don't follow it; a browser would, then reach
        // real content — so escalate.
        let html = r#"<html><head><meta http-equiv="refresh" content="0; url=https://example.org/real"></head><body>Redirecting...</body></html>"#;
        assert!(looks_like_thin_html(html));
        assert!(warrants_browser_retry(html));
    }

    #[test]
    fn detects_spa_shell() {
        let html = r#"<html><head></head><body><div id="root"></div><script src="/app.js"></script></body></html>"#;
        assert!(needs_js_rendering(html));
    }

    #[test]
    fn static_page_no_js_needed() {
        let html = r#"<html><body><article><h1>Hello World</h1><p>This is a long article with plenty of text content to read and enjoy. It has multiple paragraphs and lots of useful information.</p></article></body></html>"#;
        assert!(!needs_js_rendering(html));
    }

    #[test]
    fn detects_loading_placeholder_text() {
        let html =
            r#"<html><body><div><p>Loading...</p><p>Hi! Ask me anything.</p></div></body></html>"#;
        assert!(looks_like_loading_placeholder(html));
    }

    #[test]
    fn detects_spinner_only_body() {
        let html = r#"<html><body><div class="spinner"></div></body></html>"#;
        assert!(looks_like_loading_placeholder(html));
    }

    #[test]
    fn real_content_not_placeholder() {
        let html = r#"<html><body><article><h1>Welcome to my creative space</h1><p>Waqar Bin Abrar is a full stack developer specializing in MERN stack and Flutter apps, building scalable digital solutions for clients worldwide.</p><p>With years of experience delivering production applications, he combines technical expertise with design sensibility.</p></article></body></html>"#;
        assert!(!looks_like_loading_placeholder(html));
    }

    #[test]
    fn logo_alt_loading_on_real_page_not_placeholder() {
        // Regression: "Loading..." inside an img alt attribute must NOT trigger
        // placeholder detection when the page has real visible content.
        let html = r#"<html><body>
            <header><img alt="Loading..." src="/logo.png"/></header>
            <article>
                <h1>Software Engineering Blog</h1>
                <p>Thoughts on distributed systems, programming languages, and the craft of writing software that lasts. New posts weekly.</p>
                <p>This site covers topics from Rust ownership to Kubernetes operators.</p>
            </article>
        </body></html>"#;
        assert!(!looks_like_loading_placeholder(html));
    }

    #[test]
    fn empty_body_is_placeholder() {
        let html = r#"<html><body></body></html>"#;
        assert!(looks_like_loading_placeholder(html));
    }

    #[test]
    fn large_page_never_placeholder() {
        let filler = "x".repeat(100_000);
        let html = format!("<html><body><p>Loading...</p>{filler}</body></html>");
        assert!(!looks_like_loading_placeholder(&html));
    }

    #[test]
    fn detects_nextjs_app_router_error_boundary() {
        let html = r#"<html><body><div id="__next-error-0"><h2>Application error: a client-side exception has occurred.</h2></div></body></html>"#;
        assert_eq!(
            looks_like_failed_render(html),
            Some(FailedRenderReason::NextJsClientError)
        );
    }

    #[test]
    fn detects_nextjs_pages_router_error() {
        let html = r#"<html><body><div id="__next_error__">oops</div></body></html>"#;
        assert_eq!(
            looks_like_failed_render(html),
            Some(FailedRenderReason::NextJsClientError)
        );
    }

    #[test]
    fn detects_react_minified_error() {
        let html = r#"<html><body><a href="https://react.dev/errors/418">Minified React error #418</a></body></html>"#;
        assert_eq!(
            looks_like_failed_render(html),
            Some(FailedRenderReason::ReactMinifiedError)
        );
    }

    #[test]
    fn detects_legacy_react_error_decoder_url() {
        let html = r#"<html><body><a href="https://reactjs.org/docs/error-decoder.html?invariant=31">React</a></body></html>"#;
        assert_eq!(
            looks_like_failed_render(html),
            Some(FailedRenderReason::ReactMinifiedError)
        );
    }

    #[test]
    fn blog_post_about_error_is_not_failed_render() {
        // Regression: a blog post that *describes* the Next.js error must NOT
        // be flagged as a failed render. The string appears only in body text
        // (and not in a __next-error- element id), so the detector must let
        // it through.
        let html = r#"<html><body><article><h1>Debugging Next.js</h1>
            <p>When you see "Application error: a client-side exception has occurred",
            it usually means a hydration mismatch.</p>
            <pre><code>console.log('debug')</code></pre>
        </article></body></html>"#;
        assert!(looks_like_failed_render(html).is_none());
    }

    #[test]
    fn healthy_page_is_not_failed_render() {
        let html =
            r#"<html><body><main><h1>Hello</h1><p>Real content here.</p></main></body></html>"#;
        assert!(looks_like_failed_render(html).is_none());
    }

    #[test]
    fn huge_page_is_not_scanned() {
        // Pages over 200KB are exempt. Even with a marker that would normally
        // trigger, the function must short-circuit to None.
        let mut html = String::from(r#"<html><body><div id="__next-error-0"></div>"#);
        html.push_str(&"<p>filler</p>".repeat(20_000));
        html.push_str("</body></html>");
        assert!(html.len() > 200_000);
        assert!(looks_like_failed_render(&html).is_none());
    }

    #[test]
    fn cf_strong_marker_detected() {
        let html =
            r#"<html><body><div id="cf-browser-verification">Just a moment...</div></body></html>"#;
        assert!(looks_like_cloudflare_challenge(html));
    }

    #[test]
    fn cf_managed_token_detected() {
        let html = r#"<html><body><script>window._cf_chl_opt={cvId:'2'};</script></body></html>"#;
        assert!(looks_like_cloudflare_challenge(html));
    }

    #[test]
    fn cf_strong_marker_detected_on_large_page() {
        // Modern Cloudflare managed challenge: a large (>80KB) HTML whose only
        // machine marker is a challenge-platform <script src> deep in the body
        // (Glassdoor served a 275KB page with the marker at byte ~128k). The old
        // 80KB size cap made this evade detection.
        let mut html = String::from("<html><head><title>Just a moment...</title></head><body>");
        html.push_str(&"<p>verifying you are human, one moment please.</p>".repeat(3_000));
        html.push_str(
            r#"<script src="/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1?ray=abc"></script>"#,
        );
        html.push_str("</body></html>");
        assert!(
            html.len() > 80_000,
            "fixture must exceed the weak-marker cap"
        );
        assert!(looks_like_cloudflare_challenge(&html));
    }

    #[test]
    fn cf_large_real_page_with_footer_mention_not_flagged() {
        // A large real page that merely mentions Cloudflare in a footer (no
        // challenge markers) must NOT be flagged — the weak-marker path stays
        // size-guarded.
        let mut html = String::from("<html><body><article>");
        html.push_str(&"<p>Real article content about web performance.</p>".repeat(3_000));
        html.push_str(
            "<footer>Hosted via Cloudflare. Ray ID: abc123</footer></article></body></html>",
        );
        assert!(html.len() > 80_000);
        assert!(!looks_like_cloudflare_challenge(&html));
    }

    #[test]
    fn cf_single_weak_marker_not_enough() {
        // A page that just mentions "Cloudflare" should not trigger.
        let html = r#"<html><body><article><h1>Why we use Cloudflare</h1><p>Performance benefits.</p></article></body></html>"#;
        assert!(!looks_like_cloudflare_challenge(html));
    }

    #[test]
    fn cf_two_weak_markers_trigger() {
        // Two challenge-specific phrases must co-occur. "ray id:" was
        // removed from the weak set because legitimate CF-fronted error
        // pages also include both "ray id" and "cloudflare".
        let html =
            r#"<html><body><h1>Just a moment...</h1><p>Checking your browser...</p></body></html>"#;
        assert!(looks_like_cloudflare_challenge(html));
    }

    #[test]
    fn cf_ray_id_alone_does_not_trigger() {
        // Pre-fix this would false-positive: "ray id" + "cloudflare" both
        // appear on benign CF-fronted pages.
        let html = r#"<html><body><h1>About</h1><p>Hosted via Cloudflare.</p><footer>Ray ID: abc123</footer></body></html>"#;
        assert!(!looks_like_cloudflare_challenge(html));
    }

    #[test]
    fn cf_mitigated_header_challenge() {
        assert!(is_cloudflare_mitigated_header("challenge"));
        assert!(is_cloudflare_mitigated_header(" CHALLENGE "));
        assert!(is_cloudflare_mitigated_header("block"));
    }

    #[test]
    fn cf_mitigated_header_other_values() {
        assert!(!is_cloudflare_mitigated_header(""));
        assert!(!is_cloudflare_mitigated_header("ok"));
        assert!(!is_cloudflare_mitigated_header("verified"));
    }

    #[test]
    fn cf_strong_marker_beyond_scan_limit_not_flagged() {
        // Perf bound: strong markers are scanned only within the first 512KB.
        // A marker past that (pathologically large page) is not scanned — real
        // CF challenge markers sit well within the first few hundred KB.
        let mut html = String::from("<html><body>");
        html.push_str(&"<p>x</p>".repeat(80_000)); // ~640KB of filler > 512KB
        html.push_str(r#"<div id="cf-browser-verification"></div></body></html>"#);
        assert!(html.len() > 512 * 1024);
        assert!(!looks_like_cloudflare_challenge(&html));
    }

    #[test]
    fn cloudfront_403_block_page_is_bot_wall() {
        // Real-world CloudFront geo-block page (americastire.com from EU egress).
        // Two strong markers: title and footer attribution. Engine must escalate
        // through the renderer chain (e.g. to chrome_proxy) instead of returning
        // this 403 shell as success.
        let html = r#"<html><head><title>ERROR: The request could not be satisfied</title></head>
            <body><h1>403 ERROR</h1>
            <h3>The request could not be satisfied.</h3>
            <p>The Amazon CloudFront distribution is configured to block access from your country.</p>
            <hr><i>Generated by cloudfront (CloudFront)</i></body></html>"#;
        assert!(looks_like_generic_bot_wall(html, false));
    }

    #[test]
    fn generic_403_with_block_phrasing_is_bot_wall() {
        // Origin-side / WAF block page that uses "configured to block access"
        // without naming a vendor — still a clear block signal.
        let html = r#"<html><body><h1>403</h1>
            <p>Our firewall is configured to block access from this region.</p></body></html>"#;
        assert!(looks_like_generic_bot_wall(html, false));
    }

    /// The Wikimedia Varnish error shell, reproducing the real structure: an
    /// HTTP-200 static page with NO <body>/<head> tags, one inline <style>, and
    /// the content in <div>s directly under <html> (source: Wikimedia
    /// operations/puppet error templates).
    fn wikimedia_block_html() -> &'static str {
        r#"<!DOCTYPE html>
<html lang="en">
<meta charset="utf-8">
<title>Wikimedia Error</title>
<style>body{font-family:sans-serif}</style>
<meta name="color-scheme" content="light dark">
<div class="content" role="main">
<h1>Error</h1>
<p>Contabo networks are forbidden due to abuse. Contact noc@wikimedia.org for assistance.</p>
</div>
<div class="footer">
<p>If you report this error to the Wikimedia System Administrators, please include the details below.</p>
<p class="text-muted"><code>Request served via cp6016, Varnish XID 12345<br>Error: 403, Contabo networks are forbidden due to abuse.<br><details><summary>Sensitive client information</summary>IP address: 207.180.230.151</details></code></p>
</div>
</html>"#
    }

    #[test]
    fn wikimedia_http200_block_shell_is_bot_wall() {
        // Regression for the silent-success bug: the Wikimedia error page is
        // HTTP-200, scriptless, and crucially has NO <body> tag. Body-only text
        // extraction returned "" here, so the phrase list never matched. The
        // no-<body> fallback must let the canonical footer phrase trip.
        assert!(looks_like_generic_bot_wall(wikimedia_block_html(), false));
    }

    /// The Amazon.it TLS-fingerprint interstitial served to non-browser TLS
    /// stacks (verified live 2026-09): HTTP 200, a small body whose visible
    /// text is essentially the one sentence. Only the FULL sentence is on the
    /// phrase list; a loose "continue shopping" must NOT match on its own.
    #[test]
    fn generic_bot_wall_catches_amazon_interstitial_it() {
        let html = r#"<html><body>
            <p>Per continuare a fare acquisti sul nostro sito, fai clic sul pulsante qui sotto per continuare a fare acquisti.</p>
            </body></html>"#;
        assert!(looks_like_generic_bot_wall(html, false));
    }

    #[test]
    fn english_retail_continue_shopping_copy_is_not_a_wall() {
        // "Click the button below to continue shopping" is ordinary retail
        // copy (empty-cart / order-confirmation pages), not an interstitial
        // marker: only the verified it-IT sentence is on the list, so this
        // shop page must stay clean even under the 600-char visible-text cap.
        let html = r#"<html><body>
            <p>Your cart is empty.</p>
            <p>Click the button below to continue shopping.</p>
            </body></html>"#;
        assert!(!looks_like_generic_bot_wall(html, false));
    }

    #[test]
    fn amazon_interstitial_loose_phrase_alone_is_not_a_wall() {
        // Only the full sentences are listed, never the loose fragment, so a
        // page whose visible text merely contains "continue shopping" in
        // passing stays clean.
        let html =
            r#"<html><body><p>Happy days: continue shopping with us soon!</p></body></html>"#;
        assert!(!looks_like_generic_bot_wall(html, false));
    }

    #[test]
    fn real_article_quoting_amazon_sentence_is_not_bot_wall() {
        // A genuine article that QUOTES the full interstitial sentence stays
        // clean once its visible text exceeds the 600-char cap: the same
        // safeguard that already protects CloudFront/Wikimedia quotations.
        let html = format!(
            "<html><body><article><h1>Why Amazon shows interstitials</h1>{}</article></body></html>",
            "<p>Shoppers behind datacenter IP ranges increasingly report the \
             message \"fai clic sul pulsante qui sotto per continuare a fare \
             acquisti\" when opening product pages, which analysts attribute to \
             TLS fingerprinting rather than IP reputation alone.</p>"
                .repeat(6)
        );
        assert!(!looks_like_generic_bot_wall(&html, false));
    }

    #[test]
    fn json_api_error_without_body_is_not_bot_wall() {
        // Regression: a directly-scraped JSON/plain-text error response has no
        // <body> AND no <html> marker, so the no-<body> fallback must not scan
        // it — otherwise the existing "access denied" phrase would wrongly flag
        // a legitimate small API payload as a block.
        let json = r#"{"error":"access_denied","message":"Access Denied: insufficient permissions for this resource"}"#;
        assert!(!looks_like_generic_bot_wall(json, false));
        let xml = r#"<?xml version="1.0"?><Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>"#;
        assert!(!looks_like_generic_bot_wall(xml, false));
    }

    #[test]
    fn real_wikipedia_article_is_not_bot_wall() {
        // A real article (>600 visible chars, normal <body>) must NOT trip.
        let html = format!(
            "<html><body><article><h1>Radcliffe College</h1>{}</article></body></html>",
            "<p>Radcliffe College was a women's liberal arts college in Cambridge, \
             Massachusetts, and functioned as the female coordinate institution for \
             the all-male Harvard College.</p>"
                .repeat(6)
        );
        assert!(!looks_like_generic_bot_wall(&html, false));
    }

    #[test]
    fn legitimate_blog_about_cloudfront_is_not_bot_wall() {
        // Regression guard: a long article about CloudFront must NOT trip the
        // bot-wall heuristic. visible body text length cap (600 chars) is the
        // existing safeguard — exceed it here to assert it still applies.
        let mut html = String::from(r#"<html><body><article>"#);
        html.push_str("<p>An article about CloudFront and how distributions are configured to block access by country. </p>".repeat(20).as_str());
        html.push_str("</article></body></html>");
        assert!(!looks_like_generic_bot_wall(&html, false));
    }

    #[test]
    fn vendor_cloudflare_challenge_form_detected() {
        let html = r#"<html><body><form class="challenge-form" action="/?__cf_chl_f_tk=abc123">
            </form></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("cloudflare"));
    }

    #[test]
    fn vendor_cloudflare_error_code_detected() {
        let html = r#"<html><body><span class="cf-error-code">1020</span></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("cloudflare"));
    }

    #[test]
    fn vendor_cloudflare_challenge_platform_detected() {
        let html = r#"<html><head><script src="/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1?ray=abc"></script></head></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("cloudflare"));
    }

    #[test]
    fn vendor_akamai_reference_id_detected() {
        let html = r#"<html><body><p>Access Denied</p>
            <p>Reference #18.2d351ab8.1557333295.a4e16ab</p></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("akamai"));
    }

    #[test]
    fn vendor_akamai_pardon_our_interruption_detected() {
        let html = r#"<html><body><h1>Pardon Our Interruption</h1>
            <p>As you were browsing, something about your browser made us think you were a bot.</p>
            </body></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("akamai"));
    }

    #[test]
    fn vendor_perimeterx_pxappid_detected() {
        let html = r#"<html><head><script>window._pxAppId = 'PXabc123';</script></head></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("perimeterx"));
    }

    #[test]
    fn vendor_datadome_captcha_domain_detected() {
        let html = r#"<html><body><iframe src="https://geo.captcha-delivery.com/captcha/?initialCid=xyz"></iframe></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("datadome"));
    }

    #[test]
    fn vendor_imperva_incapsula_resource_detected() {
        let html = r#"<html><body><script src="/_Incapsula_Resource?SWJIYLWA=blah"></script></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("imperva"));
    }

    #[test]
    fn vendor_sucuri_firewall_brand_detected() {
        let html = r#"<html><body><h1>Sucuri WebSite Firewall - Access Denied</h1></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("sucuri"));
    }

    #[test]
    fn vendor_cloudfront_block_detected() {
        let html = r#"<html><head><title>ERROR: The request could not be satisfied</title></head>
            <body><h1>403 ERROR</h1>
            <hr><i>Generated by cloudfront (CloudFront)</i></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("cloudfront"));
    }

    #[test]
    fn vendor_legit_blog_about_cloudflare_is_none() {
        // A 50KB legit page mentioning Cloudflare but with no challenge markers
        // must NOT be flagged as a vendor block.
        let mut html = String::from("<html><body><article><h1>Why we picked Cloudflare</h1>");
        html.push_str(
            &"<p>Cloudflare gives us DDoS protection and a global anycast network.</p>".repeat(400),
        );
        html.push_str("</article></body></html>");
        assert!(html.len() > 15_000);
        assert!(looks_like_vendor_block(&html).is_none());
    }

    #[test]
    fn vendor_block_oversized_page_returns_none() {
        let big = "x".repeat(300_000);
        assert!(looks_like_vendor_block(&big).is_none());
    }

    #[test]
    fn vendor_block_clean_page_returns_none() {
        let html = r#"<html><body><main><h1>Hello</h1><p>Real content.</p></main></body></html>"#;
        assert!(looks_like_vendor_block(html).is_none());
    }

    #[test]
    fn spinner_class_in_script_body_ignored() {
        // class="spinner" inside a <script> block must not trigger spinner detection,
        // since scripts are stripped before text-length measurement.
        let html = r#"<html><body><article><h1>Real Article</h1><p>This is a real article with substantial content about the topic at hand, providing useful information.</p><script>const x = 'class="spinner"';</script></article></body></html>"#;
        assert!(!looks_like_loading_placeholder(html));
    }

    /// Pad `head` with ASCII so the multibyte `c` straddles byte `cap`, landing
    /// `offset` bytes into it — a mid-char index a naive `&html[..cap]` panics on.
    /// `tail` closes the page past the cap.
    fn page_with_char_straddling(
        head: &str,
        cap: usize,
        tail: &str,
        c: char,
        offset: usize,
    ) -> String {
        let mut html = String::with_capacity(cap + tail.len() + 8);
        html.push_str(head);
        html.push_str(&"a".repeat(cap - offset - head.len()));
        html.push(c);
        html.push_str(tail);
        assert!(!html.is_char_boundary(cap), "byte {cap} must be mid-char");
        assert!(html.len() > cap, "page must exceed the cap");
        html
    }

    /// Every fixed-cap prefix scan in this module, paired with a page that
    /// straddles that cap with a multibyte char and the verdict a correct scan
    /// must still reach. Slicing mid-char panics outright; a clamp that walks
    /// back too far scans the wrong window and flips the verdict instead. Each
    /// verdict is false on an empty window, so none of these pass vacuously.
    #[test]
    fn multibyte_char_straddling_a_scan_cap_keeps_the_verdict() {
        type Verdict = fn(&str) -> bool;
        let cases: [(&str, usize, &str, Verdict); 3] = [
            // 500KB SPA/thin scan: an SPA marker for `needs_js_rendering`, and an
            // inline script for `warrants_browser_retry` (drop it and that arm
            // goes false). The body reads as empty because the cap truncates ahead
            // of `</body>`, not because the script is stripped — the padding is
            // what pushes the close tag out of the scanned window.
            (
                r#"<html><body><div id="root"></div><script>x="#,
                500_000,
                "\"</script></body></html>",
                |h| needs_js_rendering(h) && looks_like_thin_html(h) && warrants_browser_retry(h),
            ),
            // 15KB vendor-block head scan, kept under the 200KB vendor ceiling.
            (
                r#"<html><head><script src="/cdn-cgi/challenge-platform/h/x"></script>"#,
                15_000,
                "</head></html>",
                |h| looks_like_vendor_block(h) == Some("cloudflare"),
            ),
            // 512KB strong-marker scan: a managed-challenge page big enough to
            // reach the cap, well past the 80KB weak-marker guard.
            (
                r#"<html><body><script src="/cdn-cgi/challenge-platform/h/b/orchestrate/j"></script>"#,
                512 * 1024,
                "</body></html>",
                looks_like_cloudflare_challenge,
            ),
        ];

        for (head, cap, tail, verdict) in cases {
            // 'é' can only ever be split one byte in; the 4-byte emoji is the
            // widest char and straddles at three distinct interior offsets, so a
            // clamp that clears a single byte still splits it.
            for c in ['é', '\u{1F600}'] {
                for offset in 1..c.len_utf8() {
                    let html = page_with_char_straddling(head, cap, tail, c, offset);
                    assert!(
                        verdict(&html),
                        "cap {cap}, char {c:?}, straddle offset {offset}"
                    );
                }
            }
        }
    }

    // ── needs_js_rendering: SPA indicator coverage ──────────────────────

    /// Generates one `#[test]` per listed indicator, each asserting that a
    /// short body containing ONLY that indicator triggers `needs_js_rendering`.
    macro_rules! spa_indicator_tests {
        ($($name:ident => $indicator:expr),+ $(,)?) => {
            $(
                #[test]
                fn $name() {
                    let html = format!("<html><body>x{}x</body></html>", $indicator);
                    assert!(
                        needs_js_rendering(&html),
                        "SPA indicator {:?} should trigger JS rendering on a short body",
                        $indicator
                    );
                }
            )+
        };
    }

    spa_indicator_tests! {
        spa_indicator_id_root => "id=\"root\"",
        spa_indicator_id_app => "id=\"app\"",
        spa_indicator_id_next => "id=\"__next\"",
        spa_indicator_id_nuxt => "id=\"__nuxt\"",
        spa_indicator_id_gatsby => "id=\"__gatsby\"",
        spa_indicator_id_svelte => "id=\"svelte\"",
        spa_indicator_ng_app => "ng-app",
        spa_indicator_data_reactroot => "data-reactroot",
        spa_indicator_data_reactid => "data-reactid",
        spa_indicator_data_remix_run => "data-remix-run",
        spa_indicator_data_sveltekit => "data-sveltekit",
        spa_indicator_data_astro => "data-astro-",
        spa_indicator_script_src => "<script src",
        spa_indicator_window_initial_state => "window.__initial_state__",
        spa_indicator_next_data => "__next_data__",
        spa_indicator_nuxt_double_underscore => "__nuxt__",
        spa_indicator_sveltekit_data => "__sveltekit_data",
        spa_indicator_window_remixcontext => "window.__remixcontext",
        spa_indicator_window_astro => "window.__astro",
        spa_indicator_gatsby_focus_wrapper => "gatsby-focus-wrapper",
    }

    macro_rules! builder_indicator_tests {
        ($($name:ident => $indicator:expr),+ $(,)?) => {
            $(
                #[test]
                fn $name() {
                    // 300 non-whitespace filler chars keeps body_len in the
                    // [200, 500) window so the SPA-indicator branch (< 200) is
                    // skipped and the site-builder branch (< 500) is the one
                    // that actually decides the verdict.
                    let html = format!(
                        "<html><body>{}{}</body></html>",
                        "z".repeat(300),
                        $indicator
                    );
                    assert!(
                        needs_js_rendering(&html),
                        "builder indicator {:?} should trigger JS rendering",
                        $indicator
                    );
                }
            )+
        };
    }

    builder_indicator_tests! {
        builder_indicator_framer => "framerusercontent.com",
        builder_indicator_webflow => "webflow.io",
        builder_indicator_wix => "wixsite.com",
        builder_indicator_squarespace => "squarespace.com/universal",
    }

    macro_rules! storybook_indicator_tests {
        ($($name:ident => $indicator:expr),+ $(,)?) => {
            $(
                #[test]
                fn $name() {
                    // 600 non-whitespace filler chars keeps body_len in the
                    // [500, 1000) window: past the builder branch, still inside
                    // the bundler/storybook branch.
                    let html = format!(
                        "<html><body>{}{}</body></html>",
                        "z".repeat(600),
                        $indicator
                    );
                    assert!(
                        needs_js_rendering(&html),
                        "storybook indicator {:?} should trigger JS rendering",
                        $indicator
                    );
                }
            )+
        };
    }

    storybook_indicator_tests! {
        storybook_indicator_root => "id=\"storybook-root\"",
        storybook_indicator_docs => "id=\"storybook-docs\"",
        storybook_indicator_dunder => "__storybook",
        storybook_indicator_path_docs => "?path=/docs/",
        storybook_indicator_iframe => "/iframe.html",
    }

    #[test]
    fn noscript_with_enable_javascript_triggers() {
        let html = r#"<html><body><article>Real looking body text that is long enough to not be considered thin by any other heuristic in this module, well past the two hundred character floor used elsewhere.</article><noscript>Please enable JavaScript to view this site.</noscript></body></html>"#;
        assert!(needs_js_rendering(html));
    }

    #[test]
    fn noscript_without_enable_javascript_phrase_does_not_trigger() {
        let html = r#"<html><body><article>Real looking body text that is long enough to not be considered thin by any other heuristic in this module, well past the two hundred character floor used elsewhere.</article><noscript>Sorry, this feature requires cookies.</noscript></body></html>"#;
        assert!(!needs_js_rendering(html));
    }

    #[test]
    fn enable_javascript_phrase_outside_noscript_does_not_trigger() {
        // The check requires BOTH "<noscript>" and "enable javascript" to be
        // present; the phrase alone, outside any noscript tag, must not fire.
        let html = r#"<html><body><article>Please enable JavaScript in your browser settings to use every feature of this otherwise fully-rendered static article about browser configuration.</article></body></html>"#;
        assert!(!needs_js_rendering(html));
    }

    #[test]
    fn script_count_four_below_threshold_not_detected() {
        let mut html = String::from("<html><body>short filler text here");
        for _ in 0..4 {
            html.push_str("<script></script>");
        }
        html.push_str("</body></html>");
        assert!(!needs_js_rendering(&html));
    }

    #[test]
    fn script_count_five_meets_threshold_detected() {
        let mut html = String::from("<html><body>short filler text here");
        for _ in 0..5 {
            html.push_str("<script></script>");
        }
        html.push_str("</body></html>");
        assert!(needs_js_rendering(&html));
    }

    #[test]
    fn spa_body_len_boundary_199_triggers() {
        // 193 filler chars + "ng-app" (6 chars) = 199 non-whitespace body chars.
        let html = format!("<html><body>{}ng-app</body></html>", "z".repeat(193));
        assert!(needs_js_rendering(&html));
    }

    #[test]
    fn spa_body_len_boundary_200_does_not_trigger() {
        // 194 filler chars + "ng-app" (6 chars) = 200: the `< 200` check is
        // strict, so at exactly 200 the SPA branch must NOT fire, and none of
        // the other indicator lists recognize "ng-app" either.
        let html = format!("<html><body>{}ng-app</body></html>", "z".repeat(194));
        assert!(!needs_js_rendering(&html));
    }

    #[test]
    fn builder_body_len_boundary_499_triggers() {
        // 488 filler chars + "wixsite.com" (11 chars) = 499 < 500.
        let html = format!("<html><body>{}wixsite.com</body></html>", "z".repeat(488));
        assert!(needs_js_rendering(&html));
    }

    #[test]
    fn builder_body_len_boundary_500_does_not_trigger() {
        // 489 filler chars + "wixsite.com" (11 chars) = 500, not < 500.
        let html = format!("<html><body>{}wixsite.com</body></html>", "z".repeat(489));
        assert!(!needs_js_rendering(&html));
    }

    #[test]
    fn bundler_body_len_boundary_999_triggers() {
        let mut html = String::from("<html><body>");
        html.push_str(&"z".repeat(999));
        for _ in 0..5 {
            html.push_str("<script></script>");
        }
        html.push_str("</body></html>");
        assert!(needs_js_rendering(&html));
    }

    #[test]
    fn bundler_body_len_boundary_1000_does_not_trigger() {
        let mut html = String::from("<html><body>");
        html.push_str(&"z".repeat(1000));
        for _ in 0..5 {
            html.push_str("<script></script>");
        }
        html.push_str("</body></html>");
        assert!(!needs_js_rendering(&html));
    }

    #[test]
    fn unicode_filler_counts_chars_not_bytes_toward_spa_threshold() {
        // Each emoji is 4 bytes but 1 char. 150 emoji + "id=\"root\"" (9 chars)
        // = 159 non-whitespace chars, comfortably under 200.
        let html = format!(
            "<html><body>{}id=\"root\"</body></html>",
            "\u{1F600}".repeat(150)
        );
        assert!(needs_js_rendering(&html));
    }

    #[test]
    fn unicode_filler_past_threshold_does_not_trigger_spa_branch() {
        // 250 emoji chars alone already exceed 200 non-whitespace chars, so the
        // SPA branch is skipped even though "id=\"root\"" is present.
        let html = format!(
            "<html><body>{}id=\"root\"</body></html>",
            "\u{1F600}".repeat(250)
        );
        assert!(!needs_js_rendering(&html));
    }

    #[test]
    fn spa_indicator_beyond_500kb_scan_window_not_detected() {
        // The scan window is capped at 500KB; an indicator placed well past it
        // must not be seen, so the function falls through to "not needed".
        let mut html = String::from("<html><body>");
        html.push_str(&"z".repeat(600_000));
        html.push_str("id=\"root\"</body></html>");
        assert!(html.len() > 500_000);
        assert!(!needs_js_rendering(&html));
    }

    #[test]
    fn spa_indicator_within_500kb_scan_window_detected() {
        // A large <head> (documented at the top of needs_js_rendering: some
        // pages ship huge preloaded-data heads) sits before a short body
        // carrying the real SPA marker. The marker must still be found even
        // though a lot of unrelated markup precedes it, as long as the whole
        // document stays comfortably under the 500KB scan cap.
        let mut html = String::from("<html><head>");
        html.push_str(&"<!-- padding -->".repeat(20_000));
        html.push_str(
            r#"</head><body><div id="root"></div><script src="/app.js"></script></body></html>"#,
        );
        assert!(html.len() < 500_000);
        assert!(needs_js_rendering(&html));
    }

    #[test]
    fn needs_js_rendering_empty_string_is_false() {
        assert!(!needs_js_rendering(""));
    }

    #[test]
    fn needs_js_rendering_whitespace_only_is_false() {
        assert!(!needs_js_rendering("   \n\t  "));
    }

    #[test]
    fn needs_js_rendering_no_body_tag_defaults_to_has_content() {
        // No `<body>` at all: `extract_body_text_len` falls back to 1000, which
        // is >= 200/500/1000, so none of the size-gated indicator branches run
        // even though "id=\"root\"" is present in the fragment.
        let html = r#"<div id="root"></div>"#;
        assert!(!needs_js_rendering(html));
    }

    #[test]
    fn needs_js_rendering_case_insensitive_indicator_match() {
        let html = format!("<html><body>x{}x</body></html>", "ID=\"ROOT\"");
        assert!(needs_js_rendering(&html));
    }

    #[test]
    fn needs_js_rendering_malformed_truncated_mid_tag_does_not_panic() {
        let html = r#"<html><body><div id="ro"#;
        let _ = needs_js_rendering(html);
    }

    #[test]
    fn needs_js_rendering_deeply_nested_html_static_article_not_detected() {
        let mut html = String::from("<html><body>");
        for _ in 0..200 {
            html.push_str("<div>");
        }
        html.push_str("Plenty of genuine article text goes here so this deeply nested static page reads as real content rather than an empty SPA shell.");
        for _ in 0..200 {
            html.push_str("</div>");
        }
        html.push_str("</body></html>");
        assert!(!needs_js_rendering(&html));
    }

    // ── looks_like_generic_bot_wall: phrase coverage ────────────────────

    macro_rules! bot_wall_phrase_tests {
        ($($name:ident => $phrase:expr),+ $(,)?) => {
            $(
                #[test]
                fn $name() {
                    let html = format!("<html><body><p>{}</p></body></html>", $phrase);
                    assert!(
                        looks_like_generic_bot_wall(&html, false),
                        "phrase {:?} should be recognized as a bot wall",
                        $phrase
                    );
                }
            )+
        };
    }

    bot_wall_phrase_tests! {
        bot_wall_phrase_performing_security_verification => "Performing security verification",
        bot_wall_phrase_verify_you_are_human => "Verify you are human",
        bot_wall_phrase_checking_your_browser => "Checking your browser",
        bot_wall_phrase_enable_js_and_cookies => "Please enable JavaScript and cookies to continue",
        bot_wall_phrase_security_check => "Security check in progress",
        bot_wall_phrase_access_denied => "Access Denied",
        bot_wall_phrase_request_blocked => "Request Blocked",
        bot_wall_phrase_cloudfront_could_not_satisfy => "The request could not be satisfied",
        bot_wall_phrase_generated_by_cloudfront => "Generated by cloudfront",
        bot_wall_phrase_configured_to_block_access => "This distribution is configured to block access",
        bot_wall_phrase_wikimedia_footer => "If you report this error to the Wikimedia System Administrators",
    }

    #[test]
    fn bot_wall_visible_text_boundary_600_is_bot_wall() {
        // Exactly 600 non-whitespace visible chars: the `> 600` check does not
        // trip, so the phrase is still eligible to match — the boundary is
        // inclusive (still scanned, still matches).
        let phrase = "access denied";
        let non_ws = phrase.chars().filter(|c| !c.is_whitespace()).count();
        let pad = "z".repeat(600 - non_ws);
        let html = format!("<html><body><p>{pad}{phrase}</p></body></html>");
        assert!(looks_like_generic_bot_wall(&html, false));
    }

    #[test]
    fn bot_wall_visible_text_boundary_601_is_not_bot_wall() {
        // 601 non-whitespace visible chars pushes past the cap: even though the
        // phrase is present, the page is treated as real content.
        let phrase = "access denied";
        let non_ws = phrase.chars().filter(|c| !c.is_whitespace()).count();
        let pad = "z".repeat(601 - non_ws);
        let html = format!("<html><body><p>{pad}{phrase}</p></body></html>");
        assert!(!looks_like_generic_bot_wall(&html, false));
    }

    #[test]
    fn bot_wall_size_cap_just_under_80kb_still_scanned() {
        let mut html = String::from("<html><body><p>access denied</p><!--");
        html.push_str(&"z".repeat(79_000));
        html.push_str("--></body></html>");
        assert!(html.len() <= 80_000);
        assert!(looks_like_generic_bot_wall(&html, false));
    }

    #[test]
    fn bot_wall_size_cap_over_80kb_not_scanned() {
        let mut html = String::from("<html><body><p>access denied</p><!--");
        html.push_str(&"z".repeat(90_000));
        html.push_str("--></body></html>");
        assert!(html.len() > 80_000);
        assert!(!looks_like_generic_bot_wall(&html, false));
    }

    #[test]
    fn bot_wall_no_body_but_html_tag_marker_scanned() {
        // No `<body>` tag, but an `<html` marker is present, so the fallback
        // path (strip script/style from the whole doc) should still scan it.
        let html = r#"<html lang="en"><title>Blocked</title><div>Access Denied</div></html>"#;
        assert!(looks_like_generic_bot_wall(html, false));
    }

    #[test]
    fn bot_wall_plain_fragment_without_html_marker_not_scanned() {
        // Neither `<body` nor `<html`/`<!doctype html` present: bail out
        // entirely, even though the phrase is textually present.
        let text = "access denied: you do not have permission to view this resource";
        assert!(!looks_like_generic_bot_wall(text, false));
    }

    #[test]
    fn bot_wall_non_english_block_page_not_detected() {
        // Known gap: the phrase list is English-only, so a Turkish-language
        // block page using none of the English phrases is not recognized.
        // This documents the current (real) limitation rather than a panic.
        let html = r#"<html><body><p>Erisim engellendi. Guvenlik dogrulamasi devam ediyor, lutfen bekleyin.</p></body></html>"#;
        assert!(!looks_like_generic_bot_wall(html, false));
    }

    #[test]
    fn bot_wall_phrase_inside_script_not_counted_as_visible_text() {
        // "access denied" sitting inside a <script> block must not count toward
        // the visible-text scan; scripts are stripped before phrase matching.
        let html = r#"<html><body><script>var msg = "access denied";</script><article>A perfectly ordinary short article with no block phrasing anywhere in its visible text.</article></body></html>"#;
        assert!(!looks_like_generic_bot_wall(html, false));
    }

    #[test]
    fn bot_wall_multiple_phrases_still_single_verdict() {
        let html = r#"<html><body><p>Access Denied. Request Blocked. Please complete the security check.</p></body></html>"#;
        assert!(looks_like_generic_bot_wall(html, false));
    }

    #[test]
    fn bot_wall_malformed_truncated_html_does_not_panic() {
        let html = "<html><body><p>access den";
        let _ = looks_like_generic_bot_wall(html, false);
    }

    #[test]
    fn bot_wall_empty_string_is_false() {
        assert!(!looks_like_generic_bot_wall("", false));
    }

    // ── looks_like_vendor_block: additional vendor / marker coverage ───

    #[test]
    fn vendor_kasada_sdk_signature_detected() {
        let html = r#"<html><head><script>KPSDK.scriptStart = KPSDK.now();</script></head></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("kasada"));
    }

    #[test]
    fn vendor_imperva_incident_id_phrasing_detected() {
        let html = r#"<html><body><p>Request unsuccessful. Incapsula incident ID: 123-456</p></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("imperva"));
    }

    #[test]
    fn vendor_perimeterx_captcha_cdn_detected() {
        let html = r#"<html><body><img src="https://captcha.px-cdn.net/logo.png"></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("perimeterx"));
    }

    #[test]
    fn vendor_cloudfront_request_could_not_be_satisfied_alone_detected() {
        let html = r#"<html><body><h3>The request could not be satisfied.</h3></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("cloudfront"));
    }

    #[test]
    fn vendor_cloudflare_challenge_form_alone_without_token_not_matched() {
        // The `challenge-form` check requires BOTH the class AND the
        // `__cf_chl_f_tk=` token in the same head window — the class name
        // alone (e.g. reused by an unrelated form) must not match.
        let html =
            r#"<html><body><form class="challenge-form" action="/submit"></form></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), None);
    }

    #[test]
    fn vendor_cloudflare_cleared_page_telemetry_loader_is_still_flagged() {
        // BUG: looks_like_vendor_block's Cloudflare check matches ANY
        // `/cdn-cgi/challenge-platform/` substring, unlike the more precise
        // looks_like_cloudflare_challenge (see its STRONG list comment), which
        // deliberately requires the `/h/` orchestrate segment because the bare
        // `/scripts/jsd/` telemetry loader ships on ordinary, ALREADY-CLEARED
        // Bot-Management responses too. That fix was applied to
        // looks_like_cloudflare_challenge and to
        // crw_crawl::single::classify_block, but looks_like_vendor_block was
        // never updated — the two lists drifted, and this is the copy that
        // still has the bug. A real cleared page carrying only the telemetry
        // loader is currently misreported as a Cloudflare block by this
        // function (while looks_like_cloudflare_challenge correctly says no).
        let html = r#"<html><head><script src="/cdn-cgi/challenge-platform/scripts/jsd/main.js"></script></head><body><article>Real page content that loaded successfully after Cloudflare cleared it.</article></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("cloudflare"));
        assert!(!looks_like_cloudflare_challenge(html));
    }

    #[test]
    fn vendor_akamai_ref_id_uppercase_hex_not_matched() {
        // AKAMAI_REF_RE is case-sensitive and only matches lowercase hex
        // ([0-9a-f]); an uppercase-hex reference ID is a real (if narrow) gap.
        let html = "<html><body><p>Access Denied</p><p>Reference #18.2D351AB8.1557333295.A4E16AB</p></body></html>";
        assert_eq!(looks_like_vendor_block(html), None);
    }

    #[test]
    fn vendor_akamai_ref_id_lowercase_hex_matches() {
        let html = "<html><body><p>Reference #18.2d351ab8.1557333295.a4e16ab</p></body></html>";
        assert_eq!(looks_like_vendor_block(html), Some("akamai"));
    }

    #[test]
    fn vendor_head_15kb_boundary_marker_within_window_detected() {
        let mut html = String::from("<html><head>");
        html.push_str(&"<!-- pad -->".repeat(1000));
        html.push_str(r#"<span class="cf-error-code">1020</span>"#);
        html.push_str("</head></html>");
        assert!(html.floor_char_boundary(15_000) > 0);
        assert_eq!(looks_like_vendor_block(&html), Some("cloudflare"));
    }

    #[test]
    fn vendor_head_15kb_boundary_marker_beyond_window_not_detected() {
        let mut html = String::from("<html><head>");
        html.push_str(&"z".repeat(15_500));
        html.push_str(r#"<span class="cf-error-code">1020</span>"#);
        html.push_str("</head></html>");
        assert_eq!(looks_like_vendor_block(&html), None);
    }

    #[test]
    fn vendor_block_200kb_boundary_just_under_still_scanned() {
        let mut html = String::from(r#"<html><head><span class="cf-error-code">1020</span>"#);
        html.push_str(&"z".repeat(199_000));
        html.push_str("</head></html>");
        assert!(html.len() <= 200_000);
        assert_eq!(looks_like_vendor_block(&html), Some("cloudflare"));
    }

    #[test]
    fn vendor_block_200kb_boundary_just_over_not_scanned() {
        let mut html = String::from(r#"<html><head><span class="cf-error-code">1020</span>"#);
        html.push_str(&"z".repeat(201_000));
        html.push_str("</head></html>");
        assert!(html.len() > 200_000);
        assert_eq!(looks_like_vendor_block(&html), None);
    }

    #[test]
    fn vendor_block_case_insensitive_markers() {
        let html = r#"<html><body><H1>PARDON OUR INTERRUPTION</H1></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("akamai"));
    }

    #[test]
    fn vendor_block_akamai_mention_without_marker_is_none() {
        let html = r#"<html><body><article><p>We use Akamai for our CDN, which speeds up global delivery.</p></article></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), None);
    }

    #[test]
    fn vendor_block_datadome_mention_without_marker_is_none() {
        let html = r#"<html><body><article><p>DataDome is a bot-protection vendor some sites use.</p></article></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), None);
    }

    #[test]
    fn vendor_block_sucuri_mention_without_marker_is_none() {
        let html = r#"<html><body><article><p>Sucuri offers website firewall products for WordPress.</p></article></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), None);
    }

    #[test]
    fn vendor_block_kasada_mention_without_marker_is_none() {
        let html = r#"<html><body><article><p>Kasada makes bot-mitigation SDKs used by some airlines.</p></article></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), None);
    }

    #[test]
    fn vendor_block_imperva_mention_without_marker_is_none() {
        let html = r#"<html><body><article><p>Imperva provides application security services.</p></article></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), None);
    }

    #[test]
    fn vendor_block_priority_cloudflare_checked_before_akamai() {
        // A fixture with BOTH a Cloudflare marker and an Akamai marker: the
        // function must return the first match in source order (cloudflare).
        let html = r#"<html><body><span class="cf-error-code">1020</span><p>Pardon Our Interruption</p></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), Some("cloudflare"));
    }

    #[test]
    fn vendor_block_empty_string_is_none() {
        assert_eq!(looks_like_vendor_block(""), None);
    }

    #[test]
    fn vendor_block_malformed_truncated_does_not_panic() {
        let html = r#"<html><head><span class="cf-error-c"#;
        let _ = looks_like_vendor_block(html);
    }

    // ── looks_like_thin_html ─────────────────────────────────────────

    #[test]
    fn thin_html_boundary_199_is_thin() {
        let html = format!("<html><body>{}</body></html>", "z".repeat(199));
        assert!(looks_like_thin_html(&html));
    }

    #[test]
    fn thin_html_boundary_200_is_not_thin() {
        let html = format!("<html><body>{}</body></html>", "z".repeat(200));
        assert!(!looks_like_thin_html(&html));
    }

    #[test]
    fn thin_html_no_body_tag_defaults_to_not_thin() {
        let html = "no body tag here at all, just a raw fragment";
        assert!(!looks_like_thin_html(html));
    }

    #[test]
    fn thin_html_large_truncated_page_with_content_past_cap_is_not_thin() {
        // Real content sits past the </body> which itself sits past the 500KB
        // scan cap; the truncated fallback treats the rest of the slice as body.
        let mut html = String::from("<html><body>");
        html.push_str(&"real article words ".repeat(30_000));
        html.push_str("</body></html>");
        assert!(html.len() > 500_000);
        assert!(!looks_like_thin_html(&html));
    }

    #[test]
    fn thin_html_empty_string_defaults_to_not_thin() {
        // Same "no <body> tag" fallback as needs_js_rendering: an empty string
        // has no <body>, so extract_body_text_len defaults to 1000 and the
        // page reads as "probably has content" rather than thin.
        assert!(!looks_like_thin_html(""));
    }

    #[test]
    fn thin_html_whitespace_only_body_is_thin() {
        let html = "<html><body>   \n\t  </body></html>";
        assert!(looks_like_thin_html(html));
    }

    #[test]
    fn thin_html_unicode_body_counts_chars() {
        // 150 emoji chars: under the 200-char floor, still thin.
        let html = format!("<html><body>{}</body></html>", "\u{1F600}".repeat(150));
        assert!(looks_like_thin_html(&html));
    }

    #[test]
    fn thin_html_malformed_no_closing_body_truncated_treats_rest_as_body() {
        let mut html = String::from("<html><body>");
        html.push_str(&"real words here ".repeat(40_000));
        // deliberately no </body></html>
        assert!(html.len() > 500_000);
        assert!(!looks_like_thin_html(&html));
    }

    // ── warrants_browser_retry ───────────────────────────────────────

    #[test]
    fn browser_retry_meta_refresh_without_url_not_enough() {
        let html =
            r#"<html><head><meta http-equiv="refresh" content="5"></head><body>wait</body></html>"#;
        assert!(!warrants_browser_retry(html));
    }

    #[test]
    fn browser_retry_meta_with_url_but_not_refresh_not_enough() {
        let html = r#"<html><head><meta name="description" content="url=https://example.com"></head><body>hi</body></html>"#;
        assert!(!warrants_browser_retry(html));
    }

    #[test]
    fn browser_retry_meta_refresh_uppercase_still_matches() {
        let html = r#"<html><head><META HTTP-EQUIV="REFRESH" CONTENT="0; URL=https://example.org/real"></head></html>"#;
        assert!(warrants_browser_retry(html));
    }

    #[test]
    fn browser_retry_importmap_script_not_enough() {
        let html = r#"<html><body><p>hi</p><script type="importmap">{"imports":{}}</script></body></html>"#;
        assert!(!warrants_browser_retry(html));
    }

    #[test]
    fn browser_retry_speculationrules_script_not_enough() {
        let html = r#"<html><body><p>hi</p><script type="speculationrules">{"prefetch":[]}</script></body></html>"#;
        assert!(!warrants_browser_retry(html));
    }

    #[test]
    fn browser_retry_application_json_script_not_enough() {
        let html = r#"<html><body><p>hi</p><script type="application/json">{"a":1}</script></body></html>"#;
        assert!(!warrants_browser_retry(html));
    }

    #[test]
    fn browser_retry_inline_script_boundary_8_chars_not_enough() {
        // Trimmed inline body is exactly 8 chars: `len() > 8` is strict, so
        // exactly 8 must NOT warrant a retry.
        let html = r#"<html><body><script>12345678</script></body></html>"#;
        assert!(!warrants_browser_retry(html));
    }

    #[test]
    fn browser_retry_inline_script_boundary_9_chars_warrants_retry() {
        let html = r#"<html><body><script>123456789</script></body></html>"#;
        assert!(warrants_browser_retry(html));
    }

    #[test]
    fn browser_retry_mixed_data_and_executable_scripts_warrants_retry() {
        let html = r#"<html><body>
            <script type="application/ld+json">{"a":1}</script>
            <script>doSomethingReal();</script>
        </body></html>"#;
        assert!(warrants_browser_retry(html));
    }

    #[test]
    fn browser_retry_malformed_unclosed_script_does_not_panic() {
        let html = r#"<html><body><script>window.x = "#;
        let _ = warrants_browser_retry(html);
    }

    #[test]
    fn browser_retry_empty_string_is_false() {
        assert!(!warrants_browser_retry(""));
    }

    #[test]
    fn browser_retry_module_script_without_src_counts_as_inline() {
        let html = r#"<html><body><script type="module">import init from "./x.js"; init();</script></body></html>"#;
        assert!(warrants_browser_retry(html));
    }

    #[test]
    fn browser_retry_beyond_500kb_scan_window_not_seen() {
        let mut html = String::from("<html><body>");
        html.push_str(&"z".repeat(600_000));
        html.push_str(r#"<script src="/bundle.js"></script></body></html>"#);
        assert!(html.len() > 500_000);
        assert!(!warrants_browser_retry(&html));
    }

    // ── is_thin_markdown ──────────────────────────────────────────────

    #[test]
    fn thin_markdown_boundary_99_is_thin() {
        assert!(is_thin_markdown(99));
    }

    #[test]
    fn thin_markdown_boundary_100_is_not_thin() {
        assert!(!is_thin_markdown(100));
    }

    #[test]
    fn thin_markdown_boundary_101_is_not_thin() {
        assert!(!is_thin_markdown(101));
    }

    #[test]
    fn thin_markdown_zero_is_thin() {
        assert!(is_thin_markdown(0));
    }

    // ── FailedRenderReason / looks_like_failed_render ──────────────────

    #[test]
    fn failed_render_reason_as_str_next_client_error() {
        assert_eq!(
            FailedRenderReason::NextJsClientError.as_str(),
            "nextjs_client_error"
        );
    }

    #[test]
    fn failed_render_reason_as_str_react_minified() {
        assert_eq!(
            FailedRenderReason::ReactMinifiedError.as_str(),
            "react_minified_error"
        );
    }

    #[test]
    fn failed_render_reason_as_str_empty_next_root() {
        assert_eq!(
            FailedRenderReason::EmptyNextRoot.as_str(),
            "empty_next_root"
        );
    }

    #[test]
    fn empty_next_root_with_whitespace_only_is_detected() {
        let html = "<html><body><div id=\"__next\">    </div></body></html>";
        assert_eq!(
            looks_like_failed_render(html),
            Some(FailedRenderReason::EmptyNextRoot)
        );
    }

    #[test]
    fn next_root_with_nested_content_is_not_empty() {
        let html =
            r#"<html><body><div id="__next"><main><h1>Hello</h1></main></div></body></html>"#;
        assert!(looks_like_failed_render(html).is_none());
    }

    #[test]
    fn next_error_and_next_error_dunder_both_present_returns_first_match() {
        let html = r#"<html><body><div id="__next-error-0"></div><div id="__next_error__"></div></body></html>"#;
        assert_eq!(
            looks_like_failed_render(html),
            Some(FailedRenderReason::NextJsClientError)
        );
    }

    #[test]
    fn react_error_url_case_insensitive_match() {
        let html = r#"<html><body><a href="HTTPS://REACT.DEV/ERRORS/418">err</a></body></html>"#;
        assert_eq!(
            looks_like_failed_render(html),
            Some(FailedRenderReason::ReactMinifiedError)
        );
    }

    #[test]
    fn failed_render_200kb_boundary_just_under_still_scanned() {
        let mut html = String::from(r#"<html><body><div id="__next-error-0"></div>"#);
        html.push_str(&"<p>x</p>".repeat(24_800));
        html.push_str("</body></html>");
        assert!(html.len() <= 200_000);
        assert_eq!(
            looks_like_failed_render(&html),
            Some(FailedRenderReason::NextJsClientError)
        );
    }

    #[test]
    fn failed_render_malformed_truncated_does_not_panic() {
        let html = r#"<html><body><div id="__nex"#;
        let _ = looks_like_failed_render(html);
    }

    #[test]
    fn failed_render_next_id_single_quotes_not_matched() {
        // The marker check hardcodes a double-quoted `id="__next"`; a
        // single-quoted attribute is a real (narrow) gap in the detector.
        let html = r#"<html><body><div id='__next'></div></body></html>"#;
        assert!(looks_like_failed_render(html).is_none());
    }

    #[test]
    fn failed_render_similar_css_class_name_not_matched() {
        let html = r#"<html><body><div class="__next-error-banner">A styled banner, not an error boundary.</div></body></html>"#;
        assert!(looks_like_failed_render(html).is_none());
    }

    #[test]
    fn failed_render_multiple_next_divs_first_empty_detected() {
        let html = r#"<html><body><div id="__next"></div><p>footer</p></body></html>"#;
        assert_eq!(
            looks_like_failed_render(html),
            Some(FailedRenderReason::EmptyNextRoot)
        );
    }

    #[test]
    fn failed_render_unicode_content_inside_error_boundary_still_error() {
        let html = "<html><body><div id=\"__next-error-0\"><h2>Uygulama hatasi olustu \u{1F625}</h2></div></body></html>";
        assert_eq!(
            looks_like_failed_render(html),
            Some(FailedRenderReason::NextJsClientError)
        );
    }

    #[test]
    fn failed_render_empty_string_is_none() {
        assert!(looks_like_failed_render("").is_none());
    }

    // ── looks_like_loading_placeholder ──────────────────────────────

    macro_rules! loading_marker_tests {
        ($($name:ident => $marker:expr),+ $(,)?) => {
            $(
                #[test]
                fn $name() {
                    let html = format!("<html><body><p>{}</p></body></html>", $marker);
                    assert!(
                        looks_like_loading_placeholder(&html),
                        "loading marker {:?} should be recognized",
                        $marker
                    );
                }
            )+
        };
    }

    loading_marker_tests! {
        loading_marker_dots => "loading...",
        loading_marker_ellipsis_char => "loading\u{2026}",
        loading_marker_please_wait => "please wait",
        loading_marker_just_a_moment => "just a moment",
        loading_marker_initializing => "initializing",
        loading_marker_preparing => "preparing",
        loading_marker_one_moment => "one moment",
    }

    macro_rules! spinner_marker_tests {
        ($($name:ident => $marker:expr),+ $(,)?) => {
            $(
                #[test]
                fn $name() {
                    let html = format!(r#"<html><body><div {}"></div></body></html>"#, $marker);
                    assert!(
                        looks_like_loading_placeholder(&html),
                        "spinner marker {:?} should be recognized",
                        $marker
                    );
                }
            )+
        };
    }

    spinner_marker_tests! {
        spinner_marker_spinner => "class=\"spinner",
        spinner_marker_loader => "class=\"loader",
        spinner_marker_loading_class => "class=\"loading",
        spinner_marker_preloader => "class=\"preloader",
        spinner_marker_id_loader => "id=\"loader",
        spinner_marker_id_preloader => "id=\"preloader",
        spinner_marker_aria_loading => "aria-label=\"loading\"",
    }

    #[test]
    fn loading_placeholder_text_boundary_399_triggers() {
        let pad = "z".repeat(399 - "loading...".len());
        let html = format!("<html><body><p>{pad}loading...</p></body></html>");
        assert!(looks_like_loading_placeholder(&html));
    }

    #[test]
    fn loading_placeholder_text_boundary_400_does_not_trigger_via_text() {
        let pad = "z".repeat(400 - "loading...".len());
        let html = format!("<html><body><p>{pad}loading...</p></body></html>");
        assert!(!looks_like_loading_placeholder(&html));
    }

    #[test]
    fn loading_placeholder_spinner_boundary_199_triggers() {
        let pad = "z".repeat(199);
        let html = format!(r#"<html><body>{pad}<div class="spinner"></div></body></html>"#);
        assert!(looks_like_loading_placeholder(&html));
    }

    #[test]
    fn loading_placeholder_spinner_boundary_200_does_not_trigger_via_spinner() {
        let pad = "z".repeat(200);
        let html = format!(r#"<html><body>{pad}<div class="spinner"></div></body></html>"#);
        assert!(!looks_like_loading_placeholder(&html));
    }

    #[test]
    fn loading_placeholder_80kb_boundary_just_under_still_scanned() {
        let mut html = String::from("<html><body><p>loading...</p><!--");
        html.push_str(&"z".repeat(79_000));
        html.push_str("--></body></html>");
        assert!(html.len() <= 80_000);
        assert!(looks_like_loading_placeholder(&html));
    }

    #[test]
    fn loading_placeholder_80kb_boundary_just_over_not_scanned() {
        let mut html = String::from("<html><body><p>loading...</p><!--");
        html.push_str(&"z".repeat(90_000));
        html.push_str("--></body></html>");
        assert!(html.len() > 80_000);
        assert!(!looks_like_loading_placeholder(&html));
    }

    #[test]
    fn loading_placeholder_unicode_body_below_floor_is_placeholder() {
        let html = format!(
            "<html><body>{}<p>loading...</p></body></html>",
            "\u{1F600}".repeat(50)
        );
        assert!(looks_like_loading_placeholder(&html));
    }

    #[test]
    fn loading_placeholder_malformed_truncated_does_not_panic() {
        let html = "<html><body><div class=\"spin";
        let _ = looks_like_loading_placeholder(html);
    }

    // ── private helpers ──────────────────────────────────────────────

    #[test]
    fn strip_tag_blocks_removes_all_occurrences() {
        let html = "a<script>one</script>b<script>two</script>c";
        assert_eq!(strip_tag_blocks(html, "script"), "abc");
    }

    #[test]
    fn strip_tag_blocks_tag_not_present_unchanged() {
        let html = "just plain text, no scripts anywhere";
        assert_eq!(strip_tag_blocks(html, "script"), html);
    }

    #[test]
    fn strip_tag_blocks_unclosed_tag_drops_rest_of_document() {
        // Naive strip: an unclosed <script> consumes everything after it.
        let html = "keep this<script>but not this, ever, no closing tag";
        assert_eq!(strip_tag_blocks(html, "script"), "keep this");
    }

    #[test]
    fn strip_tag_blocks_empty_input() {
        assert_eq!(strip_tag_blocks("", "script"), "");
    }

    #[test]
    fn visible_text_from_stripped_html_collapses_whitespace() {
        let stripped = "<p>hello   \n\n  world</p>";
        assert_eq!(visible_text_from_stripped_html(stripped), "hello world");
    }

    #[test]
    fn visible_text_from_stripped_html_stray_open_angle_swallows_rest() {
        // A stray unmatched '<' flips `in_tag` permanently, so everything after
        // it is treated as tag content and dropped — a real, naive-parser
        // consequence worth pinning down.
        let stripped = "hello <world";
        assert_eq!(visible_text_from_stripped_html(stripped), "hello ");
    }

    #[test]
    fn visible_text_from_stripped_html_unicode_preserved() {
        let stripped = "<p>caf\u{00e9} \u{1F600}</p>";
        assert_eq!(
            visible_text_from_stripped_html(stripped),
            "caf\u{00e9} \u{1F600}"
        );
    }

    /// A LightPanda render cut off by its own `nav_budget` arrives without a
    /// closing `</body>`. Prod shipped exactly this to a paying customer as
    /// `success: true` with `creditCost: 1`, because the body extraction
    /// returned "" and the phrase list scanned nothing.
    /// Live case: https://www.prlib.ru/en/history/619410 on 2026-09-03.
    #[test]
    fn truncated_wall_without_close_body_is_a_bot_wall() {
        let html = "<html><body><h1>Security Check</h1>\
                    <p>Checking your browser before accessing the site</p>\
                    <p>This will take a few seconds";
        assert!(
            looks_like_generic_bot_wall(html, true),
            "a budget-truncated wall must be detected"
        );
    }

    /// The same body claiming to be complete stays undetected: a missing
    /// `</body>` on a NON-truncated response is a malformed page, and the
    /// existing contract treats that as thin so it escalates to a fresh
    /// render. This pins that the new parameter did not widen the default.
    #[test]
    fn same_wall_marked_complete_keeps_old_behaviour() {
        let html = "<html><body><h1>Security Check</h1>\
                    <p>Checking your browser before accessing the site</p>\
                    <p>This will take a few seconds";
        assert!(!looks_like_generic_bot_wall(html, false));
    }

    /// The 600-visible-char bail still protects a real article that was
    /// truncated mid-page and happens to mention a wall phrase, so the fix
    /// cannot strip content from a legitimate long page.
    #[test]
    fn truncated_long_article_mentioning_a_phrase_is_not_a_wall() {
        let filler = "the quick brown fox jumps over the lazy dog. ".repeat(30);
        let html = format!(
            "<html><body><h1>How bot walls work</h1><p>Sites often say \
             checking your browser before letting you in.</p><p>{filler}"
        );
        assert!(
            !looks_like_generic_bot_wall(&html, true),
            "a long truncated article must not be called a wall"
        );
    }

    #[test]
    fn body_html_without_scripts_lower_missing_close_body_not_truncated_is_empty() {
        let lower = "<html><body>hello world, no closing body tag here at all";
        assert_eq!(body_html_without_scripts_lower(lower, false), "");
    }

    #[test]
    fn body_html_without_scripts_lower_missing_close_body_truncated_reads_to_end() {
        let lower = "<html><body>hello world, truncated mid-stream with no closing tag";
        let out = body_html_without_scripts_lower(lower, true);
        assert!(out.contains("hello world"));
    }

    #[test]
    fn body_html_without_scripts_lower_no_body_tag_is_empty() {
        let lower = "<html><div>no body element in this document</div></html>";
        assert_eq!(body_html_without_scripts_lower(lower, false), "");
    }

    #[test]
    fn extract_body_text_len_no_body_tag_defaults_1000() {
        let lower = "<div>fragment with no body tag</div>";
        assert_eq!(extract_body_text_len(lower, false), 1000);
    }

    #[test]
    fn extract_body_text_len_counts_stripped_visible_text() {
        let lower = "<html><body><script>ignored</script><p>hello world</p></body></html>";
        // "hello world" minus the space = 10 non-whitespace chars.
        assert_eq!(extract_body_text_len(lower, false), 10);
    }

    // ── Cloudflare challenge / headers: additional coverage ────────────

    #[test]
    fn cf_strong_marker_managed_tk_dunder_detected() {
        let html = r#"<html><body><script>__cf_chl_managed_tk__ = "abc";</script></body></html>"#;
        assert!(looks_like_cloudflare_challenge(html));
    }

    #[test]
    fn cf_strong_marker_challenge_running_detected() {
        let html = r#"<html><body><script>if (cf-challenge-running) {}</script></body></html>"#;
        assert!(looks_like_cloudflare_challenge(html));
    }

    #[test]
    fn cf_bare_scripts_jsd_directory_alone_not_flagged() {
        // Ordinary Bot-Management telemetry loader (not the /h/ orchestrator)
        // must not, by itself, count as a challenge — this is the exact
        // distinction the STRONG list comment documents.
        let html = r#"<html><head><script src="/cdn-cgi/challenge-platform/scripts/jsd/main.js"></script></head><body><article>A page that already cleared Cloudflare and rendered normally.</article></body></html>"#;
        assert!(!looks_like_cloudflare_challenge(html));
    }

    #[test]
    fn cf_weak_markers_attention_required_plus_checking_browser() {
        let html = r#"<html><body><h1>Attention Required!</h1><p>Checking your browser before accessing.</p></body></html>"#;
        assert!(looks_like_cloudflare_challenge(html));
    }

    #[test]
    fn cf_weak_markers_performance_security_ampersand_variant() {
        let html = r#"<html><body><p>Just a moment...</p><footer>Performance &amp; security by Cloudflare</footer></body></html>"#;
        assert!(looks_like_cloudflare_challenge(html));
    }

    #[test]
    fn cf_mitigated_header_captcha_is_false() {
        // `cf-mitigated` only documents "challenge" and "block"; "captcha" is
        // not a recognized value for THIS header (unlike the AWS WAF header).
        assert!(!is_cloudflare_mitigated_header("captcha"));
    }

    #[test]
    fn cf_mitigated_header_block_uppercase() {
        assert!(is_cloudflare_mitigated_header("BLOCK"));
    }

    #[test]
    fn cf_mitigated_header_tab_and_newline_whitespace_trimmed() {
        assert!(is_cloudflare_mitigated_header("\t\nchallenge\n\t"));
    }

    #[test]
    fn aws_waf_action_header_challenge_true() {
        assert!(is_aws_waf_action_header("challenge"));
    }

    #[test]
    fn aws_waf_action_header_captcha_true() {
        assert!(is_aws_waf_action_header("captcha"));
    }

    #[test]
    fn aws_waf_action_header_block_is_false() {
        // Unlike `cf-mitigated`, AWS documents Challenge/CAPTCHA for this
        // header — a Block action uses its own status/body, not this value.
        assert!(!is_aws_waf_action_header("block"));
    }

    #[test]
    fn aws_waf_action_header_case_insensitive() {
        assert!(is_aws_waf_action_header("CAPTCHA"));
        assert!(is_aws_waf_action_header("Challenge"));
    }

    #[test]
    fn aws_waf_action_header_whitespace_trimmed() {
        assert!(is_aws_waf_action_header("  challenge  "));
    }

    #[test]
    fn aws_waf_action_header_empty_is_false() {
        assert!(!is_aws_waf_action_header(""));
    }

    // ── divergence with crw_extract::antibot::classify (the system meant
    //    to be wired into the failover loop) ────────────────────────────

    #[test]
    fn bug_akamai_uppercase_hex_diverges_between_detector_and_antibot() {
        // BUG: detector::AKAMAI_REF_RE is case-sensitive and only matches
        // lowercase hex digits ([0-9a-f]) with no flexible whitespace around
        // "#". antibot's TIER1 Akamai pattern is `(?i)` with `\s*` around "#".
        // A real Akamai block page using uppercase hex in its reference ID is
        // missed by detector::looks_like_vendor_block but correctly caught by
        // antibot::classify, which is the system meant to be wired into the
        // failover loop.
        let html = "<html><body><p>Access Denied</p><p>Reference #18.2D351AB8.1557333295.A4E16AB</p></body></html>";
        assert_eq!(looks_like_vendor_block(html), None);
        let r = crw_extract::antibot::classify(Some(403), html);
        assert_eq!(r.signal, crw_extract::antibot::AntibotSignal::Akamai);
    }

    #[test]
    fn bug_access_denied_on_http_200_diverges_between_detector_and_antibot() {
        // BUG: detector::looks_like_generic_bot_wall flags "access denied"
        // text regardless of HTTP status. antibot::classify only checks its
        // GenericBlock-producing TIER2 patterns (which include "Access
        // Denied") when status is 403/503 or >= 400 — never on a 200. A
        // real HTTP-200 page that happens to discuss "access denied" errors
        // in prose is flagged by the detector but NOT by antibot::classify.
        let html = r#"<html><body><article><h1>Understanding Access Denied Errors</h1>
            <p>When your app throws access denied, check the permissions.</p>
            </article></body></html>"#;
        assert!(looks_like_generic_bot_wall(html, false));
        let r = crw_extract::antibot::classify(Some(200), html);
        assert_eq!(r.signal, crw_extract::antibot::AntibotSignal::None);
    }

    #[test]
    fn bug_vercel_checkpoint_has_no_vendor_attribution_in_detector() {
        // BUG (partial): detector::looks_like_vendor_block has no Vercel
        // signature at all, so it never names the vendor. The generic phrase
        // list happens to catch this particular page anyway (its "security
        // check" phrase is a substring of "Security Checkpoint"), but only as
        // an unattributed generic wall — detector.rs cannot say "Vercel" the
        // way antibot::classify does with its dedicated Vercel pattern. A
        // differently-worded Vercel checkpoint that doesn't happen to contain
        // "security check" would be invisible to detector.rs entirely.
        let html = "<html><body><h1>Vercel Security Checkpoint</h1>\
            <p>We're verifying your browser</p></body></html>";
        assert_eq!(looks_like_vendor_block(html), None);
        assert!(looks_like_generic_bot_wall(html, false));
        let r = crw_extract::antibot::classify(Some(200), html);
        assert_eq!(r.signal, crw_extract::antibot::AntibotSignal::Vercel);
    }

    #[test]
    fn bug_network_security_block_recognized_only_by_antibot() {
        // BUG: the "blocked by network security" phrasing (a Reddit-class WAF
        // page) is not in detector::looks_like_generic_bot_wall's phrase list
        // at all, so detector.rs has no way to see this block. antibot.rs
        // added it as a dedicated NetworkSecurity TIER1 pattern.
        let html =
            "<html><body>You've been blocked by network security. Contact support.</body></html>";
        assert!(!looks_like_generic_bot_wall(html, false));
        assert_eq!(looks_like_vendor_block(html), None);
        let r = crw_extract::antibot::classify(Some(200), html);
        assert_eq!(
            r.signal,
            crw_extract::antibot::AntibotSignal::NetworkSecurity
        );
    }

    #[test]
    fn bug_google_rate_limit_page_recognized_only_by_antibot() {
        // BUG: Google's "sent too many requests" rate-limit page (served as
        // HTTP 200 by JS renderers that don't propagate the real 429) has no
        // equivalent detector.rs predicate. antibot.rs added a dedicated
        // RateLimited TIER1 pattern specifically to catch this case.
        let html = "<html><body><p>We're sorry, but you have sent too many requests to us \
            recently. Please try again later.</p></body></html>";
        assert!(!looks_like_generic_bot_wall(html, false));
        let r = crw_extract::antibot::classify(Some(200), html);
        assert_eq!(r.signal, crw_extract::antibot::AntibotSignal::RateLimited);
    }

    #[test]
    fn bug_google_unusual_traffic_recognized_only_by_antibot() {
        // BUG: Google's HTTP-200 "/sorry" reCAPTCHA bot wall ("unusual traffic
        // from your computer network") has no detector.rs equivalent; the
        // phrase list in looks_like_generic_bot_wall does not include it.
        // antibot.rs added a dedicated GenericBlock TIER1 pattern for it.
        let html = "<html><head><title>Sorry...</title></head><body>\
            <p>Our systems have detected unusual traffic from your computer \
            network.</p></body></html>";
        assert!(!looks_like_generic_bot_wall(html, false));
        let r = crw_extract::antibot::classify(Some(200), html);
        assert_eq!(r.signal, crw_extract::antibot::AntibotSignal::GenericBlock);
    }

    #[test]
    fn cf_mitigated_header_whitespace_only_is_false() {
        assert!(!is_cloudflare_mitigated_header("   "));
    }

    #[test]
    fn strip_tag_blocks_naive_nested_same_tag_stops_at_first_close() {
        // Naive strip: it is not nesting-aware, so a "nested" <script> inside
        // another closes at the FIRST </script>, leaving the outer close tag
        // as literal text. This pins the current (real) non-nesting behavior.
        let html = "a<script>outer<script>inner</script>orphan-close</script>b";
        assert_eq!(strip_tag_blocks(html, "script"), "aorphan-close</script>b");
    }

    #[test]
    fn visible_text_from_stripped_html_only_tags_no_text_is_empty() {
        let stripped = "<div><span></span></div>";
        assert_eq!(visible_text_from_stripped_html(stripped), "");
    }

    #[test]
    fn body_html_without_scripts_lower_strips_both_script_and_style() {
        let lower =
            "<html><body><style>.a{color:red}</style><script>x()</script><p>hi</p></body></html>";
        assert_eq!(body_html_without_scripts_lower(lower, false), "<p>hi</p>");
    }

    #[test]
    fn extract_body_text_len_large_real_content_exceeds_1000() {
        let lower = format!(
            "<html><body><article>{}</article></body></html>",
            "word ".repeat(1000)
        );
        assert!(extract_body_text_len(&lower, false) > 1000);
    }

    #[test]
    fn needs_js_rendering_newlines_do_not_count_toward_body_length() {
        // Whitespace (including newlines) never counts toward body_len, so a
        // page padded only with newlines still reads as short and an SPA
        // indicator inside it still fires.
        let html = format!("<html><body>{}id=\"root\"</body></html>", "\n".repeat(5000));
        assert!(needs_js_rendering(&html));
    }

    #[test]
    fn bot_wall_phrases_summing_past_600_chars_combined_not_flagged() {
        let filler = "z".repeat(650);
        let html = format!("<html><body><p>access denied {filler}</p></body></html>");
        assert!(!looks_like_generic_bot_wall(&html, false));
    }

    #[test]
    fn vendor_sucuri_extra_internal_spacing_does_not_match() {
        // Substring match only: "Sucuri  Website  Firewall" (double spaces)
        // does not equal the exact marker "sucuri website firewall".
        let html =
            r#"<html><body><h1>Sucuri  Website  Firewall - Access Denied</h1></body></html>"#;
        assert_eq!(looks_like_vendor_block(html), None);
    }

    #[test]
    fn cf_each_single_weak_marker_alone_is_insufficient() {
        let weak_phrases = [
            "just a moment",
            "checking your browser",
            "attention required",
            "performance &amp; security by cloudflare",
            "performance & security by cloudflare",
        ];
        for phrase in weak_phrases {
            let html = format!("<html><body><p>{phrase}</p></body></html>");
            assert!(
                !looks_like_cloudflare_challenge(&html),
                "single weak marker {phrase:?} must not trigger alone"
            );
        }
    }

    #[test]
    fn failed_render_200kb_boundary_just_over_not_scanned() {
        let mut html = String::from(r#"<html><body><div id="__next-error-0"></div>"#);
        html.push_str(&"<p>x</p>".repeat(25_100));
        html.push_str("</body></html>");
        assert!(html.len() > 200_000);
        assert!(looks_like_failed_render(&html).is_none());
    }

    #[test]
    fn thin_markdown_large_value_is_not_thin() {
        assert!(!is_thin_markdown(100_000));
    }

    #[test]
    fn mitigated_headers_disagree_on_block_value_by_design() {
        // Cross-check the two header predicates: "block" is a valid
        // `cf-mitigated` value but is explicitly NOT a valid
        // `x-amzn-waf-action` value (a WAF Block action uses its own
        // configured status/body instead) — this is deliberate per the
        // doc comment on is_aws_waf_action_header, not a bug.
        assert!(is_cloudflare_mitigated_header("block"));
        assert!(!is_aws_waf_action_header("block"));
    }

    #[test]
    fn needs_js_rendering_builder_indicator_case_insensitive() {
        let html = format!("<html><body>{}WIXSITE.COM</body></html>", "z".repeat(300));
        assert!(needs_js_rendering(&html));
    }

    #[test]
    fn bot_wall_phrase_uppercase_still_matches() {
        let html = "<html><body><p>ACCESS DENIED</p></body></html>";
        assert!(looks_like_generic_bot_wall(html, false));
    }

    #[test]
    fn vendor_block_control_characters_do_not_panic() {
        let html = "<html><body>\u{0}\u{1}\u{2}Access Denied</body></html>";
        let _ = looks_like_vendor_block(html);
    }

    #[test]
    fn browser_retry_only_last_of_several_meta_tags_has_refresh() {
        let html = r#"<html><head>
            <meta charset="utf-8">
            <meta name="viewport" content="width=device-width">
            <meta http-equiv="refresh" content="0; url=https://example.org/real">
        </head><body>redirecting</body></html>"#;
        assert!(warrants_browser_retry(html));
    }

    #[test]
    fn cf_strong_marker_just_under_512kb_scan_limit_detected() {
        let mut html = String::from("<html><body>");
        html.push_str(&"<p>x</p>".repeat(63_000)); // ~504KB, under 512KB
        html.push_str(r#"<div id="cf-browser-verification"></div></body></html>"#);
        assert!(html.len() < 512 * 1024);
        assert!(looks_like_cloudflare_challenge(&html));
    }

    #[test]
    fn needs_js_rendering_large_real_page_with_many_scripts_stays_false() {
        // body_len well over the 1000-char bundler threshold, even with 10
        // script tags present: a genuinely large real page must not trigger.
        let mut html = String::from("<html><body><article>");
        html.push_str(&"real article content word ".repeat(200));
        for _ in 0..10 {
            html.push_str("<script>1</script>");
        }
        html.push_str("</article></body></html>");
        assert!(!needs_js_rendering(&html));
    }
}
