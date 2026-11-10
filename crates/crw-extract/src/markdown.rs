use htmd::element_handler::{HandlerResult, Handlers};
use htmd::{Element, HtmlToMarkdown};
use markup5ever_rcdom::{Node, NodeData};
use regex::Regex;
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::rc::Rc;
use std::sync::LazyLock;

/// htmd with two of its handlers wrapped: `table` (padding removed) and `pre`
/// (code rendered one element per line). Every other element converts exactly
/// as `htmd::convert` would.
static CONVERTER: LazyLock<HtmlToMarkdown> = LazyLock::new(|| {
    HtmlToMarkdown::builder()
        .add_handler(vec!["table"], compact_table)
        .add_handler(vec!["pre"], line_element_code_block)
        .build()
});

/// Two or more spaces closing a cell. htmd escapes a literal `|` inside a cell
/// as `&#124;`, so in its table output every `|` is a column delimiter.
static TABLE_PADDING_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r" {2,}\|").unwrap());
static TABLE_RULE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-{4,}").unwrap());

/// htmd pads every cell to the widest cell in its column and draws the
/// separator just as wide, so one long cell (a titled link) widens every row
/// of the table. On wide tables that padding is most of the output and eats
/// the caller's `maxLength`.
///
/// htmd only draws a GFM table for a `<table>` with a header cell that is not
/// itself inside a table cell; any other table comes back as its plain child
/// content, which can hold pipe-shaped text of its own (ASCII art in a
/// `<pre>`) and is left alone. In a drawn table only the run of pipe lines
/// that opens with the header and separator rows is rewritten, and only the
/// separator row loses dashes, so a data row of `-----` keeps its content.
fn compact_table(handlers: &dyn Handlers, element: Element) -> Option<HandlerResult> {
    let drawn = htmd_draws_table(element.node);
    let mut result = handlers.fallback(element)?;
    if !drawn || !result.markdown_translated {
        return Some(result);
    }
    let lines: Vec<&str> = result.content.split('\n').collect();
    let Some(header) = lines.iter().position(|l| is_gfm_table_line(l)) else {
        return Some(result);
    };
    if !lines
        .get(header + 1)
        .is_some_and(|l| is_gfm_separator_line(l))
    {
        return Some(result);
    }
    let mut in_table = false;
    let compacted: Vec<Cow<'_, str>> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            in_table = (i == header || in_table) && is_gfm_table_line(line);
            if i == header + 1 {
                TABLE_RULE_RE.replace_all(line, "---")
            } else if in_table {
                TABLE_PADDING_RE.replace_all(line, " |")
            } else {
                Cow::Borrowed(*line)
            }
        })
        .collect();
    result.content = compacted.join("\n");
    Some(result)
}

/// Mirrors the condition under which htmd 0.5's table handler draws a pipe
/// table in its default (pure) mode: a `<th>` or `<thead>` that does not
/// belong to a nested table, and no `<td>`/`<th>` ancestor.
fn htmd_draws_table(table: &Rc<Node>) -> bool {
    fn has_header(node: &Rc<Node>) -> bool {
        node.children
            .borrow()
            .iter()
            .any(|child| match element_tag(child) {
                Some("table") => false,
                Some("th" | "thead") => true,
                _ => has_header(child),
            })
    }
    let mut ancestor = parent_of(table);
    while let Some(node) = ancestor {
        if matches!(element_tag(&node), Some("td" | "th")) {
            return false;
        }
        ancestor = parent_of(&node);
    }
    has_header(table)
}

fn parent_of(node: &Node) -> Option<Rc<Node>> {
    // rcdom keeps the parent in a `Cell<Option<Weak>>`: take it to read it,
    // then put it back.
    let weak = node.parent.take();
    let parent = weak.as_ref().and_then(std::rc::Weak::upgrade);
    node.parent.set(weak);
    parent
}

fn element_tag(node: &Node) -> Option<&str> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.as_ref()),
        _ => None,
    }
}

/// Some sites render a code sample as one element per line inside a `<pre>`,
/// with no newline in the text and the line breaks coming only from CSS
/// (docs.unity3d.com: `<pre><div><span>line</span><span>line</span>...`).
/// htmd fences only `<pre><code>`, so such a sample came out as a single line
/// of prose. When the `<pre>` text has no newline and one `<div>` inside it
/// holds all of that text as a run of identical sibling elements, each sibling
/// becomes a line of a fenced block. Anything else converts as before.
fn line_element_code_block(handlers: &dyn Handlers, element: Element) -> Option<HandlerResult> {
    match code_lines_from_elements(element.node) {
        Some(lines) => Some(fenced_code_block(&lines).into()),
        None => handlers.fallback(element),
    }
}

fn code_lines_from_elements(pre: &Rc<Node>) -> Option<Vec<String>> {
    let text = node_text(pre);
    if text.contains('\n') || text.trim().is_empty() {
        return None;
    }
    let visible = non_whitespace(&text);
    let mut stack: Vec<Rc<Node>> = pre.children.borrow().iter().rev().cloned().collect();
    while let Some(node) = stack.pop() {
        if let Some(lines) = line_run(&node)
            && non_whitespace(&lines.concat()) == visible
        {
            return Some(lines);
        }
        stack.extend(node.children.borrow().iter().rev().cloned());
    }
    None
}

/// The text of each child when `node` is a `<div>` whose content is two or
/// more element children of the same tag and class, with no text between them.
///
/// Each child must also look like a line rather than a token: a block element
/// (`<div>`, `<p>`), or an element that carries no text of its own and only
/// wraps others. A run of bare `<span>word</span>` tokens on one line of code
/// is the common highlighter shape and must stay one line.
fn line_run(node: &Rc<Node>) -> Option<Vec<String>> {
    let NodeData::Element { name, .. } = &node.data else {
        return None;
    };
    if name.local.as_ref() != "div" {
        return None;
    }
    let children = node.children.borrow();
    let mut signature = None;
    let mut lines = Vec::new();
    for child in children.iter() {
        match &child.data {
            NodeData::Text { contents } if contents.borrow().trim().is_empty() => {}
            NodeData::Element { name, attrs, .. } => {
                let class = attrs
                    .borrow()
                    .iter()
                    .find(|a| a.name.local.as_ref() == "class")
                    .map(|a| a.value.to_string());
                let sig = (name.local.to_string(), class);
                if signature.get_or_insert_with(|| sig.clone()) != &sig {
                    return None;
                }
                let is_block = matches!(name.local.as_ref(), "div" | "p");
                let has_own_text = child.children.borrow().iter().any(|c| {
                    matches!(&c.data, NodeData::Text { contents } if !contents.borrow().trim().is_empty())
                });
                if (!is_block && has_own_text) || !holds_only_text(child) {
                    return None;
                }
                lines.push(node_text(child));
            }
            NodeData::Comment { .. } => {}
            _ => return None,
        }
    }
    (lines.len() >= 2).then_some(lines)
}

/// Whether everything under `node` is text in plain wrapper or emphasis
/// elements. A link, a `<br>` or an image would lose its target or its break
/// when the line is read as text, so such a `<pre>` keeps the old path.
fn holds_only_text(node: &Rc<Node>) -> bool {
    node.children
        .borrow()
        .iter()
        .all(|child| match element_tag(child) {
            Some("span" | "div" | "p" | "code" | "b" | "strong" | "i" | "em") => {
                holds_only_text(child)
            }
            Some(_) => false,
            None => true,
        })
}

fn node_text(node: &Rc<Node>) -> String {
    let mut out = String::new();
    let mut stack = vec![Rc::clone(node)];
    while let Some(n) = stack.pop() {
        if let NodeData::Text { contents } = &n.data {
            out.push_str(&contents.borrow());
        }
        stack.extend(n.children.borrow().iter().rev().cloned());
    }
    out
}

fn non_whitespace(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn fenced_code_block(lines: &[String]) -> String {
    let lines: Vec<&str> = lines.iter().map(|l| l.trim_end()).collect();
    let start = lines.iter().position(|l| !l.is_empty()).unwrap_or(0);
    let end = lines
        .iter()
        .rposition(|l| !l.is_empty())
        .map_or(0, |i| i + 1);
    let body = lines[start..end].join("\n");
    let longest_backtick_run = body.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest_backtick_run.max(2) + 1);
    format!("\n\n{fence}\n{body}\n{fence}\n\n")
}

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
    let md = CONVERTER.convert(&html).unwrap_or_default();
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

    #[test]
    fn table_cells_are_not_padded_to_column_width() {
        let long = "x".repeat(200);
        let html = format!(
            "<table><tr><th>Name</th><th>Note</th></tr>\
             <tr><td>a</td><td>{long}</td></tr>\
             <tr><td>b</td><td></td></tr>\
             <tr><td>c &#124; d</td><td>short</td></tr></table>"
        );
        let md = html_to_markdown(&html);
        assert!(md.contains("| Name | Note |"), "{md}");
        assert!(md.contains("| --- | --- |"), "{md}");
        assert!(md.contains("| b | |"), "{md}");
        assert!(md.contains("| c &#124; d | short |"), "{md}");
        assert!(md.contains(&format!("| a | {long} |")), "{md}");
        assert!(!md.contains("  |"), "padding left in: {md}");
        assert!(md.len() < long.len() + 200, "{} chars: {md}", md.len());
    }

    #[test]
    fn padding_compaction_applies_with_normalize_tables() {
        let html = "<table><tr><th>A</th><th>Bbbbbbbbbbbbbbbb</th></tr>\
                    <tr><td colspan=\"2\">wide</td></tr></table>";
        let md = html_to_markdown_with(html, true);
        assert!(md.contains("| A | Bbbbbbbbbbbbbbbb |"), "{md}");
        assert!(!md.contains("  |"), "{md}");
    }

    #[test]
    fn spaces_outside_tables_are_untouched() {
        let html = "<p>a  | b</p><pre><code>| x    | y |\n| ---- | - |</code></pre>";
        let md = html_to_markdown(html);
        assert!(md.contains("| x    | y |"), "code changed: {md}");
        assert!(md.contains("| ---- | - |"), "code changed: {md}");
    }

    /// docs.unity3d.com 6000.3 markup: one span per code line, no newline.
    const LINE_SPAN_PRE: &str = r#"<h3>Examples</h3><pre class="MuiTypography-root" data-code-block="true"><div class="MuiBox-root mui-0"><div class="MuiStack-root inline-code-block-controls"><button aria-label="Copy code to clipboard"><svg><path d="M0"></path></svg></button><button aria-label="Toggle fullscreen"><svg><path d="M1"></path></svg></button></div><div class="MuiStack-root mui-1u6odbp"><span class="MuiSkeleton-root mui-1y71r9t" style="height:18px"><span class="MuiTypography-code mui-15abx7k">using UnityEngine;</span></span><span class="MuiSkeleton-root mui-1y71r9t" style="height:18px"><span class="MuiTypography-code mui-15abx7k"></span></span><span class="MuiSkeleton-root mui-1y71r9t" style="height:18px"><span class="MuiTypography-code mui-15abx7k">public class Example : MonoBehaviour</span></span><span class="MuiSkeleton-root mui-1y71r9t" style="height:18px"><span class="MuiTypography-code mui-15abx7k">{</span></span><span class="MuiSkeleton-root mui-1y71r9t" style="height:18px"><span class="MuiTypography-code mui-15abx7k">    //Fetch the Rigidbody</span></span><span class="MuiSkeleton-root mui-1y71r9t" style="height:18px"><span class="MuiTypography-code mui-15abx7k">    m_Rigidbody = GetComponent&lt;Rigidbody&gt;();</span></span><span class="MuiSkeleton-root mui-1y71r9t" style="height:18px"><span class="MuiTypography-code mui-15abx7k">}</span></span><span class="MuiSkeleton-root mui-1y71r9t" style="height:18px"><span class="MuiTypography-code mui-15abx7k"></span></span></div></div></pre><p>After.</p>"#;

    #[test]
    fn line_per_element_pre_becomes_a_fenced_block() {
        let md = html_to_markdown(LINE_SPAN_PRE);
        let expected = "```\nusing UnityEngine;\n\npublic class Example : MonoBehaviour\n{\n    //Fetch the Rigidbody\n    m_Rigidbody = GetComponent<Rigidbody>();\n}\n```";
        assert!(md.contains(expected), "{md}");
        assert!(md.contains("After."), "{md}");
    }

    #[test]
    fn line_per_element_pre_with_normalize_tables() {
        let html = format!(
            "<html><head><title>T</title></head><body><main><h1>AddForce</h1><p>Adds a force to the Rigidbody.</p>{LINE_SPAN_PRE}</main></body></html>"
        );
        let md = html_to_markdown_with(&html, true);
        assert!(md.contains("```\nusing UnityEngine;\n"), "{md}");
    }

    #[test]
    fn ordinary_pre_blocks_convert_as_before() {
        // Real newlines: the line rule does not apply.
        let with_newlines =
            "<pre><div><span class=\"l\">a = 1</span>\n<span class=\"l\">b = 2</span></div></pre>";
        assert_eq!(
            html_to_markdown(with_newlines),
            htmd::convert(with_newlines).unwrap()
        );
        // Token spans directly under <pre>/<code>, not a <div> of lines.
        let tokens = "<pre><code><span class=\"t\">print</span><span class=\"t\">(</span><span class=\"t\">x</span><span class=\"t\">)</span></code></pre>";
        assert!(html_to_markdown(tokens).contains("print(x)"));
        // Mixed siblings: not a uniform run of lines.
        let mixed = "<pre><div><span class=\"a\">one</span><b>two</b></div></pre>";
        assert_eq!(html_to_markdown(mixed), htmd::convert(mixed).unwrap());
        // Text outside the run (a visible "Copy" label): fall back.
        let labelled = "<pre><div><button>Copy</button></div><div><span class=\"l\">a</span><span class=\"l\">b</span></div></pre>";
        assert_eq!(html_to_markdown(labelled), htmd::convert(labelled).unwrap());
    }

    #[test]
    fn fence_outgrows_backticks_in_the_code() {
        let html = "<pre><div><div class=\"l\">let s = \"```\";</div><div class=\"l\">done</div></div></pre>";
        let md = html_to_markdown(html);
        assert!(md.contains("````\nlet s = \"```\";\ndone\n````"), "{md}");
    }

    #[test]
    fn layout_tables_without_a_header_are_untouched() {
        let html = "<table><tr><td><pre>| a    | b |\n| ---- | - |</pre></td></tr></table>";
        assert_eq!(html_to_markdown(html), htmd::convert(html).unwrap());
        let nested = "<table><tr><th>H</th></tr><tr><td><table><tr><th>x    </th></tr></table></td></tr></table>";
        assert!(html_to_markdown(nested).contains("| H |"));
    }

    #[test]
    fn a_data_row_of_dashes_keeps_its_content() {
        let html = "<table><tr><th>Name</th><th>Value</th></tr>\
                    <tr><td>a</td><td>-----</td></tr></table>";
        let md = html_to_markdown(html);
        assert!(md.contains("| Name | Value |\n| --- | --- |"), "{md}");
        assert!(md.contains("| a | ----- |"), "{md}");
    }

    #[test]
    fn single_line_token_spans_stay_one_line() {
        for html in [
            "<pre><div><span>git</span> <span>clone</span> <span>url</span></div></pre>",
            "<pre><div><span class=\"p\">{</span><span class=\"p\">}</span></div></pre>",
        ] {
            assert_eq!(
                html_to_markdown(html),
                htmd::convert(html).unwrap(),
                "{html}"
            );
        }
    }

    #[test]
    fn lines_with_links_or_breaks_keep_the_old_path() {
        for html in [
            "<pre><div><p>a<br>b</p><p>c</p></div></pre>",
            "<pre><div><div><a href=\"/x\">one</a></div><div><a href=\"/y\">two</a></div></div></pre>",
        ] {
            assert_eq!(
                html_to_markdown(html),
                htmd::convert(html).unwrap(),
                "{html}"
            );
        }
    }
}
