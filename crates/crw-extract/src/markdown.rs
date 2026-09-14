use regex::Regex;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::LazyLock;

/// Convert HTML to Markdown using htmd (turndown.js-inspired converter).
///
/// Thin wrapper over [`html_to_markdown_with`] with `normalize = false` ,
/// byte-identical to this function's historical behavior. Existing callers
/// never need to change to pick up table normalization.
pub fn html_to_markdown(html: &str) -> String {
    html_to_markdown_with(html, false)
}

/// Convert HTML to Markdown, optionally normalizing `<table>` elements first.
///
/// `normalize = false` is byte-identical to [`html_to_markdown`] (the revert
/// guarantee for `ExtractionConfig::normalize_tables`). `normalize = true`
/// expands `rowspan`/`colspan` into a flat grid (`crate::table_normalize`) and
/// uses the table-safe indented-code pass so a table nested inside a `<li>`
/// (indented 4 spaces by htmd) isn't swallowed into a fenced code block.
pub fn html_to_markdown_with(html: &str, normalize: bool) -> String {
    let html = if normalize {
        crate::table_normalize::normalize_tables(html)
    } else {
        html.to_string()
    };
    let md = htmd::convert(&html).unwrap_or_default();
    let md = strip_anchor_artifacts(&md);
    let md = strip_data_uris(&md);
    if normalize {
        convert_indented_code_to_fenced_table_safe(&md)
    } else {
        convert_indented_code_to_fenced(&md)
    }
}

/// One markdown link or image.
static LINK_OR_IMAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!?\[[^\]]*\]\([^)]*\)").unwrap());

/// What must remain once the links are removed from a menu entry: exactly one
/// bullet or ordered-list marker, and whitespace. A `#`, a `|` or any prose
/// means the line carries content of its own, and an empty residue means a
/// standalone link, which can just as easily be a paragraph.
static NAV_RESIDUE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*([*+\-]|\d+[.)])\s*$").unwrap());

/// Shortest line that may be dropped as a repeat.
const MIN_DEDUP_LINE_LEN: usize = 30;

/// How many repeated entries must sit together before the run counts as a
/// repeated menu rather than a coincidence.
const MIN_REPEATED_RUN: usize = 2;

/// Drop verbatim repeats of navigation lines.
///
/// Responsive templates ship a mobile and a desktop copy of the same nav, and
/// mega-menus repeat their whole link list in every dropdown, so one menu entry
/// can appear six or more times. Only lines that are **nothing but** links plus
/// a list marker qualify, which is the shape a menu entry takes; a heading, a
/// table row, a paragraph or a line of code is never touched, however often it
/// repeats. The first occurrence always survives.
///
/// Applied only when the caller asked for main content, with
/// `onlyMainContent: false` they asked for the document as it is.
pub fn drop_repeated_nav_lines(md: &str) -> String {
    let lines: Vec<&str> = md.lines().collect();

    // Pass 1: find the menu-shaped lines, note where each text first appeared,
    // and mark the later occurrences.
    let mut first_at: HashMap<&str, usize> = HashMap::new();
    let mut shaped = vec![false; lines.len()];
    let mut repeat = vec![false; lines.len()];
    let mut in_fenced = false;
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("```") {
            in_fenced = !in_fenced;
            continue;
        }
        if in_fenced || line.trim().len() < MIN_DEDUP_LINE_LEN {
            continue;
        }
        let residue = LINK_OR_IMAGE_RE.replace_all(line, "");
        // `replace_all` returning Borrowed means nothing matched: the line holds
        // no link at all, so it is not a menu entry however much it looks like
        // one (a `-----` rule leaves the same residue).
        let has_link = matches!(residue, std::borrow::Cow::Owned(_));
        if !has_link || !NAV_RESIDUE_RE.is_match(&residue) {
            continue;
        }
        shaped[i] = true;
        // Keyed on the untrimmed line, so a nested entry and a top-level one
        // stay distinct.
        match first_at.entry(line) {
            Entry::Occupied(_) => repeat[i] = true,
            Entry::Vacant(slot) => {
                slot.insert(i);
            }
        }
    }

    // The next menu-shaped line, looking past blanks only. A line of prose ends
    // the block.
    let next_shaped = |from: usize| {
        lines
            .iter()
            .enumerate()
            .skip(from + 1)
            .find(|(j, l)| shaped[*j] || !l.trim().is_empty())
            .filter(|(j, _)| shaped[*j])
            .map(|(j, _)| j)
    };

    // Pass 2: drop a repeat only where the whole block genuinely repeated ,
    // at least two entries in a row, in the same order they first appeared.
    // A lone repeat, or two entries that merely happen to sit together now, is
    // a real cross-listing (the same item catalogued under two categories) and
    // is kept.
    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if !repeat[i] {
            kept.push(lines[i]);
            i += 1;
            continue;
        }
        let mut end = i;
        let mut run: Vec<usize> = Vec::new();
        while end < lines.len() && (repeat[end] || lines[end].trim().is_empty()) {
            if repeat[end] {
                run.push(end);
            }
            end += 1;
        }
        // Trailing blanks belong to whatever follows, not to the run.
        while end > i && lines[end - 1].trim().is_empty() {
            end -= 1;
        }
        let ordered_as_first_seen = run.windows(2).all(|w| {
            first_at
                .get(lines[w[0]])
                .and_then(|&at| next_shaped(at))
                .is_some_and(|j| lines[j] == lines[w[1]])
        });
        if run.len() < MIN_REPEATED_RUN || !ordered_as_first_seen {
            kept.extend_from_slice(&lines[i..end]);
        }
        i = end;
    }

    let mut out = kept.join("\n");
    if md.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// A markdown list item at any indent depth: `* x`, `    - x`, `\t1. x`.
/// htmd indents nested `<ul>`/`<ol>` items by exactly 4 spaces, which is also
/// the indented-code-block marker, so nested menus, sitemaps and multi-level
/// footers must not be mistaken for code.
static LIST_ITEM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[ \t]*([*+\-]|\d+[.)])\s").unwrap());

/// Convert 4-space indented code blocks to fenced (```) code blocks.
/// Fenced blocks are unambiguous and easier for RAG pipelines to parse.
fn convert_indented_code_to_fenced(md: &str) -> String {
    convert_indented_code_to_fenced_impl(md, false)
}

/// Table-safe variant of [`convert_indented_code_to_fenced`]: a 4-space
/// indented line that is a GFM pipe-table row or separator (`| --- |`) is
/// dedented and passed through as-is instead of being folded into a fenced
/// code block. htmd indents block content inside a `<li>` by exactly 4 spaces
/// (`ul_bullet_spacing: 3` + 1), so a table normalized inside a list item
/// would otherwise come out as a fenced block full of literal pipe text.
fn convert_indented_code_to_fenced_table_safe(md: &str) -> String {
    convert_indented_code_to_fenced_impl(md, true)
}

fn convert_indented_code_to_fenced_impl(md: &str, table_safe: bool) -> String {
    let mut result = String::with_capacity(md.len());
    let mut code_lines: Vec<&str> = Vec::new();
    let mut in_fenced = false;
    // Whether the last non-blank line before the current indented run was a
    // list item. Only then can the run be list continuation rather than code.
    let mut after_list_item = false;
    let mut run_continues_list = false;

    let lines: Vec<&str> = md.lines().collect();
    let table_lines = if table_safe {
        indented_table_line_flags(&lines)
    } else {
        vec![false; lines.len()]
    };

    for (idx, line) in lines.iter().copied().enumerate() {
        // Track existing fenced code blocks to avoid double-fencing.
        if line.trim_start().starts_with("```") {
            in_fenced = !in_fenced;
            if !code_lines.is_empty() {
                flush_code_block(&mut result, &mut code_lines, run_continues_list);
            }
            after_list_item = false;
            result.push_str(line);
            result.push('\n');
            continue;
        }

        if in_fenced {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        let is_code_indent = line.starts_with("    ") || line.starts_with('\t');
        let is_blank = line.trim().is_empty();

        if is_code_indent {
            if table_lines[idx] {
                if !code_lines.is_empty() {
                    flush_code_block(&mut result, &mut code_lines, run_continues_list);
                }
                result.push_str(dedent_code_indent(line).unwrap_or(line));
                result.push('\n');
                continue;
            }

            if code_lines.is_empty() {
                run_continues_list = after_list_item;
            }
            code_lines.push(line);
        } else if is_blank && !code_lines.is_empty() {
            code_lines.push("");
        } else {
            if !code_lines.is_empty() {
                flush_code_block(&mut result, &mut code_lines, run_continues_list);
            }
            if !is_blank {
                after_list_item = LIST_ITEM_RE.is_match(line);
            }
            result.push_str(line);
            result.push('\n');
        }
    }

    if !code_lines.is_empty() {
        flush_code_block(&mut result, &mut code_lines, run_continues_list);
    }

    // Match original trailing newline behavior.
    if result.ends_with('\n') && !md.ends_with('\n') {
        result.pop();
    }

    result
}

/// A GFM pipe-table row (`| a | b |`), including a separator row
/// (`| --- | :-: |`), which also starts and ends with `|`.
fn is_gfm_table_line(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 2 && t.starts_with('|') && t.ends_with('|')
}

/// A GFM header separator row: `|` delimited, made only of dashes, colons,
/// pipes and spaces, and carrying at least one dash.
fn is_gfm_separator_line(line: &str) -> bool {
    let t = line.trim();
    is_gfm_table_line(t)
        && t.contains('-')
        && t[1..t.len() - 1]
            .chars()
            .all(|c| matches!(c, '-' | ':' | '|' | ' ' | '\t'))
}

fn dedent_code_indent(line: &str) -> Option<&str> {
    line.strip_prefix("    ")
        .or_else(|| line.strip_prefix('\t'))
}

/// Mark the indented lines that belong to a real pipe table, so the
/// indented-code pass can pass them through instead of fencing them.
///
/// A run only counts as a table when it has at least two consecutive pipe
/// lines AND one of them is a header separator. A single pipe-looking line
/// inside genuine indented code (`    | 0 | 1 |` in an ASCII diagram) would
/// otherwise split that code block into three pieces.
fn indented_table_line_flags(lines: &[&str]) -> Vec<bool> {
    let is_indented_pipe = |line: &str| dedent_code_indent(line).is_some_and(is_gfm_table_line);

    let mut flags = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if !is_indented_pipe(lines[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && is_indented_pipe(lines[i]) {
            i += 1;
        }
        let run = &lines[start..i];
        if run.len() >= 2
            && run
                .iter()
                .filter_map(|l| dedent_code_indent(l))
                .any(is_gfm_separator_line)
        {
            flags[start..i].fill(true);
        }
    }
    flags
}

fn flush_code_block(result: &mut String, code_lines: &mut Vec<&str>, continues_list: bool) {
    while code_lines.last() == Some(&"") {
        code_lines.pop();
    }
    if code_lines.is_empty() {
        return;
    }

    // An indented run that continues a list and holds list items is a nested
    // list, not code. Emit it untouched, fencing it would strip the markers and
    // destroy the list. Both conditions are needed: a genuine code block can
    // contain a line that looks like a list item, but it will not follow one.
    if continues_list && code_lines.iter().any(|l| LIST_ITEM_RE.is_match(l)) {
        for line in code_lines.iter() {
            result.push_str(line);
            result.push('\n');
        }
        code_lines.clear();
        return;
    }

    result.push_str("```\n");
    for line in code_lines.iter() {
        let stripped = line
            .strip_prefix("    ")
            .or_else(|| line.strip_prefix('\t'))
            .unwrap_or(line);
        result.push_str(stripped);
        result.push('\n');
    }
    result.push_str("```\n");
    code_lines.clear();
}

/// Strip markdown images with data: URIs (base64 blobs) as a safety net
/// for cases where clean.rs didn't run (e.g. onlyMainContent: false + rawHtml format).
fn strip_data_uris(md: &str) -> String {
    static DATA_URI_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"!\[[^\]]*\]\(data:[^)]{10,}\)").unwrap());
    DATA_URI_RE.replace_all(md, "").to_string()
}

/// Remove empty anchors and decorative linked pilcrows from headings.
fn strip_anchor_artifacts(md: &str) -> String {
    static EMPTY_ANCHOR_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"\[\]\(#[^)]*\)"#).unwrap());
    static HEADING_PILCROW_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?m)^(#{1,6}[^\r\n]*?)[ \t]*\[¶\]\(#[^)\r\n]*\)[ \t]*$"#).unwrap()
    });

    let without_empty = EMPTY_ANCHOR_RE.replace_all(md, "");
    HEADING_PILCROW_RE
        .replace_all(&without_empty, "$1")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_basic_html() {
        let html = "<h1>Title</h1><p>Paragraph with <strong>bold</strong> text.</p>";
        let md = html_to_markdown(html);
        assert!(md.contains("# Title"));
        assert!(md.contains("**bold**"));
    }

    #[test]
    fn strips_pilcrow_from_headers() {
        let html = r##"<h2>Section <a href="#section">¶</a></h2>"##;
        let md = html_to_markdown(html);
        assert!(!md.contains('\u{00b6}'));
        assert!(md.contains("Section"));
    }

    #[test]
    fn preserves_section_sign_in_body_prose() {
        // Regression: `strip_anchor_artifacts` ran `.replace(" §", "")` over the
        // whole document, so a statutory citation lost both the sigil and the
        // space before it. Section signs are load-bearing in legal content.
        let md = html_to_markdown("<p>See § 230 of the CDA for details.</p>");
        assert!(
            md.contains("§ 230"),
            "statutory citation must survive extraction, got {md:?}"
        );

        let de = html_to_markdown("<p>Nach § 1 BGB gilt das.</p>");
        assert!(
            de.contains("§ 1 BGB"),
            "German statutory citation must survive extraction, got {de:?}"
        );
    }

    #[test]
    fn preserves_a_decorative_sigil_that_is_not_an_anchor_link() {
        // Pins the trade-off this behaviour accepts. A site that renders a
        // headerlink as a bare span rather than an anchor keeps its sigil,
        // because after conversion it is indistinguishable from content. This
        // test exists so the fix is not "corrected" by reinstating a
        // document-wide replace, which is what destroyed `§ 230` in prose.
        let md = html_to_markdown(r##"<h2>Section <span class="anchor">¶</span></h2>"##);
        assert!(
            md.contains('\u{00b6}'),
            "a sigil outside a fragment anchor is content, not an artifact, got {md:?}"
        );
    }

    #[test]
    fn preserves_pilcrow_in_body_prose() {
        // Same class: a pilcrow discussed as content, not used as a headerlink.
        let md = html_to_markdown("<p>The pilcrow ¶ marks a paragraph break.</p>");
        assert!(
            md.contains("pilcrow ¶ marks"),
            "a pilcrow in prose must survive extraction, got {md:?}"
        );
    }

    #[test]
    fn strips_only_heading_pilcrow_links() {
        let input = "## Heading [¶](#heading)\n\n[¶](#body)\n\nSee [¶](#para12) 12.\n\n[](#empty)";
        let result = strip_anchor_artifacts(input);
        assert!(!result.contains("[](#"));
        assert!(result.contains("## Heading\n"));
        assert!(result.contains("[¶](#body)"));
        assert!(result.contains("See [¶](#para12) 12."));
    }

    #[test]
    fn nested_lists_are_not_fenced_as_code() {
        // htmd indents nested list items by 4 spaces, which used to be read as
        // an indented code block, every dropdown menu came back as ``` (#365).
        let html = "<ul><li>Company<ul><li><a href=\"/about\">About</a></li>\
                    <li><a href=\"/awards\">Awards</a></li></ul></li></ul>";
        let md = html_to_markdown(html);
        assert!(!md.contains("```"), "nested list got fenced: {md}");
        assert!(md.contains("About"), "list content lost: {md}");
        assert!(md.contains("*   [Awards]"), "list markers lost: {md}");
    }

    #[test]
    fn genuine_indented_code_still_gets_fenced() {
        let input = "Intro paragraph\n\n    let x = 1;\n    let y = 2;\n\nOutro";
        let result = convert_indented_code_to_fenced(input);
        assert!(result.contains("```"), "code lost its fence: {result}");
        assert!(result.contains("let x = 1;"), "code body lost: {result}");
        assert!(
            !result.contains("    let x = 1;"),
            "indent should be stripped inside the fence: {result}"
        );
    }

    #[test]
    fn drops_repeated_nav_blocks_only() {
        let nav1 = "*   [Where to buy our tiles](https://example.com/where-to-buy/)";
        let nav2 = "*   [Exclusive showrooms near you](https://example.com/showrooms/)";
        let heading = "## Technical specification of the product";
        let row = "| Porcelain Matt | 30x120CM | Stair Tiles | Residential |";
        let prose = "Matt tiles offer a sophisticated, non-shiny surface finish.";
        let input = format!(
            "{nav1}\n{nav2}\n\n{heading}\n\n{row}\n\n{prose}\n\n{nav1}\n{nav2}\n\n{heading}\n\n{row}\n\n{prose}\n"
        );
        let result = drop_repeated_nav_lines(&input);

        assert_eq!(result.matches(nav1).count(), 1, "nav repeat kept: {result}");
        assert_eq!(result.matches(nav2).count(), 1, "nav repeat kept: {result}");
        // Everything that carries content of its own keeps every occurrence:
        // a repeated table row is a second record, not a duplicate menu.
        assert_eq!(result.matches(heading).count(), 2, "heading lost: {result}");
        assert_eq!(result.matches(row).count(), 2, "table row lost: {result}");
        assert_eq!(result.matches(prose).count(), 2, "prose lost: {result}");
    }

    #[test]
    fn keeps_a_lone_repeated_entry() {
        // One link repeated on its own is more likely a genuine cross-listing
        // (the same item catalogued under two categories) than a menu.
        let item = "*   [The Pragmatic Programmer](https://example.com/books/pragmatic)";
        let input = format!("## Best sellers\n\n{item}\n\n## New arrivals\n\n{item}\n");
        let result = drop_repeated_nav_lines(&input);
        assert_eq!(
            result.matches(item).count(),
            2,
            "cross-listing lost: {result}"
        );
    }

    #[test]
    fn keeps_two_items_cross_listed_together() {
        // A and B were each listed under their own category; a third category
        // lists both together. That pair never appeared as a block before, so
        // it is a real cross-listing, not a repeated menu.
        let a = "*   [The Pragmatic Programmer](https://example.com/books/pragmatic)";
        let b = "*   [Structure and Interpretation](https://example.com/books/sicp)";
        let other = "*   [Working Effectively with Legacy Code](https://example.com/books/legacy)";
        let input = format!(
            "## Classics\n\n{a}\n{other}\n\n## Foundations\n\n{b}\n\n## Staff picks\n\n{a}\n{b}\n"
        );
        let result = drop_repeated_nav_lines(&input);
        assert_eq!(result.matches(a).count(), 2, "cross-listing lost: {result}");
        assert_eq!(result.matches(b).count(), 2, "cross-listing lost: {result}");
    }

    #[test]
    fn dedup_keeps_nesting_levels_distinct() {
        let a = "*   [Long privacy policy link text](https://example.com/privacy/)";
        let b = "*   [Long terms of service link text](https://example.com/terms/)";
        // The same two entries again, one level deeper: a repeated BLOCK, so the
        // run rule alone would drop it. Keyed on the untrimmed line these are
        // different lines, so the nested copy survives, which is what makes this
        // a guard on the key and not just on the run length.
        let input = format!("{a}\n{b}\n    {a}\n    {b}\n");
        let result = drop_repeated_nav_lines(&input);
        assert_eq!(result.lines().count(), 4, "nested block lost: {result}");
        assert!(
            result.contains(&format!("    {a}")),
            "nested entry lost: {result}"
        );
    }

    #[test]
    fn dedup_leaves_code_blocks_alone() {
        // The lines are eligible in shape, a markdown sample inside a fence ,
        // so the fence tracking is the only thing keeping them.
        let a = "*   [Where to buy our tiles](https://example.com/where-to-buy/)";
        let b = "*   [Exclusive showrooms near you](https://example.com/showrooms/)";
        let input = format!("```markdown\n{a}\n{b}\n{a}\n{b}\n```\n");
        let result = drop_repeated_nav_lines(&input);
        assert_eq!(result.matches(a).count(), 2, "code repeat lost: {result}");
        assert_eq!(result.matches(b).count(), 2, "code repeat lost: {result}");
    }

    #[test]
    fn converts_links() {
        let html = r#"<p><a href="https://example.com">Link</a></p>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("[Link](https://example.com)"));
    }

    #[test]
    fn table_safe_fence_pass_does_not_fence_indented_pipe_table() {
        let md = "Intro\n\n    | A | B |\n    | - | - |\n    | 1 | 2 |\n";
        let out = convert_indented_code_to_fenced_table_safe(md);
        assert!(!out.contains("```"), "pipe table must not be fenced: {out}");
        assert!(out.contains("| A | B |"));
        assert!(out.contains("| 1 | 2 |"));
    }

    #[test]
    fn table_safe_fence_pass_still_fences_real_indented_code() {
        let md = "Intro\n\n    fn main() {\n        println!(\"hi\");\n    }\n";
        let out = convert_indented_code_to_fenced_table_safe(md);
        assert!(
            out.contains("```"),
            "real indented code must still be fenced: {out}"
        );
        assert!(out.contains("fn main()"));
    }

    /// W2: a single pipe-looking line inside genuine indented code used to be
    /// passed through, splitting one code block into three pieces.
    #[test]
    fn table_safe_fence_pass_keeps_code_with_a_single_pipe_line_intact() {
        let md = "Intro\n\n    fn render() {\n    | col1 | col2 |\n        draw();\n    }\n";
        let out = convert_indented_code_to_fenced_table_safe(md);
        assert_eq!(
            out.matches("```").count(),
            2,
            "code block must stay one piece: {out}"
        );
        assert!(out.contains("| col1 | col2 |"), "line lost: {out}");
        assert!(out.contains("fn render()") && out.contains("draw();"));
    }

    /// W2 guard: two consecutive pipe lines with no separator row are still
    /// code, not a table.
    #[test]
    fn table_safe_fence_pass_needs_a_separator_row() {
        let md = "Intro\n\n    | a | b |\n    | c | d |\n";
        let out = convert_indented_code_to_fenced_table_safe(md);
        assert!(
            out.contains("```"),
            "a pipe run with no separator is not a table: {out}"
        );
    }

    #[test]
    fn legacy_fence_pass_still_fences_indented_pipe_lines() {
        // The ORIGINAL (non-table-safe) pass is the documented bug this PR
        // fixes only behind the flag: an indented pipe line is still folded
        // into a fenced code block when `normalize_tables` is off.
        let md = "Intro\n\n    | A | B |\n    | - | - |\n    | 1 | 2 |\n";
        let out = convert_indented_code_to_fenced(md);
        assert!(
            out.contains("```"),
            "legacy path keeps fencing indented pipes: {out}"
        );
    }
}
