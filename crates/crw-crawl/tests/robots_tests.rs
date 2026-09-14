use crw_crawl::robots::RobotsTxt;

#[test]
fn robots_multiple_user_agents() {
    let text = r#"
User-agent: Googlebot
Disallow: /google-only/

User-agent: *
Disallow: /admin/
Disallow: /private/

User-agent: Bingbot
Disallow: /bing-only/

Sitemap: https://example.com/sitemap.xml
"#;
    let robots = RobotsTxt::parse(text);
    // Only * section should apply to CRW
    assert!(!robots.is_allowed("/admin/page"));
    assert!(!robots.is_allowed("/private/page"));
    // Googlebot-specific rules should NOT apply
    assert!(robots.is_allowed("/google-only/page"));
    // Bingbot-specific rules should NOT apply
    assert!(robots.is_allowed("/bing-only/page"));
    assert_eq!(robots.sitemaps, vec!["https://example.com/sitemap.xml"]);
}

#[test]
fn robots_empty_file() {
    let robots = RobotsTxt::parse("");
    assert!(robots.is_allowed("/anything"));
    assert!(robots.is_allowed("/admin/secret"));
    assert!(robots.sitemaps.is_empty());
}

#[test]
fn robots_crw_specific_section() {
    let text = r#"
User-agent: *
Disallow: /

User-agent: CRW
Disallow: /crw-blocked/
"#;
    let robots = RobotsTxt::parse(text);
    // The exact CRW group takes precedence over the wildcard group.
    assert!(robots.is_allowed("/anything"));
    assert!(!robots.is_allowed("/crw-blocked/page"));
}

#[test]
fn robots_wildcard_in_disallow() {
    let text = r#"
User-agent: *
Disallow: /*.pdf
"#;
    let robots = RobotsTxt::parse(text);
    assert!(
        !robots.is_allowed("/document.pdf"),
        "Wildcard patterns should block .pdf URLs"
    );
    assert!(!robots.is_allowed("/path/to/file.pdf"));
    assert!(robots.is_allowed("/document.html"));
}

#[test]
fn robots_allow_directive() {
    let text = r#"
User-agent: *
Disallow: /private/
Allow: /private/public-page
"#;
    let robots = RobotsTxt::parse(text);
    assert!(!robots.is_allowed("/private/secret"));
    assert!(robots.is_allowed("/private/public-page"));
    assert!(robots.is_allowed("/public/anything"));
}

#[test]
fn robots_specificity_longer_wins() {
    let text = r#"
User-agent: *
Disallow: /
Allow: /public/
"#;
    let robots = RobotsTxt::parse(text);
    assert!(!robots.is_allowed("/private"));
    assert!(robots.is_allowed("/public/page"));
}

#[test]
fn robots_dollar_end_anchor() {
    let text = "User-agent: *\nDisallow: /*.pdf$\n";
    let robots = RobotsTxt::parse(text);
    assert!(!robots.is_allowed("/doc.pdf"));
    assert!(robots.is_allowed("/doc.pdf?q=1"));
}

#[test]
fn robots_comments_ignored() {
    // Note: inline comments (after directives) are NOT stripped by the parser.
    // Only full-line comments (starting with #) are properly handled.
    let text = r#"
# This is a comment
User-agent: *
Disallow: /blocked/
# Another comment
Sitemap: https://example.com/sitemap.xml
"#;
    let robots = RobotsTxt::parse(text);
    assert!(!robots.is_allowed("/blocked/page"));
    assert!(robots.is_allowed("/allowed/page"));
}

#[test]
fn robots_multiple_sitemaps() {
    let text = r#"
User-agent: *
Disallow:

Sitemap: https://example.com/sitemap1.xml
Sitemap: https://example.com/sitemap2.xml
Sitemap: https://example.com/sitemap3.xml
"#;
    let robots = RobotsTxt::parse(text);
    assert_eq!(robots.sitemaps.len(), 3);
    assert!(robots.is_allowed("/anything"));
}

#[test]
fn robots_case_insensitive_directives() {
    let text = "USER-AGENT: *\nDISALLOW: /blocked/\nSITEMAP: https://example.com/sitemap.xml\n";
    let robots = RobotsTxt::parse(text);
    assert!(!robots.is_allowed("/blocked/page"));
    assert_eq!(robots.sitemaps.len(), 1);
}

/// Real Hacker News robots.txt. Its rules key on the query string, so matching
/// on the bare path silently allows everything it forbids — which is exactly
/// how /map ended up rendering `/hide?id=...` through a full Chrome ladder.
const HN_ROBOTS: &str = "User-Agent: *\n\
Crawl-delay: 30\n\
Disallow: /x?\n\
Disallow: /r?\n\
Disallow: /vote?\n\
Disallow: /reply?\n\
Disallow: /submitlink?\n\
Disallow: /collapse?\n\
Disallow: /context?\n\
Disallow: /fave?\n\
Disallow: /flag?\n\
Disallow: /hide?\n\
Disallow: /login\n\
Disallow: /logout\n";

#[test]
fn url_allowed_matches_on_path_and_query() {
    let robots = RobotsTxt::parse(HN_ROBOTS);

    // Forbidden action links: these were being fetched before the fix.
    for blocked in [
        "https://news.ycombinator.com/hide?id=48876505&goto=news",
        "https://news.ycombinator.com/vote?id=123&how=up",
        "https://news.ycombinator.com/reply?id=9&goto=item",
        "https://news.ycombinator.com/login",
    ] {
        let url = url::Url::parse(blocked).unwrap();
        assert!(
            !robots.is_url_allowed(&url),
            "robots.txt forbids {blocked}, it must not be fetched"
        );
    }

    // Real content that HN does NOT disallow must still be crawled: robots is
    // the authority, so we must not over-filter into a hand-rolled deny-list.
    for allowed in [
        "https://news.ycombinator.com/",
        "https://news.ycombinator.com/item?id=48876505",
        "https://news.ycombinator.com/user?id=tionis",
        "https://news.ycombinator.com/from?site=iroh.computer",
    ] {
        let url = url::Url::parse(allowed).unwrap();
        assert!(
            robots.is_url_allowed(&url),
            "robots.txt permits {allowed}, it must still be crawled"
        );
    }
}

/// Pins the exact bug: matching on the bare path lets `/hide?...` through.
#[test]
fn bare_path_matching_would_miss_query_keyed_rules() {
    let robots = RobotsTxt::parse(HN_ROBOTS);
    let url = url::Url::parse("https://news.ycombinator.com/hide?id=1&goto=news").unwrap();

    // The old call shape — path only — wrongly reports "allowed".
    assert!(robots.is_allowed(url.path()));
    // The fixed call shape correctly reports "forbidden".
    assert!(!robots.is_url_allowed(&url));
}
