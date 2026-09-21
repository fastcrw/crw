//! Scrape subcommand — fetch a single URL and extract content.

use crate::teardown::CmdError;
use clap::{Args, ValueEnum};
use crw_core::config::{RendererConfig, RendererMode, StealthConfig};
use crw_core::types::{OutputFormat, ScrapeRequest};
use crw_crawl::single::scrape_url;
use crw_renderer::FallbackRenderer;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, ValueEnum)]
pub enum Format {
    Markdown,
    Json,
    Html,
    Rawhtml,
    Text,
    Links,
    Images,
}

#[derive(Args)]
pub struct ScrapeArgs {
    /// URL to scrape (http or https)
    pub url: String,

    /// Output format
    #[arg(short, long, value_enum, default_value = "markdown")]
    pub format: Format,

    /// Write output to file instead of stdout
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<String>,

    /// Disable main content extraction (return full page content)
    #[arg(long)]
    pub raw: bool,

    /// Enable JavaScript rendering (auto-detects LightPanda/Chrome, or use CRW_CDP_URL)
    #[arg(long)]
    pub js: bool,

    /// Extract only elements matching this CSS selector
    #[arg(long, value_name = "SELECTOR")]
    pub css: Option<String>,

    /// Extract only elements matching this XPath expression
    #[arg(long, value_name = "EXPR")]
    pub xpath: Option<String>,

    /// HTTP, HTTPS, or SOCKS5 proxy URL (e.g. http://user:pass@host:port or socks5://user:pass@host:1080)
    #[arg(long, value_name = "URL")]
    pub proxy: Option<String>,

    /// Enable stealth mode (rotate user agents, inject browser headers)
    #[arg(long)]
    pub stealth: bool,

    /// Generate an AI summary of the page using the configured LLM.
    #[arg(long, conflicts_with = "extract")]
    pub summary: bool,

    /// Style/format hint for --summary (e.g. "in 3 bullet points", "as a haiku").
    #[arg(long, value_name = "TEXT", requires = "summary")]
    pub prompt: Option<String>,

    /// Extract structured data using a JSON Schema.
    /// Accepts inline JSON or @path/to/schema.json.
    #[arg(long, value_name = "SCHEMA")]
    pub extract: Option<String>,

    /// Override LLM provider (anthropic, openai, openai-responses, deepseek, azure, openrouter).
    #[arg(long, value_name = "NAME")]
    pub llm_provider: Option<String>,

    /// Override LLM API key for this request.
    #[arg(long, value_name = "KEY")]
    pub llm_key: Option<String>,

    /// Override LLM model for this request.
    #[arg(long, value_name = "MODEL")]
    pub llm_model: Option<String>,

    /// Override LLM base URL (for Chat Completions-compatible, Responses-compatible, or Azure endpoints).
    #[arg(long, value_name = "URL")]
    pub llm_base_url: Option<String>,
}

pub async fn run(mut args: ScrapeArgs) -> Result<(), CmdError> {
    // Local document short-circuit: when the positional arg is an existing file
    // on disk (not a URL), parse it directly. Only PDF is supported.
    if std::path::Path::new(&args.url).is_file() {
        return run_local_file(&args).await;
    }

    // Auto-prepend https:// if no scheme is provided
    if !args.url.contains("://") {
        args.url = format!("https://{}", args.url);
    }

    // First-run nudge for plain scrapes only. AI modes already prompt
    // interactively when there's no config, so we'd be doubling up.
    if !args.summary && args.extract.is_none() {
        maybe_show_first_run_hint();
    }

    let stealth_config = StealthConfig {
        enabled: args.stealth,
        inject_headers: args.stealth,
        ..Default::default()
    };

    // Load app config (config.toml) so we can pick up persisted LLM settings.
    let app_config = crw_core::config::AppConfig::load().unwrap_or_default();
    let mut cli_extraction_cfg = app_config.extraction.clone();
    let env_cdp_url = std::env::var("CRW_CDP_URL").ok();

    // --extract: inline JSON or `@path/to/schema.json`.
    let extract_schema: Option<serde_json::Value> = match args.extract.as_deref() {
        Some(s) if s.starts_with('@') => {
            let path = &s[1..];
            match std::fs::read_to_string(path) {
                Ok(body) => match serde_json::from_str(&body) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        eprintln!("error: invalid JSON in {path}: {e}");
                        return Err(CmdError::code_only(1));
                    }
                },
                Err(e) => {
                    eprintln!("error: failed to read {path}: {e}");
                    return Err(CmdError::code_only(1));
                }
            }
        }
        Some(s) => match serde_json::from_str(s) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!(
                    "error: --extract is not valid JSON: {e}\n\
                     hint: use @path/to/schema.json for files"
                );
                return Err(CmdError::code_only(1));
            }
        },
        None => None,
    };

    let want_summary = args.summary;
    let want_extract = extract_schema.is_some();

    // Resolve effective LlmConfig: config-first, CLI overrides per-field.
    if want_summary || want_extract {
        let merged = match cli_extraction_cfg.llm.clone() {
            Some(mut cfg) => {
                if let Some(p) = args.llm_provider.clone() {
                    cfg.provider = p;
                }
                if let Some(k) = args.llm_key.clone() {
                    cfg.api_key = k;
                }
                if let Some(m) = args.llm_model.clone() {
                    cfg.model = m;
                }
                if args.llm_base_url.is_some() {
                    cfg.base_url = args.llm_base_url.clone();
                }
                Some(cfg)
            }
            None => {
                // No config — need at minimum provider + key + model on the CLI.
                match (
                    args.llm_provider.clone(),
                    args.llm_key.clone(),
                    args.llm_model.clone(),
                ) {
                    (Some(provider), Some(api_key), Some(model)) => {
                        Some(crw_core::config::LlmConfig {
                            provider,
                            api_key,
                            model,
                            base_url: args.llm_base_url.clone(),
                            ..Default::default()
                        })
                    }
                    _ => None,
                }
            }
        };
        let merged = match merged {
            Some(cfg) => Some(cfg),
            None => match run_inline_llm_setup().await {
                Ok(Some(cfg)) => Some(cfg),
                Ok(None) => {
                    eprintln!("Cancelled. --summary/--extract requires an LLM.");
                    return Err(CmdError::code_only(1));
                }
                Err(e) => {
                    eprintln!("error: LLM setup failed: {e}");
                    eprintln!(
                        "hint: pass --llm-provider/--llm-key/--llm-model, \
                         or add [extraction.llm] to config.toml."
                    );
                    return Err(CmdError::code_only(1));
                }
            },
        };
        cli_extraction_cfg.llm = merged;
    }

    // Request all formats we might need for the output.
    // When --summary/--extract is set, AI output formats are requested and `--format`
    // is ignored; we still include Markdown so phase 1 thinness detection works.
    let request_formats = if want_summary || want_extract {
        let mut v = vec![OutputFormat::Markdown];
        if want_summary {
            v.push(OutputFormat::Summary);
        }
        if want_extract {
            v.push(OutputFormat::Json);
        }
        v
    } else {
        match args.format {
            Format::Markdown => vec![OutputFormat::Markdown],
            Format::Json => vec![
                OutputFormat::Markdown,
                OutputFormat::Html,
                OutputFormat::Links,
            ],
            Format::Html => vec![OutputFormat::Html],
            Format::Rawhtml => vec![OutputFormat::RawHtml],
            Format::Text => vec![OutputFormat::PlainText],
            Format::Links => vec![OutputFormat::Links],
            Format::Images => vec![OutputFormat::Images],
        }
    };

    // Two-phase fetch when `--js` is *not* explicitly set: try HTTP-only first
    // (no browser spawn cost), then escalate to a JS-capable renderer only if
    // the HTTP body extracted to a thin/empty document. This keeps plain-HTML
    // pages (example.com, news articles, blogs) fast while still automatically
    // rendering JS-heavy SPAs (React/Vue/etc.) that return an empty shell.
    //
    // When `--js` is set the user is explicitly opting into JS rendering, so
    // we skip phase 1 and spawn browsers up front.
    let force_js = args.js;

    let mut data: Option<crw_core::types::ScrapeData> = None;
    let mut keep_alive_guards: Vec<crw_renderer::browser::ManagedBrowser> = Vec::new();

    if !force_js {
        // Phase 1: HTTP-only. No browser spawn → no spawn cost on plain pages.
        let http_cfg = RendererConfig {
            mode: RendererMode::None,
            http_timeout_ms: Some(8_000),
            ..Default::default()
        };

        let req = build_request(
            args.url.clone(),
            request_formats.clone(),
            !args.raw,
            None, // render_js = None → auto, but with no JS renderer it stays HTTP
            args.css.clone(),
            args.xpath.clone(),
            args.proxy.clone(),
            args.stealth,
            args.prompt.clone(),
            extract_schema.clone(),
        );

        let http_renderer = match FallbackRenderer::new(
            &http_cfg,
            "crw/0.7.0",
            args.proxy.as_deref(),
            &stealth_config,
        ) {
            Ok(r) => Arc::new(r),
            Err(e) => {
                eprintln!("error: failed to build renderer: {e}");
                return Err(CmdError::code_only(1));
            }
        };

        let http_deadline = crw_core::Deadline::from_request_ms(8_000);
        match scrape_url(
            &req,
            &http_renderer,
            cli_extraction_cfg.llm.as_ref(),
            &cli_extraction_cfg,
            "crw/0.7.0",
            args.stealth,
            None,
            http_deadline,
        )
        .await
        {
            Ok(d) => {
                // Only auto-escalate for the "I just want the page" case
                // (default markdown format, no selectors). Filtered output
                // (--css/--xpath) or specific non-markdown formats are an
                // explicit user choice — measuring "thinness" of a 16-char
                // h1 extraction or a links-only response would always trip
                // the threshold and trigger pointless JS spawns.
                let can_escalate = args.css.is_none()
                    && args.xpath.is_none()
                    && !want_summary
                    && !want_extract
                    && matches!(args.format, Format::Markdown | Format::Json);
                let markdown_len = d.markdown.as_deref().map(str::len).unwrap_or(0);
                let html_text_len = d
                    .plain_text
                    .as_deref()
                    .map(str::len)
                    .unwrap_or_else(|| d.html.as_deref().map(str::len).unwrap_or(0));
                // Same threshold the renderer uses (`is_thin_markdown` < 100).
                // Also catch the "empty SPA shell" case where markdown is empty
                // but the raw HTML is also tiny.
                let is_thin = can_escalate && markdown_len < 100 && html_text_len < 400;
                if is_thin {
                    eprintln!(
                        "info: HTTP returned thin content ({markdown_len} chars markdown), \
                         escalating to JS renderer..."
                    );
                } else {
                    data = Some(d);
                }
            }
            Err(crw_core::error::CrwError::UnsupportedContentType(msg)) => {
                // Same rule as the server ladder: the body is not a web page at
                // all, so spawning LightPanda and Chrome to look at it again
                // only costs the user startup time and prints a "trying JS
                // renderer" line that never had a chance of working.
                eprintln!("error: Unsupported content type: {msg}");
                // `CmdError`, never `process::exit`: `teardown` owns the single
                // exit path so `kill_all_browsers()` runs on every one of them,
                // and it keeps that guarantee structurally rather than by
                // auditing which call sites happen to run before a spawn.
                return Err(CmdError::code_only(1));
            }
            Err(e) => {
                // HTTP-only failure → fall through to JS escalation below.
                eprintln!("info: HTTP fetch failed ({e}), trying JS renderer...");
            }
        }
    }

    // Phase 2 (or sole phase when --js): spawn browsers + run the full
    // HTTP → LightPanda → Chrome fallback chain.
    if data.is_none() {
        // CLI-tuned per-tier timeouts. Server defaults (30s each) assume a
        // long Tower envelope; for interactive CLI we want faster failover so
        // a hanging LightPanda still leaves enough budget for Chrome.
        let mut renderer_config = RendererConfig {
            http_timeout_ms: Some(8_000),
            lightpanda_timeout_ms: Some(12_000),
            chrome_timeout_ms: Some(25_000),
            ..Default::default()
        };

        if let Some(ws_url) = env_cdp_url {
            // Explicit CDP URL — honor it, skip spawn.
            renderer_config.lightpanda = Some(crw_core::config::CdpEndpoint { ws_url });
        } else {
            let browsers = crw_renderer::browser::spawn_all_headless().await;
            if browsers.is_empty() && force_js {
                eprintln!(
                    "warning: --js requested but no browser found. \
                     Install LightPanda or Chrome for JS rendering. \
                     Falling back to HTTP."
                );
            }
            for (guard, ws_url, kind) in browsers {
                match kind {
                    crw_renderer::browser::RendererKind::LightPanda => {
                        renderer_config.lightpanda = Some(crw_core::config::CdpEndpoint { ws_url });
                    }
                    crw_renderer::browser::RendererKind::Chrome => {
                        renderer_config.chrome = Some(crw_core::config::CdpEndpoint { ws_url });
                    }
                }
                keep_alive_guards.push(guard);
            }
            if keep_alive_guards.is_empty()
                && renderer_config.lightpanda.is_none()
                && renderer_config.chrome.is_none()
            {
                renderer_config.mode = RendererMode::None;
            }
        }

        let renderer = match FallbackRenderer::new(
            &renderer_config,
            "crw/0.7.0",
            args.proxy.as_deref(),
            &stealth_config,
        ) {
            Ok(r) => Arc::new(r),
            Err(e) => {
                eprintln!("error: failed to build renderer: {e}");
                return Err(CmdError::code_only(1));
            }
        };

        let req = build_request(
            args.url,
            request_formats,
            !args.raw,
            if force_js { Some(true) } else { None },
            args.css,
            args.xpath,
            args.proxy.clone(),
            args.stealth,
            args.prompt,
            extract_schema,
        );

        // Size the request deadline so the configured renderer ladder
        // (http + lightpanda + chrome + per-tier CDP overhead) can run
        // uncrushed. Mirrors the server's `auto_extend_deadline_for_ladder`.
        let cli_app_config = crw_core::config::AppConfig {
            renderer: renderer_config.clone(),
            request: crw_core::config::RequestConfig {
                deadline_ms_default: 8_000,
                auto_extend_deadline_for_ladder: true,
            },
            ..Default::default()
        };
        let deadline_ms = cli_app_config.effective_deadline_ms(req.deadline_ms, req.wait_for);
        let cli_deadline = crw_core::Deadline::from_request_ms(deadline_ms);

        match scrape_url(
            &req,
            &renderer,
            cli_extraction_cfg.llm.as_ref(),
            &cli_extraction_cfg,
            "crw/0.7.0",
            args.stealth,
            None,
            cli_deadline,
        )
        .await
        {
            Ok(d) => data = Some(d),
            Err(e) => {
                eprintln!("error: {e}");
                return Err(CmdError::code_only(1));
            }
        }
    }

    let data = data.expect("data must be populated by phase 1 or phase 2");
    // Drop guards only after extraction is done so the browser stays alive
    // through the whole fetch + parse pipeline.
    drop(keep_alive_guards);

    // A blocked page is not output. The CLI decided success on the transport
    // alone, which was survivable only while an unclearable wall came back as a
    // transport error; now that the verdict travels on the document, that same
    // check would print an empty body and exit 0, and `--output` would write a
    // zero-byte file over whatever was there.
    //
    // Placed before every format branch below for exactly that reason. The
    // message is the one `/v1/scrape` puts in its `error` field, so the CLI and
    // the API say the same thing about the same page.
    //
    // Scoped to the block classes that leave nothing to print, and only when the
    // caller did not ask for an artifact the API still returns:
    //
    //  - `structural_failure` and `parked_domain` were already reaching the CLI
    //    before this change and printed normally. They are not what this change
    //    is about, and failing on them would silently turn a working
    //    `crw <parked-domain> > out.md` into an empty file and exit 1.
    //
    // No screenshot carve-out: this path runs `scrape_url` locally and the CLI's
    // own `Format` enum never maps to `OutputFormat::Screenshot`, so `data.screenshot`
    // is always `None` here. A guard for it would be dead code claiming a parity
    // with `/v1/scrape` that this code path does not have.
    if let Some(block) = &data.block
        && block.vendor != crw_core::types::STRUCTURAL_FAILURE_VENDOR
        && block.vendor != crw_core::types::PARKED_DOMAIN_VENDOR
    {
        eprintln!("error: {}", block.message());
        return Err(CmdError::code_only(1));
    }

    // AI output paths short-circuit `--format`. The backend populates
    // `data.summary` / `data.json` when those OutputFormats are requested.
    if want_summary {
        let summary = data.summary.clone().unwrap_or_default();
        match args.output {
            Some(path) => {
                if let Err(e) = std::fs::write(&path, &summary) {
                    eprintln!("error: failed to write to {path}: {e}");
                    return Err(CmdError::code_only(1));
                }
            }
            None => print!("{summary}"),
        }
        return Ok(());
    }
    if want_extract {
        let json = data
            .json
            .as_ref()
            .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
            .unwrap_or_default();
        match args.output {
            Some(path) => {
                if let Err(e) = std::fs::write(&path, &json) {
                    eprintln!("error: failed to write to {path}: {e}");
                    return Err(CmdError::code_only(1));
                }
            }
            None => println!("{json}"),
        }
        return Ok(());
    }

    let content = match args.format {
        Format::Markdown => data.markdown.unwrap_or_default(),
        Format::Json => match serde_json::to_string_pretty(&data) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: failed to serialize JSON: {e}");
                return Err(CmdError::code_only(1));
            }
        },
        Format::Html => data.html.unwrap_or_default(),
        Format::Rawhtml => data.raw_html.unwrap_or_default(),
        Format::Text => data.plain_text.unwrap_or_default(),
        Format::Links => data.links.unwrap_or_default().join("\n"),
        Format::Images => data
            .images
            .unwrap_or_default()
            .iter()
            .map(|i| i.url.clone())
            .collect::<Vec<_>>()
            .join("\n"),
    };

    match args.output {
        Some(path) => {
            if let Err(e) = std::fs::write(&path, &content) {
                eprintln!("error: failed to write to {path}: {e}");
                return Err(CmdError::code_only(1));
            }
        }
        None => print!("{content}"),
    }
    Ok(())
}

/// Print one optional-capabilities hint after the first config-free scrape.
/// Idempotent across runs via a dotfile sentinel so users who only need basic
/// scraping are not repeatedly nudged toward setup.
fn maybe_show_first_run_hint() {
    let Some(cfg_path) = crw_core::config::user_config_path() else {
        return;
    };
    if cfg_path.exists() {
        return;
    }
    let sentinel = cfg_path.with_file_name(".first-run-hint-shown");
    if sentinel.exists() {
        return;
    }
    eprintln!("  Optional: `crw setup` connects Cloud or adds local JS/search capabilities.");
    if let Some(parent) = sentinel.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::File::create(&sentinel);
}

/// Triggered when the user invokes `--summary` / `--extract` but no LLM is
/// configured. Asks once whether to set one up now; on yes, runs the same
/// interactive flow `crw setup` uses, writes the result to config.toml so it
/// sticks across runs, and returns the resolved `LlmConfig` to continue the
/// in-flight request.
async fn run_inline_llm_setup()
-> Result<Option<crw_core::config::LlmConfig>, crate::commands::setup::ui::SetupError> {
    use crate::commands::setup::config_file::{
        ExtractionSection, LlmSection, UserConfig, write_user_config,
    };
    use crate::commands::setup::{llm, ui};
    use dialoguer::{Confirm, theme::ColorfulTheme};

    ui::init_color(false);
    println!();
    println!(
        "  --summary / --extract requires an LLM, but none is configured in \
         ~/.config/crw/config.toml."
    );
    let confirm = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Configure one now?")
        .default(true)
        .interact()
        .map_err(ui::handle_dialoguer_error)?;
    if !confirm {
        return Ok(None);
    }

    let result = match llm::run().await? {
        Some(r) => r,
        None => return Ok(None), // user picked "Skip" in the provider list
    };

    // Persist to config.toml so the next `crw … --summary` just works.
    let user_cfg = UserConfig {
        client: None,
        search: None,
        extraction: Some(ExtractionSection {
            llm: Some(LlmSection {
                provider: Some(result.provider.config_value().to_string()),
                api_key: Some(result.api_key.clone()),
                model: Some(result.model.clone()),
                base_url: result.base_url.clone(),
                azure_api_version: result.azure_api_version.clone(),
            }),
        }),
    };
    match write_user_config(user_cfg) {
        Ok(path) => {
            ui::print_success(&format!("Saved to {}", path.display()));
        }
        Err(e) => {
            // Don't bail — we can still run this one request with what we have.
            eprintln!("warning: failed to save config.toml: {e}");
        }
    }

    Ok(Some(crw_core::config::LlmConfig {
        provider: result.provider.config_value().to_string(),
        api_key: result.api_key,
        model: result.model,
        base_url: result.base_url,
        azure_api_version: result.azure_api_version,
        ..Default::default()
    }))
}

/// Parse a local document file (PDF) directly, bypassing the network fetch.
/// Supports markdown/json/text/links output plus `--summary`/`--extract` when a
/// server-side LLM is configured.
async fn run_local_file(args: &ScrapeArgs) -> Result<(), CmdError> {
    let path = args.url.clone();
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: failed to read {path}: {e}");
            return Err(CmdError::code_only(1));
        }
    };

    let is_pdf = path.to_ascii_lowercase().ends_with(".pdf")
        || bytes
            .strip_prefix(&[0xEF, 0xBB, 0xBF])
            .unwrap_or(&bytes)
            .trim_ascii_start()
            .starts_with(b"%PDF-");
    if !is_pdf {
        eprintln!("error: local file parsing currently supports only PDF files (got {path})");
        return Err(CmdError::code_only(1));
    }

    let want_summary = args.summary;
    let want_extract = args.extract.is_some();

    // --extract schema (inline JSON or @path), mirroring the URL path.
    let extract_schema: Option<serde_json::Value> = match args.extract.as_deref() {
        Some(s) if s.starts_with('@') => match std::fs::read_to_string(&s[1..]) {
            Ok(body) => serde_json::from_str(&body).ok(),
            Err(e) => {
                eprintln!("error: failed to read {}: {e}", &s[1..]);
                return Err(CmdError::code_only(1));
            }
        },
        Some(s) => match serde_json::from_str(s) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("error: --extract is not valid JSON: {e}");
                return Err(CmdError::code_only(1));
            }
        },
        None => None,
    };

    let app_config = crw_core::config::AppConfig::load().unwrap_or_default();

    let formats = if want_summary || want_extract {
        let mut v = vec![OutputFormat::Markdown];
        if want_summary {
            v.push(OutputFormat::Summary);
        }
        if want_extract {
            v.push(OutputFormat::Json);
        }
        v
    } else {
        match args.format {
            Format::Markdown => vec![OutputFormat::Markdown],
            Format::Json => vec![OutputFormat::Markdown],
            Format::Html | Format::Rawhtml => vec![OutputFormat::Markdown],
            Format::Text => vec![OutputFormat::PlainText],
            Format::Links => vec![OutputFormat::Links],
            // PDFs carry no HTML image sources; request it anyway (returns empty).
            Format::Images => vec![OutputFormat::Images],
        }
    };

    let req = ScrapeRequest {
        formats,
        json_schema: extract_schema,
        summary_prompt: args.prompt.clone(),
        ..Default::default()
    };

    let source = crw_crawl::pdf::PdfSource {
        source_url: format!("file://{path}"),
        status_code: 200,
        elapsed_ms: 0,
        source_filename: std::path::Path::new(&path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned()),
    };

    let mut data = match crw_crawl::pdf::convert_pdf_bytes(bytes, &req, source).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: PDF conversion failed: {e}");
            return Err(CmdError::code_only(1));
        }
    };

    if (want_summary || want_extract)
        && let Err(e) =
            crw_crawl::pdf::apply_llm_formats(&mut data, &req, app_config.extraction.llm.as_ref())
                .await
    {
        eprintln!("error: {e}");
        eprintln!(
            "hint: pass --llm-provider/--llm-key/--llm-model, or add [extraction.llm] to config.toml."
        );
        return Err(CmdError::code_only(1));
    }

    for w in &data.warnings {
        eprintln!("warning: {w}");
    }

    let content = if want_summary {
        data.summary.unwrap_or_default()
    } else if want_extract {
        data.json
            .as_ref()
            .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
            .unwrap_or_default()
    } else {
        match args.format {
            Format::Markdown => data.markdown.unwrap_or_default(),
            Format::Json => serde_json::to_string_pretty(&data).unwrap_or_default(),
            Format::Html | Format::Rawhtml => {
                eprintln!("warning: HTML output is unavailable for PDF; returning markdown");
                data.markdown.unwrap_or_default()
            }
            Format::Text => data.plain_text.unwrap_or_default(),
            Format::Links => data.links.unwrap_or_default().join("\n"),
            Format::Images => data
                .images
                .unwrap_or_default()
                .iter()
                .map(|i| i.url.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    };

    match &args.output {
        Some(p) => {
            if let Err(e) = std::fs::write(p, &content) {
                eprintln!("error: failed to write to {p}: {e}");
                return Err(CmdError::code_only(1));
            }
        }
        None => println!("{content}"),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_request(
    url: String,
    formats: Vec<OutputFormat>,
    only_main_content: bool,
    render_js: Option<bool>,
    css: Option<String>,
    xpath: Option<String>,
    proxy: Option<String>,
    stealth: bool,
    summary_prompt: Option<String>,
    extract_schema: Option<serde_json::Value>,
) -> ScrapeRequest {
    let extract = extract_schema
        .clone()
        .map(|s| crw_core::types::ExtractOptions {
            schema: Some(s),
            prompt: None,
        });
    ScrapeRequest {
        url,
        formats,
        only_main_content,
        render_js,
        wait_for: None,
        include_tags: vec![],
        exclude_tags: vec![],
        json_schema: extract_schema,
        basis: false,
        headers: HashMap::new(),
        css_selector: css,
        xpath,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        proxy,
        proxy_list: Vec::new(),
        proxy_rotation: None,
        country: None,
        stealth: if stealth { Some(true) } else { None },
        actions: None,
        extract,
        llm_api_key: None,
        llm_provider: None,
        llm_model: None,
        base_url: None,
        summary_prompt,
        max_content_chars: None,
        renderer: None,
        force_cloak: None,
        deadline_ms: None,
        debug: None,
        change_tracking: None,
        goal: None,
        judge_enabled: None,
        parsers: None,
        screenshot_full_page: false,
        // The CLI is a one-shot process: it cannot hit a cache it just built,
        // and someone running `crw scrape` twice wants the page as it is now.
        max_age: Some(0),
        cache_scope: None,
        store_in_cache: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Args as ClapArgs, FromArgMatches, ValueEnum};

    // ---- clap parsing helpers -------------------------------------------

    fn parse(argv: &[&str]) -> Result<ScrapeArgs, clap::Error> {
        let mut full = vec!["scrape"];
        full.extend_from_slice(argv);
        let cmd = ScrapeArgs::augment_args(clap::Command::new("scrape"));
        let matches = cmd.try_get_matches_from(full)?;
        ScrapeArgs::from_arg_matches(&matches)
    }

    fn format_name(f: &Format) -> String {
        f.to_possible_value()
            .expect("Format always has a possible_value")
            .get_name()
            .to_string()
    }

    // ---- argument parsing: defaults & positional url ---------------------

    #[test]
    fn bare_url_parses_with_all_defaults() {
        let args = parse(&["https://example.com"]).unwrap();
        assert_eq!(args.url, "https://example.com");
        assert_eq!(format_name(&args.format), "markdown");
        assert!(args.output.is_none());
        assert!(!args.raw);
        assert!(!args.js);
        assert!(args.css.is_none());
        assert!(args.xpath.is_none());
        assert!(args.proxy.is_none());
        assert!(!args.stealth);
        assert!(!args.summary);
        assert!(args.prompt.is_none());
        assert!(args.extract.is_none());
        assert!(args.llm_provider.is_none());
        assert!(args.llm_key.is_none());
        assert!(args.llm_model.is_none());
        assert!(args.llm_base_url.is_none());
    }

    #[test]
    fn missing_url_positional_is_a_clap_error() {
        let err = match parse(&[]) {
            Ok(_) => panic!("expected a parse error"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn url_positional_accepts_a_non_url_string_unvalidated() {
        // Argument parsing does no URL validation — that happens at runtime
        // in `run()` (auto-https-prepend / local-file check). Any string
        // must parse as the positional `url`.
        let args = parse(&["not a url at all"]).unwrap();
        assert_eq!(args.url, "not a url at all");
    }

    // ---- --format / -f ----------------------------------------------------

    #[test]
    fn format_accepts_every_documented_value() {
        for (flag_value, expected_name) in [
            ("markdown", "markdown"),
            ("json", "json"),
            ("html", "html"),
            ("rawhtml", "rawhtml"),
            ("text", "text"),
            ("links", "links"),
            ("images", "images"),
        ] {
            let args = parse(&["--format", flag_value, "https://example.com"]).unwrap();
            assert_eq!(format_name(&args.format), expected_name, "for {flag_value}");
        }
    }

    #[test]
    fn format_short_flag_is_equivalent_to_long_flag() {
        let args = parse(&["-f", "json", "https://example.com"]).unwrap();
        assert_eq!(format_name(&args.format), "json");
    }

    #[test]
    fn format_rejects_summary_as_invalid() {
        // The setup wizard once wrongly advertised `-f summary`; it is not
        // (and must never become) a valid --format value. AI output modes
        // are their own flag (`--summary`), not a format.
        let err = match parse(&["--format", "summary", "https://example.com"]) {
            Ok(_) => panic!("expected a parse error"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn format_rejects_unknown_value_with_helpful_message() {
        let err = match parse(&["--format", "yaml", "https://example.com"]) {
            Ok(_) => panic!("expected a parse error"),
            Err(e) => e,
        };
        let rendered = err.to_string();
        assert!(rendered.contains("yaml"));
        // clap lists the valid values in the error to guide the user.
        assert!(rendered.contains("markdown"));
    }

    #[test]
    fn format_is_case_sensitive() {
        let err = match parse(&["--format", "Markdown", "https://example.com"]) {
            Ok(_) => panic!("expected a parse error"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn format_flag_cannot_be_repeated() {
        // -f is a single-value arg, so clap rejects a second occurrence rather
        // than letting the last one win.
        let err = match parse(&["-f", "json", "-f", "text", "https://example.com"]) {
            Ok(_) => panic!("expected a parse error on a repeated --format"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn format_has_exactly_the_seven_known_variants() {
        let names: Vec<String> = Format::value_variants().iter().map(format_name).collect();
        assert_eq!(
            names,
            vec![
                "markdown", "json", "html", "rawhtml", "text", "links", "images"
            ]
        );
    }

    // ---- other flags --------------------------------------------------

    #[test]
    fn output_short_and_long_flag_set_the_path() {
        let args = parse(&["-o", "out.md", "https://example.com"]).unwrap();
        assert_eq!(args.output.as_deref(), Some("out.md"));
        let args = parse(&["--output", "out2.md", "https://example.com"]).unwrap();
        assert_eq!(args.output.as_deref(), Some("out2.md"));
    }

    #[test]
    fn boolean_flags_default_off_and_flip_on_when_present() {
        let args = parse(&["--raw", "--js", "--stealth", "https://example.com"]).unwrap();
        assert!(args.raw);
        assert!(args.js);
        assert!(args.stealth);
    }

    #[test]
    fn css_and_xpath_accept_selector_values_including_unicode() {
        let args = parse(&[
            "--css",
            "div.日本語-title",
            "--xpath",
            "//h1[@id='título']",
            "https://example.com",
        ])
        .unwrap();
        assert_eq!(args.css.as_deref(), Some("div.日本語-title"));
        assert_eq!(args.xpath.as_deref(), Some("//h1[@id='título']"));
    }

    #[test]
    fn proxy_accepts_a_url_value() {
        let args = parse(&[
            "--proxy",
            "socks5://user:pass@host:1080",
            "https://example.com",
        ])
        .unwrap();
        assert_eq!(args.proxy.as_deref(), Some("socks5://user:pass@host:1080"));
    }

    #[test]
    fn llm_override_flags_all_accept_values() {
        let args = parse(&[
            "--llm-provider",
            "anthropic",
            "--llm-key",
            "sk-test",
            "--llm-model",
            "claude-sonnet-4-20250514",
            "--llm-base-url",
            "https://api.anthropic.com",
            "https://example.com",
        ])
        .unwrap();
        assert_eq!(args.llm_provider.as_deref(), Some("anthropic"));
        assert_eq!(args.llm_key.as_deref(), Some("sk-test"));
        assert_eq!(args.llm_model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(
            args.llm_base_url.as_deref(),
            Some("https://api.anthropic.com")
        );
    }

    // ---- --summary / --extract / --prompt interplay -----------------------

    #[test]
    fn summary_and_extract_conflict() {
        let err = match parse(&["--summary", "--extract", "{}", "https://example.com"]) {
            Ok(_) => panic!("expected a parse error"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn prompt_without_summary_is_a_missing_required_argument() {
        let err = match parse(&["--prompt", "in 3 bullets", "https://example.com"]) {
            Ok(_) => panic!("expected a parse error"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn prompt_with_summary_parses_fine() {
        let args = parse(&[
            "--summary",
            "--prompt",
            "in 3 bullets",
            "https://example.com",
        ])
        .unwrap();
        assert!(args.summary);
        assert_eq!(args.prompt.as_deref(), Some("in 3 bullets"));
    }

    #[test]
    fn extract_alone_parses_without_requiring_summary() {
        let args = parse(&["--extract", "{\"type\":\"object\"}", "https://example.com"]).unwrap();
        assert_eq!(args.extract.as_deref(), Some("{\"type\":\"object\"}"));
        assert!(!args.summary);
    }

    // ---- build_request(): pure field-mapping tests -------------------------

    #[test]
    fn build_request_maps_simple_fields_through_unchanged() {
        let req = build_request(
            "https://example.com".to_string(),
            vec![OutputFormat::Markdown, OutputFormat::Links],
            true,
            None,
            Some("div.main".to_string()),
            Some("//h1".to_string()),
            Some("http://proxy:8080".to_string()),
            false,
            Some("as a haiku".to_string()),
            None,
        );
        assert_eq!(req.url, "https://example.com");
        assert_eq!(
            req.formats,
            vec![OutputFormat::Markdown, OutputFormat::Links]
        );
        assert!(req.only_main_content);
        assert_eq!(req.render_js, None);
        assert_eq!(req.css_selector.as_deref(), Some("div.main"));
        assert_eq!(req.xpath.as_deref(), Some("//h1"));
        assert_eq!(req.proxy.as_deref(), Some("http://proxy:8080"));
        assert_eq!(req.summary_prompt.as_deref(), Some("as a haiku"));
    }

    #[test]
    fn build_request_stealth_true_sets_some_true() {
        let req = build_request(
            "https://example.com".to_string(),
            vec![OutputFormat::Markdown],
            true,
            None,
            None,
            None,
            None,
            true,
            None,
            None,
        );
        assert_eq!(req.stealth, Some(true));
    }

    #[test]
    fn build_request_stealth_false_sets_none_not_some_false() {
        // Deliberately not `Some(false)`: downstream code treats "stealth
        // configured at all" as a signal, so a false stealth flag must look
        // identical to "never asked for stealth".
        let req = build_request(
            "https://example.com".to_string(),
            vec![OutputFormat::Markdown],
            true,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
        );
        assert_eq!(req.stealth, None);
    }

    #[test]
    fn build_request_render_js_forced_true_when_js_flag_set() {
        let req = build_request(
            "https://example.com".to_string(),
            vec![OutputFormat::Markdown],
            true,
            Some(true),
            None,
            None,
            None,
            false,
            None,
            None,
        );
        assert_eq!(req.render_js, Some(true));
    }

    #[test]
    fn build_request_no_extract_schema_leaves_extract_and_json_schema_none() {
        let req = build_request(
            "https://example.com".to_string(),
            vec![OutputFormat::Markdown],
            true,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
        );
        assert!(req.extract.is_none());
        assert!(req.json_schema.is_none());
    }

    #[test]
    fn build_request_extract_schema_populates_both_extract_and_json_schema() {
        let schema =
            serde_json::json!({"type": "object", "properties": {"title": {"type": "string"}}});
        let req = build_request(
            "https://example.com".to_string(),
            vec![OutputFormat::Markdown, OutputFormat::Json],
            true,
            None,
            None,
            None,
            None,
            false,
            None,
            Some(schema.clone()),
        );
        assert_eq!(req.json_schema, Some(schema.clone()));
        let extract = req.extract.expect("extract must be populated");
        assert_eq!(extract.schema, Some(schema));
        assert!(
            extract.prompt.is_none(),
            "build_request never sets ExtractOptions::prompt (that's summary_prompt's job)"
        );
    }

    #[test]
    fn build_request_only_main_content_false_when_raw() {
        let req = build_request(
            "https://example.com".to_string(),
            vec![OutputFormat::Markdown],
            false,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
        );
        assert!(!req.only_main_content);
    }

    #[test]
    fn build_request_leaves_unwired_fields_at_their_zero_value() {
        // These fields have no CLI flag wiring them up yet; a future accidental
        // wiring should show up as a failing test here, not ship silently.
        let schema = serde_json::json!({"type": "object"});
        let req = build_request(
            "https://example.com".to_string(),
            vec![OutputFormat::Markdown],
            true,
            Some(true),
            Some("div".to_string()),
            Some("//p".to_string()),
            Some("http://proxy:1".to_string()),
            true,
            Some("prompt".to_string()),
            Some(schema),
        );
        assert!(!req.basis);
        assert!(req.headers.is_empty());
        assert!(req.include_tags.is_empty());
        assert!(req.exclude_tags.is_empty());
        assert!(req.chunk_strategy.is_none());
        assert!(req.query.is_none());
        assert!(req.filter_mode.is_none());
        assert!(req.top_k.is_none());
        assert!(req.proxy_list.is_empty());
        assert!(req.proxy_rotation.is_none());
        assert!(req.country.is_none());
        assert!(req.actions.is_none());
        assert!(req.llm_api_key.is_none());
        assert!(req.llm_provider.is_none());
        assert!(req.llm_model.is_none());
        assert!(req.base_url.is_none());
        assert!(req.max_content_chars.is_none());
        assert!(req.renderer.is_none());
        assert!(req.force_cloak.is_none());
        assert!(req.deadline_ms.is_none());
        assert!(req.debug.is_none());
        assert!(req.change_tracking.is_none());
        assert!(req.goal.is_none());
        assert!(req.judge_enabled.is_none());
        assert!(req.parsers.is_none());
        assert!(!req.screenshot_full_page);
    }

    #[test]
    fn build_request_empty_formats_vec_round_trips_empty() {
        let req = build_request(
            "https://example.com".to_string(),
            vec![],
            true,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
        );
        assert!(req.formats.is_empty());
    }

    // ---- run_local_file(): is-PDF sniffing, all offline (no network, no LLM) --

    fn temp_file(label: &str, contents: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "crw-scrape-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn base_scrape_args(url: String) -> ScrapeArgs {
        ScrapeArgs {
            url,
            format: Format::Markdown,
            output: None,
            raw: false,
            js: false,
            css: None,
            xpath: None,
            proxy: None,
            stealth: false,
            summary: false,
            prompt: None,
            extract: None,
            llm_provider: None,
            llm_key: None,
            llm_model: None,
            llm_base_url: None,
        }
    }

    #[tokio::test]
    async fn run_local_file_rejects_a_plain_non_pdf_file() {
        let path = temp_file("not-pdf", b"just some plain text, not a PDF");
        let args = base_scrape_args(path.display().to_string());
        let err = run_local_file(&args).await.unwrap_err();
        assert_eq!(err.code, 1);
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn run_local_file_routes_by_pdf_extension_even_with_non_pdf_bytes() {
        // `.pdf` extension alone is enough to route into the PDF parser,
        // regardless of content. `convert_pdf_bytes` soft-fails (never
        // returns Err) on unparsable bytes, so this must still return Ok
        // with an empty/warned result rather than the "only PDF files"
        // rejection.
        let dir = std::env::temp_dir().join(format!(
            "crw-scrape-pdf-ext-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("document.pdf");
        std::fs::write(&path, b"not actually pdf bytes").unwrap();
        let args = base_scrape_args(path.display().to_string());
        let result = run_local_file(&args).await;
        assert!(
            result.is_ok(),
            "extension-routed PDF path must soft-fail, not error: {result:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_local_file_routes_by_pdf_magic_bytes_without_extension() {
        // No `.pdf` extension, but the `%PDF-` magic header alone is enough
        // to route into the PDF parser.
        let path = temp_file("magic-noext", b"%PDF-1.4 not a real pdf body");
        let args = base_scrape_args(path.display().to_string());
        let result = run_local_file(&args).await;
        assert!(
            result.is_ok(),
            "magic-byte-routed PDF path must soft-fail, not error: {result:?}"
        );
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn run_local_file_routes_by_pdf_magic_bytes_behind_a_utf8_bom() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"%PDF-1.4 also fake");
        let path = temp_file("magic-bom", &bytes);
        let args = base_scrape_args(path.display().to_string());
        let result = run_local_file(&args).await;
        assert!(
            result.is_ok(),
            "BOM + magic-byte PDF path must soft-fail, not error: {result:?}"
        );
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn run_local_file_missing_file_is_a_read_error() {
        let missing = std::env::temp_dir().join("crw-scrape-test-does-not-exist.pdf");
        let args = base_scrape_args(missing.display().to_string());
        let err = run_local_file(&args).await.unwrap_err();
        assert_eq!(err.code, 1);
    }

    #[tokio::test]
    async fn run_dispatches_existing_local_file_before_any_network_prep() {
        // Proves `run()`'s own `is_file()` short-circuit is reached: a
        // non-PDF local file must produce the same "only PDF" rejection as
        // calling `run_local_file` directly, never a network attempt.
        let path = temp_file("dispatch", b"plain text, not a pdf, not a url");
        let args = base_scrape_args(path.display().to_string());
        let err = run(args).await.unwrap_err();
        assert_eq!(err.code, 1);
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn run_local_file_writes_output_to_file_when_output_is_set() {
        let path = temp_file("output-flag", b"not a pdf");
        let out_dir = std::env::temp_dir().join(format!(
            "crw-scrape-out-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&out_dir).unwrap();
        let out_path = out_dir.join("result.md");

        let mut args = base_scrape_args(path.display().to_string());
        args.format = Format::Text;
        args.output = Some(out_path.display().to_string());
        // This is still rejected before any output is written (non-PDF).
        let err = run_local_file(&args).await.unwrap_err();
        assert_eq!(err.code, 1);
        assert!(
            !out_path.exists(),
            "rejected input must not produce an output file"
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_dir_all(&out_dir).ok();
    }
}
