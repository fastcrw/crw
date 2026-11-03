use crw_core::types::OutputFormat;
use crw_extract::ExtractOptions;

#[test]
fn extract_markdown_format() {
    let html = "<html><head><title>Test</title></head><body><article><h1>Hello</h1><p>World</p></article></body></html>";
    let data = crw_extract::extract(ExtractOptions {
        raw_html: html,
        content_type: None,
        source_url: "https://example.com",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 100,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Markdown],
        only_main_content: true,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    assert!(data.markdown.is_some());
    assert!(data.html.is_none());
    assert!(data.raw_html.is_none());
    assert!(data.plain_text.is_none());
    assert!(data.links.is_none());
    // images must be gated off unless the format is requested.
    assert!(data.images.is_none());
    assert!(data.json.is_none());
    assert_eq!(data.metadata.status_code, 200);
    assert_eq!(data.metadata.source_url, "https://example.com");
}

#[test]
fn extract_images_format_populates_from_raw_html() {
    let html = "<html><head><meta property=\"og:image\" content=\"https://example.com/og.png\"></head>\
        <body><article><img src=\"/pic.png\" alt=\"Pic\"></article></body></html>";
    let data = crw_extract::extract(ExtractOptions {
        raw_html: html,
        content_type: None,
        source_url: "https://example.com",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 100,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Images],
        only_main_content: true,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    let images = data.images.expect("images populated");
    let urls: Vec<_> = images.iter().map(|i| i.url.as_str()).collect();
    // Extracts from the FULL raw HTML (og:image + <img>), not just main content.
    assert!(urls.contains(&"https://example.com/pic.png"));
    assert!(urls.contains(&"https://example.com/og.png"));
    let pic = images.iter().find(|i| i.url.ends_with("pic.png")).unwrap();
    assert_eq!(pic.alt.as_deref(), Some("Pic"));
}

#[test]
fn extract_all_formats() {
    let html = "<html><head><title>Test</title></head><body><article><h1>Hello</h1><p>World</p><a href='/page'>Link</a></article></body></html>";
    let formats = vec![
        OutputFormat::Markdown,
        OutputFormat::Html,
        OutputFormat::RawHtml,
        OutputFormat::PlainText,
        OutputFormat::Links,
    ];

    let data = crw_extract::extract(ExtractOptions {
        raw_html: html,
        content_type: None,
        source_url: "https://example.com",
        status_code: 200,
        rendered_with: Some("http".into()),
        elapsed_ms: 50,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &formats,
        only_main_content: false,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    assert!(data.markdown.is_some(), "markdown should be present");
    assert!(data.html.is_some(), "html should be present");
    assert!(data.raw_html.is_some(), "raw_html should be present");
    assert!(data.plain_text.is_some(), "plain_text should be present");
    assert!(data.links.is_some(), "links should be present");
    // JSON is always None from extract() — handled async separately
    assert!(data.json.is_none());

    assert_eq!(data.metadata.rendered_with.as_deref(), Some("http"));
    assert_eq!(data.metadata.elapsed_ms, 50);
}

#[test]
fn extract_metadata_populated() {
    let html = r#"<html lang="en"><head>
        <title>My Page</title>
        <meta name="description" content="A description">
    </head><body><p>Content</p></body></html>"#;

    let data = crw_extract::extract(ExtractOptions {
        raw_html: html,
        content_type: None,
        source_url: "https://example.com",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 10,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Markdown],
        only_main_content: false,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    assert_eq!(data.metadata.title.as_deref(), Some("My Page"));
    assert_eq!(data.metadata.description.as_deref(), Some("A description"));
    assert_eq!(data.metadata.language.as_deref(), Some("en"));
}

#[test]
fn extract_empty_html() {
    let data = crw_extract::extract(ExtractOptions {
        raw_html: "",
        content_type: None,
        source_url: "https://example.com",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 0,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Markdown, OutputFormat::PlainText],
        only_main_content: false,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    // Should not crash
    assert!(data.markdown.is_some());
    assert!(data.plain_text.is_some());
}

#[test]
fn extract_with_include_exclude_tags() {
    let html =
        r#"<html><body><div class="ad">Ad</div><article><p>Content</p></article></body></html>"#;
    let data = crw_extract::extract(ExtractOptions {
        raw_html: html,
        content_type: None,
        source_url: "https://example.com",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 0,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Markdown],
        only_main_content: false,
        include_tags: &["article".into()],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    let md = data.markdown.unwrap();
    assert!(md.contains("Content"), "Should include article content");
}

/// News/blog templates often place the article H1 in a `<header>` sibling of
/// the readability-scored container, so the title vanishes from markdown.
/// `extract` must restore it from the metadata title (preferring `og:title`).
#[test]
fn prepends_metadata_title_when_missing_from_markdown() {
    let html = r#"<html><head>
        <title>Compute Module 4 Cold Spec - Raspberry Pi</title>
        <meta property="og:title" content="New extended temperature range for Compute Module 4">
    </head><body>
        <header><h1>New extended temperature range for Compute Module 4</h1></header>
        <article><p>Body paragraph that mentions thousands of embedded customers in challenging environments.</p></article>
    </body></html>"#;
    let data = crw_extract::extract(ExtractOptions {
        raw_html: html,
        content_type: None,
        source_url: "https://example.com",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 0,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Markdown],
        only_main_content: true,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    let md = data.markdown.unwrap();
    assert!(
        md.contains("New extended temperature range for Compute Module 4"),
        "title should be present in markdown, got: {md:?}"
    );
}

/// When the title is already present in the extracted markdown (e.g. the H1
/// lived inside the readability-selected article), don't double up.
#[test]
fn does_not_duplicate_title_already_in_markdown() {
    let html = r#"<html><head><title>Hello World</title></head><body>
        <article><h1>Hello World</h1><p>Body paragraph that gives readability some content to score.</p></article>
    </body></html>"#;
    let data = crw_extract::extract(ExtractOptions {
        raw_html: html,
        content_type: None,
        source_url: "https://example.com",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 0,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Markdown],
        only_main_content: true,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    let md = data.markdown.unwrap();
    assert_eq!(
        md.matches("Hello World").count(),
        1,
        "title should appear exactly once, got: {md:?}"
    );
}

/// Strip " | Site Name" / " - Site Name" / em-dash suffixes from raw `<title>`.
#[test]
fn strips_site_name_suffix_from_title_when_prepending() {
    let html = r#"<html><head>
        <title>Article Title – Some Blog</title>
    </head><body>
        <article><p>Body content paragraph for readability to chew on.</p></article>
    </body></html>"#;
    let data = crw_extract::extract(ExtractOptions {
        raw_html: html,
        content_type: None,
        source_url: "https://example.com",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 0,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Markdown],
        only_main_content: true,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    let md = data.markdown.unwrap();
    assert!(
        md.contains("Article Title"),
        "core title should appear: {md:?}"
    );
    assert!(
        !md.contains("Some Blog"),
        "site-name suffix should be stripped: {md:?}"
    );
}

/// Regression: the title-suffix stripper must not split on bare en/em dashes
/// without surrounding whitespace. metmuseum's `<title>` is
/// "Northern Song Dynasty (960–1127) | Essay | …" — splitting on a bare en
/// dash truncated the title to "Northern Song Dynasty (960", which then no
/// longer matched any body phrase. Whitespace-anchored splits preserve the
/// in-title dash.
#[test]
fn preserves_en_dash_inside_title_parentheses() {
    let html = r#"<html><head>
        <title>Northern Song Dynasty (960–1127) | Essay | The Met</title>
    </head><body>
        <article><p>The Song dynasty was a brilliant era in Chinese history with substantial cultural achievement across the centuries.</p></article>
    </body></html>"#;
    let data = crw_extract::extract(ExtractOptions {
        raw_html: html,
        content_type: None,
        source_url: "https://example.com",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 0,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Markdown],
        only_main_content: true,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    let md = data.markdown.unwrap();
    assert!(
        md.contains("Northern Song Dynasty (960–1127)"),
        "in-title en dash must survive: {md:?}"
    );
    assert!(
        !md.contains("The Met"),
        "site-name suffix after pipe should be stripped: {md:?}"
    );
}

/// When the caller passed an explicit selector, the user opted into a narrow
/// extraction — we must not inject metadata they didn't ask for.
#[test]
fn does_not_prepend_title_when_css_selector_provided() {
    let html = r#"<html><head>
        <meta property="og:title" content="Page Title">
    </head><body>
        <main><p id="target">Just this paragraph.</p></main>
    </body></html>"#;
    let data = crw_extract::extract(ExtractOptions {
        raw_html: html,
        content_type: None,
        source_url: "https://example.com",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 0,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Markdown],
        only_main_content: true,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: Some("#target"),
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    let md = data.markdown.unwrap();
    assert!(
        !md.contains("Page Title"),
        "selector path must not inject metadata title: {md:?}"
    );
}

/// A *domain*-config-supplied selector (auto-applied per host) is not user
/// opt-in — title prepending must still fire when the article H1 lives outside
/// the selected container. Regression: `www.raspberrypi.com` ships with the
/// default selector `article.entry-content, main`; without this carve-out, the
/// title injection silently skips for every domain in `[extraction.domain_selectors]`.
#[test]
fn prepends_title_when_only_domain_selector_applies() {
    let html = r#"<html><head>
        <meta property="og:title" content="New extended temperature range for Compute Module 4 - Raspberry Pi">
    </head><body>
        <nav><h1>News</h1></nav>
        <main><p>While the Raspberry Pi project has its origins in education, the majority of Raspberry Pi computers we make today are destined for industrial and embedded applications.</p></main>
    </body></html>"#;
    let mut domain_map = std::collections::HashMap::new();
    domain_map.insert("www.raspberrypi.com".to_string(), "main".to_string());
    let data = crw_extract::extract(ExtractOptions {
        raw_html: html,
        content_type: None,
        source_url: "https://www.raspberrypi.com/news/x/",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 0,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Markdown],
        only_main_content: true,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: Some(&domain_map),
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    let md = data.markdown.unwrap();
    assert!(
        md.contains("New extended temperature range for Compute Module 4"),
        "domain-default selector must not suppress title prepend: {md:?}"
    );
}

// Content-rich page (title + meta description + a 2-row table + a 5-item list)
// so every re-leak vector is live: title-prepend, description-append, AND the
// `structural` / `basic_clean` alternates (which need >=2 table rows or >=5
// list items to fire). Used by the selector no-match regression tests below.
const NO_MATCH_PAGE: &str = "<html><head><title>My Page Title</title>\
    <meta name=\"description\" content=\"A long meta description that would otherwise be appended to short output and re-leak page context to the caller.\"></head>\
    <body><nav>Nav</nav><article><h1>Real Heading</h1><p>Real body paragraph.</p>\
    <table><tr><td>Row1A</td><td>Row1B</td></tr><tr><td>Row2A</td><td>Row2B</td></tr></table>\
    <ul><li>Item one</li><li>Item two</li><li>Item three</li><li>Item four</li><li>Item five</li></ul>\
    </article><footer>Foot</footer></body></html>";

fn extract_markdown(
    include_tags: &[String],
    css_selector: Option<&str>,
    xpath: Option<&str>,
) -> crw_core::types::ScrapeData {
    crw_extract::extract(ExtractOptions {
        raw_html: NO_MATCH_PAGE,
        content_type: None,
        source_url: "https://example.com",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 100,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Markdown],
        only_main_content: true,
        include_tags,
        exclude_tags: &[],
        css_selector,
        xpath,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap()
}

fn no_match_warning_count(data: &crw_core::types::ScrapeData) -> usize {
    data.warnings
        .iter()
        .filter(|w| *w == "selector_no_match")
        .count()
}

// A user-supplied `include_tags` selector that matches nothing must yield empty
// output through the *full* extract pipeline (not just clean_html), and warn
// exactly once. Regression guard: title-prepend, description-append, and the
// alternates ladder (basic_clean / structural) previously re-leaked the page
// and double-pushed the warning. See issue #291.
#[test]
fn include_tags_no_match_extract_returns_empty_single_warning() {
    let data = extract_markdown(&[".does-not-exist".to_string()], None, None);
    let md = data.markdown.clone().unwrap_or_default();
    assert!(
        md.trim().is_empty(),
        "unmatched include_tags must not re-leak title/description/tables/lists: {md:?}"
    );
    assert_eq!(
        no_match_warning_count(&data),
        1,
        "expected exactly one selector_no_match, got {:?}",
        data.warnings
    );
}

// include_tags no-match takes precedence over a later css/xpath selector applied
// to the (now-empty) document: an xpath scalar like `count(//x)` must not leak a
// bogus "0", and the two paths must not stack a second warning.
#[test]
fn include_tags_no_match_beats_trailing_xpath_scalar() {
    let data = extract_markdown(
        &[".does-not-exist".to_string()],
        None,
        Some("count(//article)"),
    );
    let md = data.markdown.clone().unwrap_or_default();
    assert!(
        md.trim().is_empty(),
        "include_tags no-match must win over an xpath scalar: {md:?}"
    );
    assert_eq!(
        no_match_warning_count(&data),
        1,
        "warnings: {:?}",
        data.warnings
    );
}

// include_tags AND css both matching nothing must still warn exactly once, not
// once per path.
#[test]
fn include_tags_and_css_both_no_match_warn_once() {
    let data = extract_markdown(&[".does-not-exist".to_string()], Some("#missing"), None);
    let md = data.markdown.clone().unwrap_or_default();
    assert!(md.trim().is_empty(), "both no-match must be empty: {md:?}");
    assert_eq!(
        no_match_warning_count(&data),
        1,
        "warnings: {:?}",
        data.warnings
    );
}

// A css selector that matches nothing (no include_tags) stays empty + one
// warning — the behavior the base PR established, guarded here against the
// refactor above.
#[test]
fn css_no_match_extract_returns_empty_single_warning() {
    let data = extract_markdown(&[], Some(".does-not-exist"), None);
    let md = data.markdown.clone().unwrap_or_default();
    assert!(md.trim().is_empty(), "unmatched css must be empty: {md:?}");
    assert_eq!(
        no_match_warning_count(&data),
        1,
        "warnings: {:?}",
        data.warnings
    );
}

// Issue #365: an Elementor product page rendered with duplicated responsive
// navigation used to come back as the whole unfiltered page — six copies of the
// menu, the footer, a popup form and stray ``` fences where nested lists were.
// Drives the full extract() path, not the individual helpers.
//
// The menus live INSIDE #main, so they reach markdown conversion whichever
// candidate the ladder picks — that is what makes this guard the real
// behaviour rather than readability's narrowing.
#[test]
fn elementor_page_with_duplicated_nav_extracts_cleanly() {
    let menu = r#"<div class="elementor-widget-wrap"><ul>
        <li><a href="/about/">Company overview and history</a>
          <ul><li><a href="/about/awards/">Awards and certifications page</a></li></ul>
        </li>
      </ul></div>"#;
    // The template ships the same menu three times (desktop, mobile, dropdown).
    let html = format!(
        r#"<html><head><title>30 RK PANORA</title></head><body>
        <div id="main" role="main">
          {menu}{menu}{menu}
          <div class="elementor-widget-wrap elementor-element-populated">
            <div class="elementor-element elementor-widget elementor-widget-woocommerce-product-title">
              <div class="elementor-widget-container">
                <h1>30 RK PANORA M 102 STP</h1>
              </div>
            </div>
            <div class="elementor-element elementor-widget elementor-widget-text-editor">
              <div class="elementor-widget-container">
                <p>Matt tiles offer a sophisticated, non-shiny surface finish
                   that suits a timeless and serene interior.</p>
                <p>Size</p><p>30x120CM</p><p>Surface</p><p>Porcelain Matt</p>
              </div>
            </div>
          </div>
          <aside class="widget_text">Subscribe to our newsletter today please</aside>
        </div>
        </body></html>"#
    );

    let data = crw_extract::extract(ExtractOptions {
        raw_html: &html,
        content_type: None,
        source_url: "https://example.com/product/30-rk-panora/",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 0,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Markdown],
        only_main_content: true,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    let md = data.markdown.unwrap_or_default();

    // The product content survives onlyMainContent — this is what the
    // over-broad "widget" class filter used to delete.
    assert!(md.contains("30 RK PANORA M 102 STP"), "title lost: {md}");
    assert!(md.contains("Porcelain Matt"), "spec lost: {md}");
    assert!(
        md.contains("Matt tiles offer a sophisticated"),
        "description lost: {md}"
    );

    // The nav is present exactly once, not three times. Exactly-one, so the
    // test fails both if dedup is removed and if the nav is dropped wholesale
    // for the wrong reason.
    assert_eq!(
        md.matches("Company overview and history").count(),
        1,
        "nav should appear exactly once: {md}"
    );
    assert_eq!(
        md.matches("Awards and certifications page").count(),
        1,
        "nested nav entry should appear exactly once: {md}"
    );

    // A widget area in page chrome is still boilerplate and must go.
    assert!(
        !md.contains("Subscribe to our newsletter today"),
        "widget area survived: {md}"
    );

    // A nested menu list must stay a list, not become a code block.
    assert!(!md.contains("```"), "spurious code fence: {md}");
    assert!(
        md.contains("[Awards and certifications page]"),
        "nested list link lost: {md}"
    );
}

// crw#530: a `text/plain` source (raw.githubusercontent.com and friends) is
// not HTML. Running it through the HTML-to-markdown converter anyway escaped
// every backtick (destroying fenced code blocks) and collapsed newlines into
// spaces (HTML's whitespace-collapse rule, merging paragraphs and code lines
// together) — corruption no response-side repair could undo. `content_type:
// Some("text/plain")` must return the body byte-for-byte.
#[test]
fn text_plain_source_markdown_is_byte_for_byte_passthrough() {
    let body = "## One-command install\n\n```bash\ncurl -fsSL https://fastcrw.com/install | sh\n```\n\nRuns local and free, no account needed.\n";
    let data = crw_extract::extract(ExtractOptions {
        raw_html: body,
        content_type: Some("text/plain"),
        source_url: "https://raw.githubusercontent.com/us/crw/main/README.md",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 0,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[
            OutputFormat::Markdown,
            OutputFormat::PlainText,
            OutputFormat::Html,
        ],
        only_main_content: true,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    assert_eq!(
        data.markdown.as_deref(),
        Some(body),
        "markdown must be byte-for-byte"
    );
    assert_eq!(
        data.plain_text.as_deref(),
        Some(body),
        "plain text must be byte-for-byte"
    );
    assert_eq!(
        data.html.as_deref(),
        Some(body),
        "html must be byte-for-byte"
    );
}

/// Page builders use `<header>` as a generic layout wrapper, so the candidate
/// ladder's structural rung has to be able to back out of its own pruning.
/// Without the survival floor this page extracts to nothing: every rung above
/// the raw document removes the `<header>` that holds the entire article.
#[test]
fn structural_rung_backs_out_when_it_would_delete_the_page() {
    let body = "Cog is a tool that lets you package machine learning models in a \
                reproducible container. It reads a configuration file, builds the \
                image, and gives the model a prediction interface over HTTP. \
                Packaging a model this way means the same weights and the same \
                preprocessing run identically on a laptop and on a GPU server, \
                which is the part teams usually get wrong when they hand a model \
                over to someone else to deploy.";
    let html = format!(
        "<html><head><title>Packaging models</title></head><body>\
         <header class=\"page-wrap\"><h1>Packaging models</h1>\
         <p>{body}</p><p>{body}</p><p>{body}</p></header>\
         <div class=\"legal\">© 2026</div></body></html>"
    );

    let data = crw_extract::extract(ExtractOptions {
        raw_html: &html,
        content_type: None,
        source_url: "https://example.com/packaging",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 0,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Markdown],
        only_main_content: true,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();

    let md = data.markdown.unwrap_or_default();
    assert!(
        md.contains("reproducible container"),
        "the article lives inside <header>; the structural rung must back out \
         rather than delete it. got {} chars:\n{md}",
        md.len()
    );
}

// A list page narrowed with include_tags is all links by design. The markdown
// must keep them as [text](url). The matched inline spans land on one markdown
// line, so a single title holding a boilerplate word ("filter") marked the whole
// candidate as boilerplate, the quality ladder swapped in the plaintext
// candidate, and every link was dropped.
#[test]
fn include_tags_list_page_keeps_links_in_markdown() {
    let rows: String = (1..=30)
        .map(|i| {
            let topic = if i == 7 { "filter" } else { "tool" };
            format!(
                "<tr><td><span class=\"titleline\"><a href=\"https://site{i}.example/post\">Story{i} explains how team{i} rebuilt project{i} with a new {topic}{i}</a><span class=\"sitebit comhead\"> (<a href=\"from?site=site{i}.example\"><span class=\"sitestr\">site{i}.example</span></a>)</span></span></td></tr>"
            )
        })
        .collect();
    let html = format!(
        "<html><head><title>List</title></head><body><table>{rows}</table><footer>Footer text</footer></body></html>"
    );
    let include = [".titleline".to_string()];
    let data = crw_extract::extract(ExtractOptions {
        raw_html: &html,
        content_type: None,
        source_url: "https://list.example/",
        status_code: 200,
        rendered_with: None,
        elapsed_ms: 100,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &[OutputFormat::Markdown],
        only_main_content: true,
        include_tags: &include,
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .unwrap();
    let md = data.markdown.unwrap_or_default();
    for i in [1, 7, 30] {
        let link = format!("](https://site{i}.example/post)");
        assert!(md.contains(&link), "missing {link} in {md:?}");
    }
    assert!(
        !md.contains("Footer text"),
        "include_tags must still narrow: {md:?}"
    );
}
