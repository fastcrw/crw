use lol_html::html_content::UserData;
use lol_html::{RewriteStrSettings, element, rewrite_str};

/// Marker for `<header>`/`<aside>` nested inside `<main>`/`<article>`, which
/// are article furniture rather than site chrome and must survive cleaning.
const KEEP_NESTED: u8 = 1;
use scraper::{Html, Selector};
use std::collections::HashSet;

/// How much boilerplate removal to apply beyond the always-unwanted tags.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CleanDepth {
    /// Scripts, styles, frames and data-URI images only. What a caller asking
    /// for the document as it is gets.
    Chrome,
    /// Chrome plus markup hidden by an inline style. The floor of the
    /// `onlyMainContent` ladder: the rungs above it drop hidden markup too, and
    /// the structural rung is judged by how much of this one it keeps, so both
    /// sides of that ratio have to be measured on the same visible page.
    Visible,
    /// Also drop structural chrome: `nav`, site-level `header`/`footer`/`aside`,
    /// `menu`, `select`. The class-name rules do NOT run.
    ///
    /// This is the ladder's middle rung. The class-name rules are the part that
    /// can mistake a theme's naming for chrome and take the body with it, so a
    /// page they over-prune needs somewhere to land that still is not the raw
    /// document. Defuddle's fallback has the same shape: loosen one filter
    /// category at a time and stay anchored, never fall back to the whole page.
    Structural,
    /// Everything, including the class and id rules. What `onlyMainContent`
    /// means.
    Full,
}

/// Clean HTML by stripping scripts, styles, and optionally non-content elements.
/// Then apply include_tags/exclude_tags via scraper.
///
/// Soft-failure warnings (e.g. `selector_no_match`) are discarded. Use
/// [`clean_html_with_warnings`] when the caller wants to surface them.
pub fn clean_html(
    html: &str,
    only_main_content: bool,
    include_tags: &[String],
    exclude_tags: &[String],
) -> Result<String, String> {
    clean_html_impl(
        html,
        depth_for(only_main_content),
        include_tags,
        exclude_tags,
        &mut Vec::new(),
    )
}

fn depth_for(only_main_content: bool) -> CleanDepth {
    if only_main_content {
        CleanDepth::Full
    } else {
        CleanDepth::Chrome
    }
}

/// Clean at an explicit depth. Used by the markdown candidate ladder to build a
/// rung between [`CleanDepth::Full`] and the raw document.
pub(crate) fn clean_html_at(
    html: &str,
    depth: CleanDepth,
    include_tags: &[String],
    exclude_tags: &[String],
    warnings: &mut Vec<String>,
) -> Result<String, String> {
    clean_html_impl(html, depth, include_tags, exclude_tags, warnings)
}

/// Like [`clean_html`], but collects soft-failure warnings into `warnings`.
/// Currently the only warning is `selector_no_match`, emitted when one or more
/// `include_tags` are supplied but none match any element.
pub fn clean_html_with_warnings(
    html: &str,
    only_main_content: bool,
    include_tags: &[String],
    exclude_tags: &[String],
    warnings: &mut Vec<String>,
) -> Result<String, String> {
    clean_html_impl(
        html,
        depth_for(only_main_content),
        include_tags,
        exclude_tags,
        warnings,
    )
}

fn clean_html_impl(
    html: &str,
    depth: CleanDepth,
    include_tags: &[String],
    exclude_tags: &[String],
    warnings: &mut Vec<String>,
) -> Result<String, String> {
    // Phase 1: lol_html streaming removal of always-unwanted tags.
    let mut handlers = vec![
        // <head> carries <title>/<meta>, which htmd would otherwise render as
        // a bare text line at the top of the markdown, duplicating the H1.
        element!("head", |el| {
            el.remove();
            Ok(())
        }),
        element!("script", |el| {
            el.remove();
            Ok(())
        }),
        element!("style", |el| {
            el.remove();
            Ok(())
        }),
        element!("noscript", |el| {
            el.remove();
            Ok(())
        }),
        element!("iframe", |el| {
            el.remove();
            Ok(())
        }),
        element!("svg", |el| {
            el.remove();
            Ok(())
        }),
        element!("canvas", |el| {
            el.remove();
            Ok(())
        }),
        // Remove images with data: URIs (base64 blobs bloat markdown output).
        element!("img", |el| {
            if let Some(src) = el.get_attribute("src")
                && src.starts_with("data:")
            {
                el.remove();
            }
            Ok(())
        }),
    ];

    if depth != CleanDepth::Chrome {
        // Markup the page hides with an inline style is not content: a reader
        // never sees it. Shopify themes ship `#BrowserWarning` with
        // `display: none` and a cart drawer under `visibility: hidden`, and both
        // read as body text once converted. Readability.js drops the same nodes
        // (`_isProbablyVisible`). The page skeleton is exempt: some sites hide
        // <body> or <main> until their scripts run, and that is the whole
        // document.
        //
        // The `hidden` attribute is left alone on purpose. React's streaming
        // SSR parks the real content in `<div hidden id="S:0">` and moves it into
        // place with a script, so a page fetched without running scripts keeps
        // its body there (kap.org.tr on the frozen set).
        handlers.push(element!("[style]", |el| {
            if !matches!(el.tag_name().as_str(), "html" | "body" | "main")
                && el.get_attribute("style").is_some_and(|s| hides(&s))
            {
                el.remove();
            }
            Ok(())
        }));
    }

    if matches!(depth, CleanDepth::Structural | CleanDepth::Full) {
        handlers.push(element!("nav", |el| {
            el.remove();
            Ok(())
        }));

        // `<header>` and `<aside>` mean two different things depending on where
        // they sit. Directly under <body> they are site chrome. Inside <main>
        // or <article> they are part of the article itself: the title block
        // that carries the H1 and the standfirst, or a pull-quote. Removing
        // them wholesale deleted 9.6% of what these two tags match across a
        // 400-page sample, including the lead paragraph of every Pantheon docs
        // page. The same split applies to `<footer>`: inside an article it is the
        // byline / date / tags block, not the site footer.
        //
        // lol_html streams, so a handler cannot look at its ancestors. Mark the
        // nested ones first (handlers run in registration order, and both fire
        // for the same element), then skip anything marked.
        handlers.push(element!(
            "main header, main aside, main footer, article header, article aside, article footer",
            |el| {
                el.set_user_data(KEEP_NESTED);
                Ok(())
            }
        ));
        handlers.push(element!("header", |el| {
            if el.user_data().downcast_ref::<u8>() != Some(&KEEP_NESTED) {
                el.remove();
            }
            Ok(())
        }));
        handlers.push(element!("footer", |el| {
            if el.user_data().downcast_ref::<u8>() != Some(&KEEP_NESTED) {
                el.remove();
            }
            Ok(())
        }));
        handlers.push(element!("aside", |el| {
            if el.user_data().downcast_ref::<u8>() != Some(&KEEP_NESTED) {
                el.remove();
            }
            Ok(())
        }));
        handlers.push(element!("menu", |el| {
            el.remove();
            Ok(())
        }));
        // Dropdown <select> elements are never publishable content.
        handlers.push(element!("select", |el| {
            el.remove();
            Ok(())
        }));
    }

    if depth == CleanDepth::Full {
        // Remove elements whose class or id matches common non-content patterns.
        // Covers sidebars, TOC, navigation, ads, related/recommended sections,
        // cookie banners, share widgets, and comment sections.
        //
        // IMPORTANT: Never remove structural elements (html, head, body) — that
        // would nuke the entire page. Also skip <main> since it typically
        // wraps the primary content we want to keep.
        handlers.push(element!("*", |el| {
            let tag = el.tag_name();
            let tag_name = tag.as_str();
            if matches!(tag_name, "html" | "head" | "body" | "main") {
                return Ok(());
            }

            let class = el.get_attribute("class").unwrap_or_default().to_lowercase();
            let id = el.get_attribute("id").unwrap_or_default().to_lowercase();

            // Check each CSS class token and the id individually.
            // Per-token substring matching avoids cross-token false positives
            // (e.g. "vector-toc-available skin-theme" wouldn't match "toc" on
            // a combined string that also contains the other classes).
            let combined = format!("{class} {id}");

            // Layout names, matched per class token — NEVER as a substring.
            // These describe where a region sits on the page, so themes reuse
            // them to name the wrapper that HOLDS the article:
            // `pds-sidebar-layout__content` (Pantheon docs), `has-sidebar`,
            // `content-sidebar`, `navigation-list-container`. Substring
            // matching on these emptied 14% of a 161-page labelled corpus
            // (django docs 74670 -> 248 chars). Firecrawl matches the same
            // concepts as CSS class selectors, i.e. per token, and does not
            // have this failure.
            //
            // Only names observed wrapping real content live here. Names like
            // "cookie" / "consent" / "infobox" stay in NOISE_PATTERNS below:
            // they appear almost exclusively as `cookie-notice` /
            // `cookielawinfo-*` style tokens, so requiring an exact token
            // would stop removing them entirely (measured: 459 elements,
            // ~57k chars of cookie chrome would leak back in).
            // Matched as a PREFIX of a class token (see reasoning above).
            const NOISE_LAYOUT_TOKENS: &[&str] = &[
                "sidebar",
                "navigation",
                "breadcrumb",
                "dropdown",
                "site-header",
                "site-footer",
                "page-header",
                "page-footer",
                "global-header",
                "global-footer",
                "global-nav",
                "main-nav",
                "primary-nav",
                "secondary-nav",
                // zhihu names the article body `copyrightrichtext-richtext`;
                // as a substring this deleted 98% of the page.
                "copyright",
            ];

            // Names here are matched as a SUBSTRING of "{class} {id}", so a name
            // that appears inside a content class deletes real content. Two were
            // replaced with narrower ones after measuring on the frozen scrape
            // corpus:
            //
            // - "widget" removed. Every WordPress page builder wraps page CONTENT
            //   in classes containing it: Elementor `elementor-widget` +
            //   `elementor-widget-{slug}`, SiteOrigin `so-widget-*` /
            //   `panel-widget-style` / `textwidget` / `widget_sow-editor`. It
            //   emptied those pages. `widget-area` / `widget_area` below still
            //   remove the registered sidebar container, which is where widgets
            //   that ARE boilerplate live; a bare `widget_*` block outside any
            //   container leaks, which is the precision side of the trade. Note a
            //   `widget_` PREFIX rule cannot work either: SiteOrigin's content
            //   widget is `widget_sow-editor`.
            // - "banner" removed. Hero banners carry the headline and the product
            //   copy. `role="banner"` below is the reliable chrome signal, and
            //   cookie / consent / promo cover the bars that matter. (Shopify and
            //   Squarespace announcement bars are named `announcement-bar` and
            //   were never caught by "banner" either way.)
            const NOISE_PATTERNS: &[&str] = &[
                "table-of-contents",
                "tableofcontents",
                "infobox",
                "navbox",
                "nav-box",
                "cookie",
                "consent",
                "widget-area",
                "widget_area",
                "disqus",
                "advert",
                "popup",
                "modal",
                "newsletter",
                "subscribe",
                "printfooter",
                "catlinks",
                "mw-panel",
                "mw-navigation",
                "sitesub",
                "jump-to-nav",
                "mw-editsection",
                "reflist",
                "mw-references",
                "authority-control",
                "mw-indicators",
                "sistersitebox",
                "mbox",
                "ambox",
                "ombox",
                "hatnote",
                "shortdescription",
                "sphinxsidebar",
                "sphinxfooter",
                "city-selector",
                "location-selector",
                "lang-selector",
                "language-selector",
                "skip-to",
                "skip-link",
                "skiplinks",
                "promo",
                "promotional",
                "social-share",
                "social-links",
                "social-icons",
                "follow-us",
                "site-map",
                "sitemap",
            ];

            // Patterns that need exact token matching (too short/generic for substring).
            // Checked against individual class names and the id value.
            const NOISE_EXACT_TOKENS: &[&str] = &[
                "toc",     // table of contents — "toc" but not "vector-toc-available"
                "share",   // share widgets — not "share-price" or "shareholder"
                "social",  // social buttons
                "related", // related content
                "recommended",
                "comment", // comment sections — not "uncommented"
                "footer",  // div.footer (e.g. Sphinx "Created using Sphinx")
            ];

            // Prefix patterns: match tokens that START with these strings.
            const NOISE_PREFIXES: &[&str] = &[
                "ad-", // ad containers — not "load-more", "typeahead"
                "ads-",
            ];

            // Layout names match a class token that STARTS with the name, never
            // one that merely contains it. Position carries the meaning:
            //
            //   sidebar-right, sidebar-card, dropdown-menu, breadcrumbs
            //       -> the element IS that piece of furniture. Remove.
            //   has-sidebar, no-sidebar, content-sidebar,
            //   pds-sidebar-layout__content (Pantheon docs)
            //       -> the element is the article, named after the furniture
            //          beside it. Keep.
            //
            // Plain substring matching could not tell those apart and emptied
            // the content of 14% of a 161-page labelled corpus (django docs
            // 74670 -> 248 chars). Requiring an exact token was the opposite
            // error: it let `sidebar-right` and `dropdown-menu` through.
            let is_noise = NOISE_PATTERNS.iter().any(|p| combined.contains(p)) || {
                let tokens_iter = class.split_whitespace().chain(std::iter::once(id.as_str()));
                tokens_iter.into_iter().any(|tok| {
                    NOISE_LAYOUT_TOKENS.iter().any(|p| tok.starts_with(p))
                        || NOISE_EXACT_TOKENS.contains(&tok)
                        || NOISE_PREFIXES.iter().any(|pre| tok.starts_with(pre))
                })
            };

            if is_noise {
                el.remove();
                return Ok(());
            }

            // Remove elements with ARIA landmark roles that indicate non-content areas.
            let role = el.get_attribute("role").unwrap_or_default().to_lowercase();
            if matches!(
                role.as_str(),
                "contentinfo" | "navigation" | "banner" | "complementary"
            ) {
                el.remove();
                return Ok(());
            }

            Ok(())
        }));
    }

    // lol_html 3 made RewriteStrSettings fields private; build via the
    // append builder. strict/enable_esi_tags keep their `true` defaults, same
    // as lol_html 2's Default.
    let settings = handlers
        .into_iter()
        .fold(RewriteStrSettings::new(), |s, h| {
            s.append_element_content_handler(h)
        });
    let mut result = rewrite_str(html, settings).map_err(|e| e.to_string())?;

    // Phase 2: If include_tags specified, only keep content matching those selectors.
    if !include_tags.is_empty() {
        result = keep_only_selectors(&result, include_tags, warnings);
    }

    // Phase 3: Apply exclude_tags — parse again and collect text/html without excluded.
    if !exclude_tags.is_empty() {
        result = remove_by_selectors(&result, exclude_tags);
    }

    Ok(result)
}

/// True when an inline `style` takes the element out of view.
fn hides(style: &str) -> bool {
    let style: String = style
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    style.split(';').any(|decl| {
        matches!(
            decl.trim_end_matches("!important"),
            "display:none" | "visibility:hidden"
        )
    })
}

/// Keep only the HTML of elements matching any of the given CSS selectors.
///
/// When none of the selectors match (or all are invalid), returns an empty
/// string and pushes `selector_no_match` into `warnings`. Previously this fell
/// back to the entire `html`, which silently dumped the full page into the
/// output — a footgun for LLM agents whose context fills up.
fn keep_only_selectors(html: &str, selectors: &[String], warnings: &mut Vec<String>) -> String {
    let doc = Html::parse_document(html);
    let mut parts = Vec::new();

    for sel_str in selectors {
        match Selector::parse(sel_str) {
            Ok(sel) => {
                for el in doc.select(&sel) {
                    parts.push(el.html());
                }
            }
            Err(e) => {
                tracing::warn!("Invalid CSS selector '{}': {:?}", sel_str, e);
            }
        }
    }

    if parts.is_empty() {
        warnings.push("selector_no_match".to_string());
        return String::new();
    }

    parts.join("\n")
}

/// Remove elements matching CSS selectors from the document.
/// Re-serializes the tree, skipping matched subtrees via tree node indices.
fn remove_by_selectors(html: &str, selectors: &[String]) -> String {
    let doc = Html::parse_document(html);

    // Collect pointers to matched elements for exclusion.
    // SAFETY: All pointers point into `doc` which lives for the entire function scope.
    // We only compare pointers (never dereference), so this is safe as long as `doc` is alive.
    let mut skip_ptrs: HashSet<*const scraper::node::Element> = HashSet::new();
    for sel_str in selectors {
        match Selector::parse(sel_str) {
            Ok(sel) => {
                for el in doc.select(&sel) {
                    skip_ptrs.insert(el.value() as *const _);
                }
            }
            Err(e) => {
                tracing::warn!("Invalid CSS selector '{}': {:?}", sel_str, e);
            }
        }
    }

    if skip_ptrs.is_empty() {
        return html.to_string();
    }

    // Re-serialize the root element, skipping excluded subtrees.
    // Pre-allocate output based on input size.
    let root = doc.root_element();
    let mut out = String::with_capacity(html.len());
    collect_excluding(&root, &skip_ptrs, &mut out);
    out
}

fn is_excluded(
    el: &scraper::ElementRef,
    skip_ptrs: &HashSet<*const scraper::node::Element>,
) -> bool {
    let ptr = el.value() as *const scraper::node::Element;
    skip_ptrs.contains(&ptr)
}

fn collect_excluding(
    element: &scraper::ElementRef,
    skip_ptrs: &HashSet<*const scraper::node::Element>,
    out: &mut String,
) {
    if is_excluded(element, skip_ptrs) {
        return;
    }

    let el = element.value();
    out.push('<');
    out.push_str(&el.name.local);
    for (name, value) in el.attrs() {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        out.push_str(&value.replace('"', "&quot;"));
        out.push('"');
    }
    out.push('>');

    for child in element.children() {
        match child.value() {
            scraper::node::Node::Text(text) => {
                out.push_str(text);
            }
            scraper::node::Node::Element(_) => {
                if let Some(child_el) = scraper::ElementRef::wrap(child) {
                    collect_excluding(&child_el, skip_ptrs, out);
                }
            }
            _ => {}
        }
    }

    let self_closing = matches!(
        &*el.name.local,
        "br" | "hr"
            | "img"
            | "input"
            | "meta"
            | "link"
            | "area"
            | "base"
            | "col"
            | "embed"
            | "source"
            | "track"
            | "wbr"
    );
    if !self_closing {
        out.push_str("</");
        out.push_str(&el.name.local);
        out.push('>');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_scripts_and_styles() {
        let html =
            r#"<html><body><script>alert(1)</script><p>Hello</p><style>x{}</style></body></html>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(!result.contains("<script>"));
        assert!(!result.contains("<style>"));
        assert!(result.contains("Hello"));
    }

    #[test]
    fn strips_nav_footer_in_main_content_mode() {
        let html = r#"<body><nav>Menu</nav><article>Content</article><footer>Foot</footer></body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(!result.contains("Menu"));
        assert!(!result.contains("Foot"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn page_builder_content_survives_only_main_content() {
        // Every page builder wraps page CONTENT in classes containing "widget":
        // Elementor tags each widget with both `elementor-widget` and
        // `elementor-widget-{slug}`, SiteOrigin uses `so-widget-*`,
        // `panel-widget-style` and `textwidget`. Matching "widget" by name
        // emptied all of those pages (issue #365).
        let html = r#"<body>
            <div class="elementor-element elementor-widget elementor-widget-woocommerce-product-title">
              <h1>30 RK PANORA M 102 STP</h1>
            </div>
            <div class="so-panel widget widget_sow-editor panel-first-child">
              <div class="so-widget-sow-editor so-widget-sow-editor-base">
                <div class="siteorigin-widget-tinymce textwidget">
                  <p>Our label and carton teams are ready to help.</p>
                </div>
              </div>
            </div>
        </body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(result.contains("30 RK PANORA M 102 STP"), "got: {result}");
        assert!(
            result.contains("Our label and carton teams are ready to help."),
            "got: {result}"
        );
    }

    #[test]
    fn widget_areas_in_page_chrome_are_still_removed() {
        // Widget areas are boilerplate, but they are recognised by WHERE they
        // sit — inside semantic chrome or a sidebar container — not by having
        // "widget" in the class name.
        let html = r#"<body>
            <aside class="widget_text"><p>Sidebar promo copy</p></aside>
            <footer class="footer-widgets"><p>GeneratePress footer</p></footer>
            <header class="ast-header-widget-area"><p>Astra header</p></header>
            <div class="sidebar"><div class="widget_recent_posts">Recent posts</div></div>
            <article><p>The actual article body.</p></article>
        </body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(result.contains("The actual article body."), "got: {result}");
        assert!(!result.contains("Sidebar promo copy"), "got: {result}");
        assert!(!result.contains("GeneratePress footer"), "got: {result}");
        assert!(!result.contains("Astra header"), "got: {result}");
        assert!(!result.contains("Recent posts"), "got: {result}");
    }

    #[test]
    fn registered_widget_area_is_removed_but_builder_widgets_are_not() {
        // The narrow replacement for the deleted `widget` substring: the sidebar
        // container a theme registers its widgets in, without relying on a
        // semantic tag, and without touching page-builder content classes.
        let html = r#"<body>
            <div class="widget-area"><div class="widget_text">Sidebar promo copy</div></div>
            <div class="footer-widget_area"><p>Footer widget copy</p></div>
            <div class="elementor-widget elementor-widget-text-editor">
              <p>Real page content from a builder widget.</p>
            </div>
        </body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(
            result.contains("Real page content from a builder widget."),
            "got: {result}"
        );
        assert!(!result.contains("Sidebar promo copy"), "got: {result}");
        assert!(!result.contains("Footer widget copy"), "got: {result}");
        assert!(
            !result.contains("Free shipping this week only"),
            "got: {result}"
        );
    }

    #[test]
    fn hero_banner_copy_survives_but_role_banner_does_not() {
        // "banner" as a class name is where hero headlines live; `role="banner"`
        // is the reliable signal for site chrome.
        let html = r#"<body>
            <div class="banner banner--product"><h1>KONI 2822 Race Damper</h1>
              <p>The 2822 MKII Series is the latest offering from KONI.</p></div>
            <div role="banner"><p>Site wide announcement bar</p></div>
        </body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(result.contains("KONI 2822 Race Damper"), "got: {result}");
        assert!(
            result.contains("The 2822 MKII Series is the latest offering from KONI."),
            "got: {result}"
        );
        assert!(
            !result.contains("Site wide announcement bar"),
            "got: {result}"
        );
    }

    #[test]
    fn exclude_tags_removes_matching_elements() {
        let html = r#"<body><div class="ad">Ad stuff</div><p>Real content</p></body>"#;
        let result = clean_html(html, false, &[], &["div.ad".into()]).unwrap();
        assert!(!result.contains("Ad stuff"));
        assert!(result.contains("Real content"));
    }

    #[test]
    fn does_not_remove_html_body_with_noise_classes() {
        // Wikipedia's <html> has classes like "vector-toc-available" containing "toc".
        // The noise handler must skip structural elements to avoid nuking the page.
        let html = r#"<html class="vector-toc-available"><body><main class="mw-body"><p>Content</p></main></body></html>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(
            result.contains("Content"),
            "Structural elements must not be removed by noise patterns"
        );
    }

    #[test]
    fn strips_role_contentinfo_in_main_content_mode() {
        let html = r#"<body><div role="contentinfo">Copyright 2024</div><p>Content</p><div role="navigation">Nav</div></body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(!result.contains("Copyright"));
        assert!(!result.contains("Nav"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn strips_sphinx_patterns_in_main_content_mode() {
        let html = r#"<body><div class="sphinxsidebar">Sidebar</div><p>Content</p><div class="copyright">Copyright</div></body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(!result.contains("Sidebar"));
        assert!(!result.contains("Copyright"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn include_tags_keeps_only_matching() {
        let html =
            r#"<body><nav>Nav</nav><article><p>Article</p></article><footer>Foot</footer></body>"#;
        let result = clean_html(html, false, &["article".into()], &[]).unwrap();
        assert!(result.contains("Article"));
        assert!(!result.contains("Nav"));
        assert!(!result.contains("Foot"));
    }

    #[test]
    fn include_tags_no_match_returns_empty_and_warns() {
        let html =
            r#"<body><nav>Nav</nav><article><p>Article</p></article><footer>Foot</footer></body>"#;
        let mut warnings = Vec::new();
        let result =
            clean_html_with_warnings(html, false, &[".does-not-exist".into()], &[], &mut warnings)
                .unwrap();
        assert!(
            result.trim().is_empty(),
            "a non-matching include_tags selector must not fall back to the whole page"
        );
        assert!(
            warnings.iter().any(|w| w == "selector_no_match"),
            "expected selector_no_match warning, got {warnings:?}"
        );
    }

    // ── always-stripped tags (regardless of only_main_content) ──────────────

    #[test]
    fn strips_noscript_always() {
        let html = r#"<body><noscript>Enable JS</noscript><p>Content</p></body>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(!result.contains("Enable JS"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn hidden_markup_is_dropped_in_main_content_mode() {
        // Shape of the jbhifi collection page from #660.
        let html = r#"<body><main><h1>50-55 Inch Televisions</h1>
            <p class="unsupported-browser" id="BrowserWarning" style="display: none;">Your browser version is unsupported.</p>
            <div role="button" style="z-index: 1001; visibility:hidden !important"><h3>My Cart</h3><p>Your cart is empty</p></div>
            <p style="display: block">Hisense 55 inch TV</p></main></body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(!result.contains("browser version"));
        assert!(!result.contains("Your cart is empty"));
        assert!(result.contains("50-55 Inch Televisions"));
        assert!(result.contains("Hisense 55 inch TV"));
    }

    #[test]
    fn hidden_markup_is_kept_when_only_main_content_is_false() {
        let html = r#"<body><p style="display:none">Hidden note</p><p>Content</p></body>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(result.contains("Hidden note"));
    }

    #[test]
    fn hidden_page_skeleton_and_hidden_attribute_content_survive() {
        // Sites that hide the document until their scripts run still carry the
        // content, and React streaming SSR parks the real body in
        // `<div hidden id="S:0">` until a script moves it into place.
        let html = r#"<html style="visibility:hidden"><body style="display:none"><main style="display:none">
            <p>Article body</p><div hidden id="S:0">Fund summary table</div></main></body></html>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(result.contains("Article body"));
        assert!(result.contains("Fund summary table"));
    }

    #[test]
    fn visible_depth_drops_hidden_markup_but_keeps_structure() {
        let html =
            r#"<body><nav>Menu</nav><p style="display:none">Hidden note</p><p>Content</p></body>"#;
        let result = clean_html_at(html, CleanDepth::Visible, &[], &[], &mut Vec::new()).unwrap();
        assert!(!result.contains("Hidden note"));
        assert!(result.contains("Menu"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn style_without_a_hiding_declaration_is_kept() {
        for style in [
            "display: flex",
            "visibility: visible",
            "max-height: 0; overflow: hidden",
            "--display:none-ish: 1",
        ] {
            assert!(!hides(style), "{style}");
        }
        for style in [
            "DISPLAY:NONE",
            "color:red; display : none ;",
            "visibility:hidden!important",
        ] {
            assert!(hides(style), "{style}");
        }
    }

    #[test]
    fn strips_iframe_always() {
        let html = r#"<body><iframe src="https://ads.example/x"></iframe><p>Content</p></body>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(!result.contains("iframe"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn strips_svg_always() {
        let html = r#"<body><svg><circle r="5"/></svg><p>Content</p></body>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(!result.contains("circle"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn strips_canvas_always() {
        let html = r#"<body><canvas id="chart"></canvas><p>Content</p></body>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(!result.contains("canvas"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn strips_head_and_its_title_always() {
        let html = r#"<html><head><title>Page Title</title><meta charset="utf-8"></head><body><p>Content</p></body></html>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(!result.contains("Page Title"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn removes_data_uri_images_but_keeps_normal_images() {
        let html = r#"<body>
            <img src="data:image/png;base64,iVBORw0KGgo=" alt="blob">
            <img src="https://example.com/real.png" alt="real">
            <p>Content</p>
        </body>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(!result.contains("base64"));
        assert!(result.contains("real.png"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn img_without_src_attribute_is_kept() {
        let html = r#"<body><img alt="no src"><p>Content</p></body>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(result.contains("no src"));
    }

    // ── only_main_content=false: chrome tags are preserved ──────────────────

    #[test]
    fn nav_is_kept_when_only_main_content_is_false() {
        let html = r#"<body><nav>Menu</nav><article>Content</article></body>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(result.contains("Menu"));
    }

    #[test]
    fn top_level_header_footer_aside_are_kept_when_only_main_content_is_false() {
        let html = r#"<body><header>Head</header><aside>Side</aside><p>Content</p><footer>Foot</footer></body>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(result.contains("Head"));
        assert!(result.contains("Side"));
        assert!(result.contains("Foot"));
    }

    #[test]
    fn sidebar_class_is_kept_when_only_main_content_is_false() {
        // The noise-pattern handler is only registered in only_main_content mode.
        let html = r#"<body><div class="sidebar">Side content</div><p>Content</p></body>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(result.contains("Side content"));
    }

    #[test]
    fn select_and_menu_removed_only_in_main_content_mode() {
        let html = r#"<body><select><option>A</option></select><menu><li>x</li></menu><p>Content</p></body>"#;
        let kept = clean_html(html, false, &[], &[]).unwrap();
        assert!(kept.contains("option"));
        let stripped = clean_html(html, true, &[], &[]).unwrap();
        assert!(!stripped.contains("option"));
        assert!(!stripped.contains("<menu"));
        assert!(stripped.contains("Content"));
    }

    // ── nested header/aside/footer inside <main>/<article> survive ─────────

    #[test]
    fn header_nested_in_main_survives_only_main_content() {
        let html = r#"<body><main><header><h1>Title block</h1></header><p>Body</p></main></body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(result.contains("Title block"), "got: {result}");
        assert!(result.contains("Body"));
    }

    #[test]
    fn aside_nested_in_article_survives_as_a_pull_quote() {
        let html = r#"<body><article><aside>Pull quote</aside><p>Body</p></article></body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(result.contains("Pull quote"), "got: {result}");
    }

    #[test]
    fn footer_nested_in_article_survives_as_byline() {
        let html =
            r#"<body><article><p>Body</p><footer>By Jane Doe, 2026</footer></article></body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(result.contains("By Jane Doe"), "got: {result}");
    }

    #[test]
    fn top_level_header_outside_main_is_still_removed() {
        // Only header/aside/footer NESTED inside main/article are spared; a
        // sibling header at the body level is still site chrome.
        let html = r#"<body><header>Site chrome</header><main><header>Title block</header><p>Body</p></main></body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(!result.contains("Site chrome"), "got: {result}");
        assert!(result.contains("Title block"));
    }

    // ── aria roles ────────────────────────────────────────────────────────

    #[test]
    fn strips_role_complementary_in_main_content_mode() {
        let html = r#"<body><div role="complementary">Related links</div><p>Content</p></body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(!result.contains("Related links"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn aria_roles_are_kept_when_only_main_content_is_false() {
        let html = r#"<body><div role="navigation">Nav role text</div><p>Content</p></body>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(result.contains("Nav role text"));
    }

    // ── noise pattern precision (layout-token prefix vs substring) ─────────

    #[test]
    fn sidebar_right_is_removed_but_has_sidebar_content_wrapper_is_kept() {
        let html = r#"<body>
            <div class="sidebar-right">Boilerplate</div>
            <div class="has-sidebar"><p>Real article content</p></div>
        </body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(!result.contains("Boilerplate"), "got: {result}");
        assert!(result.contains("Real article content"), "got: {result}");
    }

    #[test]
    fn pantheon_docs_content_sidebar_layout_wrapper_survives() {
        let html =
            r#"<body><div class="pds-sidebar-layout__content"><p>Docs body text</p></div></body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(result.contains("Docs body text"), "got: {result}");
    }

    #[test]
    fn copyright_prefix_token_also_removes_the_zhihu_content_wrapper() {
        // BUG: the comment above `NOISE_LAYOUT_TOKENS` ("copyright") claims this
        // token was moved to the *prefix*-matched list specifically so that
        // zhihu's `copyrightrichtext-richtext` article-body class would survive
        // (as opposed to a plain substring match, which the comment says
        // "deleted 98% of the page"). But `tok.starts_with("copyright")` is
        // still true for `copyrightrichtext-richtext` (it is a single
        // whitespace-delimited class token), so the wrapper is removed anyway
        // and the stated fix does not actually protect this case. Asserting
        // the current (unintended) behaviour here per the "don't fix
        // production code" rule; the comment's rationale and the code's
        // actual effect disagree.
        let html = r#"<body>
            <div class="copyrightrichtext-richtext"><p>Zhihu article body.</p></div>
            <div class="copyright">Page copyright footer.</div>
        </body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(!result.contains("Zhihu article body."), "got: {result}");
        assert!(!result.contains("Page copyright footer."), "got: {result}");
    }

    #[test]
    fn ad_prefixed_class_is_removed_but_unrelated_ad_word_is_kept() {
        let html = r#"<body>
            <div class="ad-banner">Sponsored</div>
            <div class="adjacent-content">Real content next to the ad</div>
        </body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(!result.contains("Sponsored"), "got: {result}");
        assert!(
            result.contains("Real content next to the ad"),
            "got: {result}"
        );
    }

    #[test]
    fn exact_token_toc_share_social_related_recommended_comment_footer_are_removed() {
        let html = r#"<body>
            <div class="toc">Table of contents</div>
            <div class="share">Share buttons</div>
            <div class="social">Social buttons</div>
            <div class="related">Related posts</div>
            <div class="recommended">Recommended posts</div>
            <div class="comment">Comment section</div>
            <div class="footer">Div footer</div>
            <p>Real content</p>
        </body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        for gone in [
            "Table of contents",
            "Share buttons",
            "Social buttons",
            "Related posts",
            "Recommended posts",
            "Comment section",
            "Div footer",
        ] {
            assert!(
                !result.contains(gone),
                "expected {gone} removed, got: {result}"
            );
        }
        assert!(result.contains("Real content"));
    }

    #[test]
    fn exact_token_does_not_match_a_class_that_merely_contains_it() {
        // "toc" as an exact token must not fire on "vector-toc-available".
        let html = r#"<body><div class="vector-toc-available">Content</div></body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(result.contains("Content"), "got: {result}");
    }

    #[test]
    fn share_price_and_shareholder_classes_are_not_removed_by_exact_share_token() {
        let html = r#"<body>
            <div class="share-price">$19.99</div>
            <div class="shareholder-info">Info</div>
        </body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(result.contains("$19.99"), "got: {result}");
        assert!(result.contains("Info"), "got: {result}");
    }

    #[test]
    fn uncommented_class_is_not_removed_by_exact_comment_token() {
        let html = r#"<body><div class="uncommented">Text</div></body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(result.contains("Text"));
    }

    // ── structural safety ─────────────────────────────────────────────────

    #[test]
    fn structural_elements_html_head_body_main_are_never_removed_by_noise_matching() {
        let html = r#"<html class="ad-theme"><head></head><body class="sidebar-layout"><main class="related-widget"><p>Content</p></main></body></html>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(result.contains("Content"), "got: {result}");
    }

    // ── malformed / truncated / empty / deeply nested HTML ──────────────────

    #[test]
    fn empty_document_does_not_panic() {
        let result = clean_html("", true, &[], &[]).unwrap();
        assert!(result.trim().is_empty());
    }

    #[test]
    fn whitespace_only_document_does_not_panic() {
        let result = clean_html("   \n\t  ", true, &[], &[]).unwrap();
        assert!(!result.contains('<'));
    }

    #[test]
    fn unclosed_tags_do_not_panic_and_content_survives() {
        let html = r#"<body><div><p>Unclosed paragraph<div>Nested unclosed"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(result.contains("Unclosed paragraph"));
        assert!(result.contains("Nested unclosed"));
    }

    #[test]
    fn truncated_mid_attribute_does_not_panic() {
        let html = r#"<body><div class="incomp"#;
        let result = clean_html(html, false, &[], &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn mismatched_closing_tags_do_not_panic() {
        let html = r#"<body><p>Text</div></span></body>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(result.contains("Text"));
    }

    #[test]
    fn deeply_nested_divs_do_not_overflow_and_preserve_innermost_text() {
        let depth = 300;
        let mut html = String::from("<body>");
        for _ in 0..depth {
            html.push_str("<div>");
        }
        html.push_str("deep content");
        for _ in 0..depth {
            html.push_str("</div>");
        }
        html.push_str("</body>");
        let result = clean_html(&html, false, &[], &[]).unwrap();
        assert!(result.contains("deep content"));
    }

    #[test]
    fn deeply_nested_divs_with_exclude_tags_selector_does_not_overflow() {
        // Exercises the recursive `collect_excluding` path at depth.
        let depth = 300;
        let mut html = String::from(r#"<body><div id="keep">"#);
        for _ in 0..depth {
            html.push_str("<div>");
        }
        html.push_str("deep content");
        for _ in 0..depth {
            html.push_str("</div>");
        }
        html.push_str(r#"</div><div class="drop">gone</div></body>"#);
        let result = clean_html(&html, false, &[], &["div.drop".into()]).unwrap();
        assert!(result.contains("deep content"));
        assert!(!result.contains("gone"));
    }

    // ── unicode / emoji content ──────────────────────────────────────────

    #[test]
    fn unicode_and_emoji_content_survives_cleaning() {
        let html = r#"<body><nav>Menu</nav><article><p>日本語のコンテンツ 😀 très bien</p></article></body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(result.contains("日本語のコンテンツ"));
        assert!(result.contains("😀"));
        assert!(result.contains("très bien"));
        assert!(!result.contains("Menu"));
    }

    #[test]
    fn unicode_class_names_are_handled_without_panic() {
        let html = r#"<body><div class="広告">Ad-like unicode class</div><p>Content</p></body>"#;
        let result = clean_html(html, true, &[], &[]).unwrap();
        assert!(result.contains("Content"));
    }

    // ── include_tags / exclude_tags combinations ─────────────────────────

    #[test]
    fn include_tags_joins_multiple_matches_with_a_newline() {
        let html = r#"<body><p class="a">First</p><p class="b">Second</p></body>"#;
        let result = clean_html(html, false, &[".a".into(), ".b".into()], &[]).unwrap();
        assert!(result.contains("First"));
        assert!(result.contains("Second"));
        assert!(result.contains('\n'));
    }

    #[test]
    fn include_tags_invalid_selector_mixed_with_valid_one_still_matches() {
        let html = r#"<body><p class="a">Kept</p></body>"#;
        let mut warnings = Vec::new();
        let result = clean_html_with_warnings(
            html,
            false,
            &["[[[invalid".into(), ".a".into()],
            &[],
            &mut warnings,
        )
        .unwrap();
        assert!(result.contains("Kept"));
        assert!(warnings.is_empty(), "a valid match must not warn");
    }

    #[test]
    fn include_tags_all_invalid_selectors_returns_empty_and_warns() {
        let html = r#"<body><p>Content</p></body>"#;
        let mut warnings = Vec::new();
        let result =
            clean_html_with_warnings(html, false, &["[[[invalid".into()], &[], &mut warnings)
                .unwrap();
        assert!(result.trim().is_empty());
        assert!(warnings.iter().any(|w| w == "selector_no_match"));
    }

    #[test]
    fn exclude_tags_with_an_invalid_selector_is_a_no_op_for_that_selector() {
        let html = r#"<body><div class="ad">Ad</div><p>Content</p></body>"#;
        let result = clean_html(html, false, &[], &["[[[invalid".into()]).unwrap();
        assert!(result.contains("Ad"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn exclude_tags_empty_list_leaves_document_unchanged_by_phase3() {
        let html = r#"<body><div class="ad">Ad</div></body>"#;
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(result.contains("Ad"));
    }

    #[test]
    fn include_then_exclude_applies_exclude_within_the_included_subset() {
        let html = r#"<body>
            <article><p class="ad">Ad inside article</p><p>Real text</p></article>
            <nav>Nav</nav>
        </body>"#;
        let result = clean_html(html, false, &["article".into()], &["p.ad".into()]).unwrap();
        assert!(!result.contains("Nav"));
        assert!(!result.contains("Ad inside article"));
        assert!(result.contains("Real text"));
    }

    #[test]
    fn exclude_tags_removes_a_whole_subtree_not_just_the_matched_element() {
        let html = r#"<body><div class="ad"><span>Nested</span><b>Also nested</b></div><p>Content</p></body>"#;
        let result = clean_html(html, false, &[], &["div.ad".into()]).unwrap();
        assert!(!result.contains("Nested"));
        assert!(!result.contains("Also nested"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn exclude_tags_by_id_selector() {
        let html = r#"<body><div id="cookie-banner">Accept cookies</div><p>Content</p></body>"#;
        let result = clean_html(html, false, &[], &["#cookie-banner".into()]).unwrap();
        assert!(!result.contains("Accept cookies"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn exclude_tags_preserves_self_closing_tags_without_a_closing_tag() {
        let html = r#"<body><p>Line<br>break</p><hr><img src="x.png"><p>Content</p></body>"#;
        let result = clean_html(html, false, &[], &["p.does-not-exist".into()]).unwrap();
        assert!(!result.contains("</br>"));
        assert!(!result.contains("</hr>"));
        assert!(!result.contains("</img>"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn exclude_tags_escapes_double_quotes_in_attribute_values() {
        let html = r#"<body><div title="say &quot;hi&quot;" class="keep">Content</div></body>"#;
        let result = clean_html(html, false, &[], &["p.does-not-exist".into()]).unwrap();
        assert!(result.contains("&quot;hi&quot;") || result.contains("say"));
        assert!(result.contains("Content"));
    }

    #[test]
    fn include_tags_selector_matching_nothing_among_several_still_returns_the_other_matches() {
        let html = r#"<body><p class="a">Kept</p></body>"#;
        let mut warnings = Vec::new();
        let result = clean_html_with_warnings(
            html,
            false,
            &[".does-not-exist".into(), ".a".into()],
            &[],
            &mut warnings,
        )
        .unwrap();
        assert!(result.contains("Kept"));
        assert!(
            warnings.is_empty(),
            "at least one selector matched, so no selector_no_match warning"
        );
    }

    // ── whitespace ────────────────────────────────────────────────────────

    #[test]
    fn clean_html_does_not_collapse_internal_whitespace_itself() {
        // clean_html operates on markup, not text normalization; it must not
        // silently mangle whitespace inside a text node.
        let html = "<body><p>a   b</p></body>";
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(result.contains("a   b"), "got: {result}");
    }

    #[test]
    fn leading_and_trailing_document_whitespace_is_preserved_around_content() {
        let html = "\n\n<body><p>Content</p></body>\n\n";
        let result = clean_html(html, false, &[], &[]).unwrap();
        assert!(result.contains("Content"));
    }
}
