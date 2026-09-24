# crw-extract

HTML content extraction and format conversion engine for the [CRW](https://github.com/fastcrw/crw) web scraper.

[![crates.io](https://img.shields.io/crates/v/crw-extract.svg)](https://crates.io/crates/crw-extract)
[![docs.rs](https://docs.rs/crw-extract/badge.svg)](https://docs.rs/crw-extract)
[![license](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](https://github.com/fastcrw/crw/blob/main/LICENSE)

## Overview

`crw-extract` converts raw HTML into clean, structured output formats for LLM consumption, RAG pipelines, and data extraction.

- **Markdown** — High-fidelity HTML→Markdown via `htmd` (Turndown.js port): tables, code blocks, nested lists. Indented code blocks are post-processed into fenced (```) blocks for better LLM compatibility
- **Plain text** — Tag-stripped, whitespace-normalized text
- **Cleaned HTML** — Boilerplate removal (scripts, styles, nav, footer, ads)
- **Readability** — Main-content extraction with text-density scoring and multi-selector fallback
- **CSS selector & XPath** — Narrow content to specific DOM elements before conversion
- **Chunking** — Split content into sentence, topic (heading-based), or regex-delimited chunks
- **BM25 & cosine filtering** — Rank chunks by relevance to a query, return top-K results
- **Structured JSON** — LLM-based extraction with JSON Schema validation (Anthropic tool_use + OpenAI function calling)

## Installation

```bash
cargo add crw-extract
```

## Usage

### High-level extraction pipeline

The `extract()` function runs the full pipeline: clean → select → readability → convert → chunk → filter.

```rust,no_run
use crw_extract::{extract, ExtractOptions};
use crw_core::types::OutputFormat;

let html = r#"<html><body><article><h1>Hello</h1><p>World</p></article></body></html>"#;

let result = extract(ExtractOptions {
    raw_html: html,
    source_url: "https://example.com",
    status_code: 200,
    rendered_with: None,
    elapsed_ms: 42,
    formats: &[OutputFormat::Markdown, OutputFormat::Links],
    only_main_content: true,
    include_tags: &[],
    exclude_tags: &[],
    css_selector: None,
    xpath: None,
    chunk_strategy: None,
    query: None,
    filter_mode: None,
    top_k: None,
}).unwrap();

println!("{}", result.markdown.unwrap());
// # Hello
//
// World
```

### HTML to Markdown

```rust
use crw_extract::markdown::html_to_markdown;

let md = html_to_markdown("<h1>Title</h1><p>Paragraph with <strong>bold</strong> text.</p>");
assert!(md.contains("# Title"));
assert!(md.contains("**bold**"));
```

### HTML to plain text

```rust
use crw_extract::plaintext::html_to_plaintext;

let text = html_to_plaintext("<p>Hello <b>world</b></p>");
assert_eq!(text.trim(), "Hello world");
```

### HTML cleaning

Remove boilerplate elements (scripts, styles, nav, footer, ads):

```rust
use crw_extract::clean::clean_html;

let html = r#"<html><body><nav>Menu</nav><article><p>Content</p></article><footer>Footer</footer></body></html>"#;
let cleaned = clean_html(html, true, &[], &[]).unwrap();
// nav and footer are stripped, article content is preserved
```

Filter by tag inclusion/exclusion:

```rust
use crw_extract::clean::clean_html;

let html = "<div><p>Keep this</p><span>Remove this</span></div>";
let result = clean_html(html, false, &["p".into()], &[]).unwrap();
assert!(result.contains("Keep this"));
```

### CSS selector extraction

```rust
use crw_extract::selector::extract_by_css;

let html = r#"<div><article class="post"><p>Target content</p></article><aside>Sidebar</aside></div>"#;
let result = extract_by_css(html, "article.post").unwrap();
assert!(result.unwrap().contains("Target content"));
```

### XPath extraction

```rust
use crw_extract::selector::extract_by_xpath;

let html = "<html><body><h1>Title</h1><p>Text</p></body></html>";
let result = extract_by_xpath(html, "//h1").unwrap();
assert_eq!(result.unwrap(), vec!["Title".to_string()]);
```

### Chunking

Split content into chunks for RAG pipelines:

```rust
use crw_extract::chunking::chunk_text;
use crw_core::types::ChunkStrategy;

let text = "# Introduction\nFirst section.\n# Methods\nSecond section.";
let strategy = ChunkStrategy::Topic {
    max_chars: None,
    overlap_chars: None,
    dedupe: None,
};
let chunks = chunk_text(text, &strategy);
assert_eq!(chunks.len(), 2);
```

### Chunk filtering

Rank chunks by relevance using BM25 or cosine similarity:

```rust
use crw_extract::filter::filter_chunks;
use crw_core::types::FilterMode;

let chunks = vec![
    "Rust is a systems programming language".to_string(),
    "The weather is sunny today".to_string(),
    "Rust provides memory safety without GC".to_string(),
];
let top = filter_chunks(&chunks, "Rust programming", &FilterMode::Bm25, 2);
assert_eq!(top.len(), 2);
// Chunks mentioning "Rust" are ranked higher
```

### Metadata extraction

Extract title, description, Open Graph metadata, and links:

```rust
use crw_extract::readability::{extract_metadata, extract_links};

let html = r#"<html><head><title>My Page</title><meta name="description" content="A page"></head><body><a href="/about">About</a></body></html>"#;
let meta = extract_metadata(html);
assert_eq!(meta.title, Some("My Page".into()));

let links = extract_links(html, "https://example.com");
assert!(links.iter().any(|l| l.contains("/about")));
```

## Part of CRW

This crate is part of the [CRW](https://github.com/fastcrw/crw) workspace — a fast, lightweight, Firecrawl-compatible web scraper built in Rust.

This crate (`crw-extract`) handles HTML → markdown/plaintext extraction. For
the full crate table and dependency graph, see
[docs.fastcrw.com/architecture/](https://docs.fastcrw.com/architecture/).

## License

AGPL-3.0 — see [LICENSE](https://github.com/fastcrw/crw/blob/main/LICENSE).
