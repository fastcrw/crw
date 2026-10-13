//! Shared PDF → [`ScrapeData`] conversion, reused by every surface that can
//! encounter a PDF: the single-scrape path (`single.rs`), the crawl path
//! (`crawl.rs`), and the REST upload endpoint (`/v2/parse`), plus CLI and MCP.
//!
//! The heavy lifting (lopdf parse via `pdf-inspector`) is CPU-bound and
//! synchronous, so it runs inside [`tokio::task::spawn_blocking`] on an OWNED
//! `Vec<u8>` — borrowing the bytes across the `'static` spawn boundary does not
//! compile, and running the parse on an async worker would stall the runtime
//! for large documents.

use std::sync::OnceLock;
use std::time::Duration;

use crw_core::config::{DocumentConfig, LlmConfig};
use crw_core::error::{CrwError, CrwResult};
use crw_core::types::{
    ChunkResult, LlmUsage, OutputFormat, PageMetadata, ParserSpec, ScrapeData, ScrapeRequest,
};
use crw_extract::pdf::{PdfError, PdfExtract};

/// Process-wide PDF-parse limits, shared by every surface (URL scrape, crawl,
/// batch, upload). Configured once at server startup via [`configure_limits`];
/// callers that never configure (CLI, tests) get the safe defaults below.
struct ParseLimits {
    /// Caps concurrent parses → bounds peak CPU and decompressed memory. A
    /// reserved lane keeps interactive PDF scrapes from queuing behind a batch
    /// that fills every parse slot (same shape as the extract pool).
    sem: crw_core::ReservedSemaphore,
    /// Per-parse wall-clock budget (`None` = disabled).
    timeout: Option<Duration>,
    /// Hard server-side page cap (0 = unlimited), combined with any per-request
    /// `maxPages` via min().
    max_pages_cap: usize,
    /// Decompression-bomb cap (max decompressed bytes; 0 = disabled).
    max_decompressed_bytes: usize,
    /// Run each parse in an isolated child process (Unix only).
    sandbox: bool,
    /// Child address-space limit (bytes) when `sandbox` is on.
    sandbox_memory_bytes: u64,
}

static LIMITS: OnceLock<ParseLimits> = OnceLock::new();

/// Install the document-parse limits from `[document]` config. Idempotent —
/// the first call wins (subsequent calls are ignored), so call it once at boot.
pub fn configure_limits(cfg: &DocumentConfig) {
    let _ = LIMITS.set(ParseLimits {
        sem: crw_core::ReservedSemaphore::new(
            cfg.max_concurrent_parses.max(1),
            crw_core::config::resolve_interactive_reserve(
                cfg.reserved_interactive_parses,
                cfg.max_concurrent_parses.max(1),
            ),
            "pdf",
        ),
        timeout: (cfg.parse_timeout_ms > 0).then(|| Duration::from_millis(cfg.parse_timeout_ms)),
        max_pages_cap: cfg.max_pages,
        max_decompressed_bytes: cfg.max_decompressed_bytes,
        sandbox: cfg.sandbox,
        sandbox_memory_bytes: cfg.sandbox_memory_bytes,
    });
}

fn limits() -> &'static ParseLimits {
    LIMITS.get_or_init(|| ParseLimits {
        // Default total 4, reserve 1 for interactive (floored, batch_gate=3).
        sem: crw_core::ReservedSemaphore::new(4, 1, "pdf"),
        timeout: Some(Duration::from_millis(30_000)),
        max_pages_cap: 0,
        max_decompressed_bytes: 104_857_600, // 100 MiB
        sandbox: false,
        sandbox_memory_bytes: 536_870_912,
    })
}

/// Combine a per-request `maxPages` with the server cap: the smaller wins; `0`
/// means unlimited on either side.
fn effective_max_pages(req_max: Option<usize>, cap: usize) -> Option<usize> {
    match (req_max, cap) {
        (Some(r), 0) => Some(r),
        (Some(r), c) => Some(r.min(c)),
        (None, 0) => None,
        (None, c) => Some(c),
    }
}

/// The single choke point for running pdf-inspector: acquires a concurrency
/// permit (held until the blocking thread actually finishes, even if we time
/// out the await), runs the parse on the blocking pool, and enforces the
/// wall-clock budget. Every conversion path funnels through here.
async fn run_parse(
    bytes: Vec<u8>,
    want_plaintext: bool,
    max_pages: Option<usize>,
) -> Result<PdfExtract, PdfError> {
    let lim = limits();
    let max_pages = effective_max_pages(max_pages, lim.max_pages_cap);
    let max_decompressed = lim.max_decompressed_bytes;

    // Acquire BEFORE spawning; move the permit into the closure so it is held
    // for the full duration of the (possibly orphaned-after-timeout) parse —
    // this keeps the semaphore an honest bound on real concurrent CPU work.
    // Read the traffic class on the async side (not inside spawn_blocking) so
    // interactive PDF scrapes take the reserved lane.
    let permit = lim.sem.acquire(crw_core::current_scrape_class()).await;

    // Sandbox path (Unix): run the parse in an isolated child process. The
    // permit is held for the whole call so the concurrency bound still holds.
    #[cfg(unix)]
    if lim.sandbox {
        let _permit = permit;
        return parse_in_subprocess(
            bytes,
            want_plaintext,
            max_pages,
            max_decompressed,
            lim.sandbox_memory_bytes,
            lim.timeout,
        )
        .await;
    }

    let handle = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        crw_extract::pdf::convert(&bytes, want_plaintext, max_pages, max_decompressed)
    });

    match lim.timeout {
        Some(budget) => match tokio::time::timeout(budget, handle).await {
            Ok(Ok(res)) => res,
            Ok(Err(join)) => Err(PdfError::Corrupt(format!("parse task failed: {join}"))),
            // The blocking thread keeps running (and still holds its permit), so
            // it can't be reused for new work until it actually finishes — but
            // the request is freed immediately.
            Err(_elapsed) => Err(PdfError::Timeout),
        },
        None => match handle.await {
            Ok(res) => res,
            Err(join) => Err(PdfError::Corrupt(format!("parse task failed: {join}"))),
        },
    }
}

/// Hidden argv sentinel that puts a binary into sandbox-worker mode.
const SANDBOX_ARG: &str = "__crw_pdf_worker";

/// Call at the VERY TOP of `main()` in every binary that may parse PDFs with
/// sandboxing on (i.e. `crw-server`). If this process was spawned as a sandbox
/// worker, it reads the PDF from stdin, converts it, writes a JSON result to
/// stdout, and exits — never returning to normal startup. No-op otherwise.
pub fn run_sandbox_worker_if_invoked() {
    let mut args = std::env::args();
    let _ = args.next(); // argv[0]
    if args.next().as_deref() != Some(SANDBOX_ARG) {
        return;
    }
    let want_pt = args.next().as_deref() == Some("1");
    let max_pages = args
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0);
    let max_decompressed = args
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    use std::io::{Read, Write};
    let mut bytes = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut bytes);

    let json = match crw_extract::pdf::convert(&bytes, want_pt, max_pages, max_decompressed) {
        Ok(extract) => serde_json::json!({ "ok": true, "extract": extract }),
        Err(e) => serde_json::json!({ "ok": false, "code": e.code() }),
    };
    let out = serde_json::to_vec(&json).unwrap_or_default();
    let _ = std::io::stdout().write_all(&out);
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

/// Run one parse in an isolated child process: hard `RLIMIT_AS` memory ceiling
/// and `RLIMIT_CPU`, no inherited env/secrets, killed on timeout. A crash, OOM,
/// or hypothetical RCE is contained to the child; the server survives.
#[cfg(unix)]
async fn parse_in_subprocess(
    bytes: Vec<u8>,
    want_plaintext: bool,
    max_pages: Option<usize>,
    max_decompressed: usize,
    mem_limit: u64,
    timeout: Option<Duration>,
) -> Result<PdfExtract, PdfError> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    use tokio::io::AsyncWriteExt;

    let exe = std::env::current_exe()
        .map_err(|e| PdfError::Corrupt(format!("sandbox: current_exe: {e}")))?;
    let cpu_secs: u64 = timeout.map(|d| d.as_secs().saturating_add(5)).unwrap_or(0);

    let mut std_cmd = std::process::Command::new(exe);
    std_cmd
        .arg(SANDBOX_ARG)
        .arg(if want_plaintext { "1" } else { "0" })
        .arg(max_pages.unwrap_or(0).to_string())
        .arg(max_decompressed.to_string())
        .env_clear() // child inherits NO secrets / config
        // Bound the child's VIRTUAL address space so a legitimately small PDF
        // doesn't trip `RLIMIT_AS` and abort. pdf-inspector parses with rayon, and
        // glibc spawns up to 8×ncpu malloc arenas reserving ~64 MiB of virtual
        // space EACH — on a multi-core host that alone blows past a 512 MiB
        // `RLIMIT_AS`, the alloc fails, the child SIGABRTs, and the parent
        // misreports it as `pdf_too_large`. Capping arenas + rayon threads keeps
        // virtual usage far under the cap while real RSS stays tiny.
        .env("MALLOC_ARENA_MAX", "2")
        .env("RAYON_NUM_THREADS", "2")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    // SAFETY: only async-signal-safe libc calls (setrlimit) run post-fork/pre-exec.
    unsafe {
        std_cmd.pre_exec(move || {
            set_rlimit(libc::RLIMIT_AS as libc::c_int, mem_limit);
            if cpu_secs > 0 {
                set_rlimit(libc::RLIMIT_CPU as libc::c_int, cpu_secs);
            }
            Ok(())
        });
    }

    let mut cmd = tokio::process::Command::from(std_cmd);
    cmd.kill_on_drop(true); // timeout / early-return → child is reaped + killed
    let mut child = cmd
        .spawn()
        .map_err(|e| PdfError::Corrupt(format!("sandbox: spawn: {e}")))?;

    // Feed the PDF on a concurrent task so a full 50 MB pipe can't deadlock.
    if let Some(mut stdin) = child.stdin.take() {
        tokio::spawn(async move {
            let _ = stdin.write_all(&bytes).await;
            let _ = stdin.shutdown().await;
        });
    }

    let wait = child.wait_with_output();
    let output = match timeout {
        Some(budget) => match tokio::time::timeout(budget, wait).await {
            Ok(r) => r,
            Err(_) => return Err(PdfError::Timeout), // child killed on drop
        },
        None => wait.await,
    }
    .map_err(|e| PdfError::Corrupt(format!("sandbox: io: {e}")))?;

    if !output.status.success() {
        return Err(classify_worker_exit(&output.status));
    }
    parse_worker_output(&output.stdout)
}

/// Map a non-zero / signalled worker exit to a `PdfError`. A memory-limit hit
/// (`RLIMIT_AS` → allocation abort, SIGABRT) or OOM-kill (SIGKILL) → `TooLarge`;
/// CPU-limit (SIGXCPU) → `Timeout`; anything else → `Corrupt`.
#[cfg(unix)]
fn classify_worker_exit(status: &std::process::ExitStatus) -> PdfError {
    use std::os::unix::process::ExitStatusExt;
    match status.signal() {
        Some(libc::SIGABRT) | Some(libc::SIGKILL) => PdfError::TooLarge,
        Some(libc::SIGXCPU) => PdfError::Timeout,
        _ => PdfError::Corrupt("sandbox worker exited abnormally".to_string()),
    }
}

/// Decode the worker's JSON result envelope.
#[cfg(unix)]
fn parse_worker_output(out: &[u8]) -> Result<PdfExtract, PdfError> {
    let v: serde_json::Value = serde_json::from_slice(out)
        .map_err(|e| PdfError::Corrupt(format!("sandbox: bad worker output: {e}")))?;
    if v.get("ok").and_then(|b| b.as_bool()) == Some(true) {
        serde_json::from_value(v.get("extract").cloned().unwrap_or_default())
            .map_err(|e| PdfError::Corrupt(format!("sandbox: decode extract: {e}")))
    } else {
        let code = v.get("code").and_then(|c| c.as_str()).unwrap_or("");
        Err(PdfError::from_code(code))
    }
}

/// Set a soft+hard `rlimit`. Best-effort: failures are ignored (the child still
/// runs, just without that particular cap).
#[cfg(unix)]
fn set_rlimit(resource: libc::c_int, value: u64) {
    let rl = libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    };
    // `resource as _` coerces to the platform's expected type (c_int on macOS,
    // __rlimit_resource_t on Linux).
    unsafe {
        libc::setrlimit(resource as _, &rl);
    }
}

/// Provenance for a PDF conversion, abstracting over URL-sourced fetches and
/// uploaded files so [`convert_pdf_bytes`] serves both.
#[derive(Debug, Clone)]
pub struct PdfSource {
    /// Source URL (or `upload://<name>` pseudo-URL for uploads).
    pub source_url: String,
    /// HTTP status of the originating fetch (200 for uploads).
    pub status_code: u16,
    /// Fetch/handling time in ms (0 for uploads — conversion time is added).
    pub elapsed_ms: u64,
    /// Original filename for uploads; `None` for URL fetches.
    pub source_filename: Option<String>,
}

/// Whether PDF parsing is requested for this scrape, per the Firecrawl
/// `parsers` semantics:
/// - field omitted (`None`) → parse (default-on, matches Firecrawl),
/// - explicit empty list → do NOT parse (leave raw),
/// - non-empty list → parse iff it contains a `pdf` entry.
pub fn pdf_parse_requested(req: &ScrapeRequest) -> bool {
    match &req.parsers {
        None => true,
        Some(list) if list.is_empty() => false,
        Some(list) => list
            .iter()
            .any(|p| p.parser_type.eq_ignore_ascii_case("pdf")),
    }
}

/// Locate the request's `pdf` parser directive, if any.
fn pdf_spec(req: &ScrapeRequest) -> Option<&ParserSpec> {
    req.parsers.as_ref().and_then(|list| {
        list.iter()
            .find(|p: &&ParserSpec| p.parser_type.eq_ignore_ascii_case("pdf"))
    })
}

/// The `maxPages` cap from the request's `pdf` parser entry, if any.
fn pdf_max_pages(req: &ScrapeRequest) -> Option<usize> {
    pdf_spec(req).and_then(|p| p.max_pages)
}

/// The requested parsing `mode` (Firecrawl: auto|fast|ocr), lowercased.
fn pdf_mode(req: &ScrapeRequest) -> Option<String> {
    pdf_spec(req).and_then(|p| p.mode.as_ref().map(|m| m.to_ascii_lowercase()))
}

/// Map a [`PdfError`] to a [`CrwError`]. Used by callers (e.g. the upload
/// endpoint) that want a hard failure; the URL path instead degrades to a
/// soft warning and never surfaces these.
pub fn pdf_error_to_crw(e: &PdfError) -> CrwError {
    match e {
        // The bytes aren't a PDF at all → client error (400).
        PdfError::NotAPdf => CrwError::InvalidRequest(e.to_string()),
        // Recognized but unprocessable (encrypted / corrupt / disabled / too
        // slow) → 422. A timeout is a per-document budget overrun, surfaced to
        // the uploader as "couldn't process this file", not a gateway error.
        PdfError::Encrypted
        | PdfError::Corrupt(_)
        | PdfError::Disabled
        | PdfError::Timeout
        | PdfError::TooLarge => CrwError::ExtractionError(e.to_string()),
    }
}

/// Convert raw PDF bytes into a populated [`ScrapeData`].
///
/// On a parse failure this returns `Ok` with empty markdown and a warning
/// (so a URL scrape that merely *failed to convert* still succeeds with
/// metadata); callers that need a hard error (upload) can inspect
/// `data.warnings` / re-run via [`convert_pdf_bytes_strict`].
pub async fn convert_pdf_bytes(
    bytes: Vec<u8>,
    req: &ScrapeRequest,
    source: PdfSource,
) -> CrwResult<ScrapeData> {
    let want_plaintext = req.formats.contains(&OutputFormat::PlainText);
    let max_pages = pdf_max_pages(req);
    let started = std::time::Instant::now();

    // Funnel through the global parse gate (concurrency cap + timeout + page cap).
    let result = run_parse(bytes, want_plaintext, max_pages).await;

    let convert_ms = started.elapsed().as_millis() as u64;
    record_metrics(&result, convert_ms);

    match result {
        Ok(extract) => Ok(build_scrape_data(
            extract,
            req,
            &source,
            source.elapsed_ms + convert_ms,
        )),
        Err(err) => {
            // Soft-fail: empty markdown + a warning. Upload callers map this to
            // an HTTP error themselves (they call the strict variant).
            tracing::warn!(url = %source.source_url, "pdf conversion failed: {err}");
            let mut data = empty_pdf_scrape_data(req, &source, source.elapsed_ms + convert_ms);
            data.warnings.push(err.to_string());
            data.warning = Some(err.to_string());
            Ok(data)
        }
    }
}

/// Emit prometheus metrics for one conversion attempt.
fn record_metrics(result: &Result<PdfExtract, PdfError>, convert_ms: u64) {
    let m = crw_core::metrics::metrics();
    m.document_conversion_duration_seconds
        .with_label_values(&["pdf"])
        .observe(convert_ms as f64 / 1000.0);
    match result {
        Ok(extract) => {
            let outcome = if extract.markdown.trim().is_empty() {
                "empty"
            } else {
                "ok"
            };
            m.document_conversions_total
                .with_label_values(&[outcome])
                .inc();
            m.document_pages_total
                .with_label_values(&["pdf"])
                .inc_by(extract.page_count as u64);
            let class = if extract.is_scanned {
                "scanned"
            } else {
                "text"
            };
            m.document_classification_total
                .with_label_values(&[class])
                .inc();
        }
        Err(err) => {
            m.document_conversions_total
                .with_label_values(&["error"])
                .inc();
            let class = match err {
                PdfError::Encrypted => "encrypted",
                PdfError::Timeout => "timeout",
                PdfError::TooLarge => "too_large",
                _ => "corrupt",
            };
            m.document_classification_total
                .with_label_values(&[class])
                .inc();
        }
    }
}

/// Like [`convert_pdf_bytes`] but returns `Err` on a parse failure. Used by the
/// upload endpoint, which should reject a bad/encrypted file loudly.
pub async fn convert_pdf_bytes_strict(
    bytes: Vec<u8>,
    req: &ScrapeRequest,
    source: PdfSource,
) -> Result<ScrapeData, (CrwError, PdfError)> {
    let want_plaintext = req.formats.contains(&OutputFormat::PlainText);
    let max_pages = pdf_max_pages(req);
    let started = std::time::Instant::now();

    let result = run_parse(bytes, want_plaintext, max_pages).await;

    let convert_ms = started.elapsed().as_millis() as u64;
    record_metrics(&result, convert_ms);
    match result {
        Ok(extract) => Ok(build_scrape_data(extract, req, &source, convert_ms)),
        Err(err) => Err((pdf_error_to_crw(&err), err)),
    }
}

/// Assemble a [`ScrapeData`] from a successful [`PdfExtract`], honoring the
/// requested formats and applying chunking when requested.
fn build_scrape_data(
    extract: PdfExtract,
    req: &ScrapeRequest,
    source: &PdfSource,
    elapsed_ms: u64,
) -> ScrapeData {
    let formats = &req.formats;
    // Markdown is the input to Json/Summary too, so produce it whenever any of
    // those is requested (matches the HTML path's behavior).
    let want_markdown = formats.contains(&OutputFormat::Markdown)
        || formats.contains(&OutputFormat::Json)
        || formats.contains(&OutputFormat::Summary);

    let mut warnings = extract.warnings;

    // Firecrawl-compat: `mode: "ocr"` forces OCR on every page. fastCRW has no
    // OCR engine, so we honor the request as far as we can (text extraction)
    // and surface a clear warning rather than rejecting it (drop-in safety).
    if pdf_mode(req).as_deref() == Some("ocr") {
        warnings.push(
            "pdf_ocr_unsupported: parsers mode 'ocr' requested but OCR is not available; \
             returned text-layer extraction only"
                .to_string(),
        );
    }

    let chunks = if want_markdown {
        compute_chunks(&extract.markdown, req)
    } else {
        None
    };

    // PDFs have no HTML DOM, so `links` can't be extracted today. Surface an
    // empty list (field present) when requested so clients don't break, plus a
    // one-time warning explaining the gap.
    let links = if formats.contains(&OutputFormat::Links) {
        warnings
            .push("pdf_links_unavailable: link extraction is not supported for PDF sources".into());
        Some(Vec::new())
    } else {
        None
    };

    ScrapeData {
        markdown: if want_markdown {
            Some(extract.markdown)
        } else {
            None
        },
        // Filled at the scrape choke point (single::scrape_url) for PDF-URL
        // scrapes; stays None for the direct /v2/parse upload path.
        source_hash: None,
        html: None,
        raw_html: None,
        plain_text: if formats.contains(&OutputFormat::PlainText) {
            Some(extract.plain_text)
        } else {
            None
        },
        links,
        // PDFs carry no HTML <img>/meta image sources.
        images: None,
        json: None,
        summary: None,
        llm_usage: None,
        chunks,
        warning: None,
        warnings,
        render_decision: None,
        // Per-page billing: 1 credit per page, floor 1. The SaaS settles
        // against this; opencore just reports it.
        credit_cost: extract.page_count.max(1) as u32,
        // PDF path does not run the basis-mode extraction; stamped only by the
        // structured-extraction choke (single.rs) when the request asks for it.
        basis: None,
        basis_warnings: Vec::new(),
        llm_input_hash: None,
        metadata: PageMetadata {
            title: extract.title,
            description: None,
            og_title: None,
            og_description: None,
            og_image: None,
            canonical_url: None,
            source_url: source.source_url.clone(),
            // Same as the HTML path: `scrape_url` stamps this from the fetch.
            // An uploaded document (`/v2/parse`) has no transport at all.
            final_url: None,
            language: None,
            status_code: source.status_code,
            rendered_with: Some("pdf".to_string()),
            elapsed_ms,
            page_count: Some(extract.page_count),
            source_filename: source.source_filename.clone(),
            extra: Default::default(),
        },
        debug_extraction: None,
        content_type: Some("application/pdf".to_string()),
        change_tracking: None,
        // PDFs are never screenshotted (binary doc path).
        screenshot: None,
        // PDFs are the content, never an anti-bot shell.
        block: None,
        unclearable_wall: false,
        // The PDF parser reads whole documents; there is no partial-DOM snapshot.
        truncated: false,
        cached: false,
    }
}

/// A minimal `ScrapeData` for a failed/empty conversion (URL soft-fail path).
fn empty_pdf_scrape_data(req: &ScrapeRequest, source: &PdfSource, elapsed_ms: u64) -> ScrapeData {
    let extract = PdfExtract::default();
    build_scrape_data(extract, req, source, elapsed_ms)
}

/// Run the LLM-backed formats (`json` structured extraction, `summary`) over a
/// document's markdown. Used by the upload endpoint (`/v2/parse`), which does
/// not go through `scrape_url`'s inline LLM stages. Mirrors the semantics of
/// those stages: server-side `llm_config` only (no per-request BYOK here),
/// strips internally-computed markdown when only `summary` was requested.
pub async fn apply_llm_formats(
    data: &mut ScrapeData,
    req: &ScrapeRequest,
    llm_config: Option<&LlmConfig>,
) -> CrwResult<()> {
    let effective_schema = req
        .json_schema
        .as_ref()
        .or_else(|| req.extract.as_ref().and_then(|e| e.schema.as_ref()));

    if req.formats.contains(&OutputFormat::Json) {
        match (effective_schema, llm_config) {
            (Some(schema), Some(llm)) => {
                let md = data.markdown.as_deref().unwrap_or("");
                let result = crw_extract::structured::extract_structured_with_usage(
                    md,
                    Some(schema),
                    None,
                    llm,
                    None,
                )
                .await?;
                data.json = Some(result.value);
                LlmUsage::accumulate(&mut data.llm_usage, result.usage);
            }
            (Some(_), None) => {
                return Err(CrwError::ExtractionError(
                    "JSON extraction requested but no LLM configured. Set [extraction.llm] in \
                     server config."
                        .into(),
                ));
            }
            (None, _) => {
                return Err(CrwError::InvalidRequest(
                    "Structured extraction (formats: json) requires a 'jsonSchema' field.".into(),
                ));
            }
        }
    }

    if req.formats.contains(&OutputFormat::Summary) {
        let Some(llm) = llm_config else {
            return Err(CrwError::ExtractionError(
                "Summary format requires an LLM config. Set [extraction.llm] in server config."
                    .into(),
            ));
        };
        let md_owned = data.markdown.clone().unwrap_or_default();
        match crw_extract::summary::summarize(
            &md_owned,
            llm,
            req.summary_prompt.as_deref(),
            req.max_content_chars,
        )
        .await
        {
            Ok(result) => {
                data.summary = Some(result.content);
                // The json leg above may already have filled this slot, and a
                // `formats: ["json","summary"]` upload runs both: overwriting
                // only when empty discarded whichever ran second.
                LlmUsage::accumulate(&mut data.llm_usage, result.usage);
                if let Some(w) = result.warning {
                    data.warnings.push(w);
                }
            }
            Err(e) => {
                data.warnings.push(format!("summary failed: {e}"));
            }
        }
        if !req.formats.contains(&OutputFormat::Markdown) {
            data.markdown = None;
        }
    }

    Ok(())
}

/// Chunk + filter PDF markdown, mirroring the HTML path's chunking stage.
fn compute_chunks(markdown: &str, req: &ScrapeRequest) -> Option<Vec<ChunkResult>> {
    let strategy = req.chunk_strategy.as_ref()?;
    if markdown.trim().is_empty() {
        return None;
    }
    let raw = crw_extract::chunking::chunk_text(markdown, strategy);
    if raw.is_empty() {
        return None;
    }
    let results: Vec<ChunkResult> = match (req.query.as_deref(), req.filter_mode.as_ref()) {
        (Some(q), Some(mode)) if !q.trim().is_empty() => {
            crw_extract::filter::filter_chunks_scored(&raw, q, mode, req.top_k.unwrap_or(5))
                .into_iter()
                .map(|sc| ChunkResult {
                    content: sc.content,
                    score: Some(sc.score),
                    index: sc.index,
                })
                .collect()
        }
        _ => {
            let mut r: Vec<ChunkResult> = raw
                .into_iter()
                .enumerate()
                .map(|(i, c)| ChunkResult {
                    content: c,
                    score: None,
                    index: i,
                })
                .collect();
            if let Some(k) = req.top_k {
                r.truncate(k);
            }
            r
        }
    };
    if results.is_empty() {
        None
    } else {
        Some(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crw_core::types::{ChunkStrategy, FilterMode};

    // ── effective_max_pages ──────────────────────────────────────────────

    #[test]
    fn page_cap_combines_request_and_config() {
        // No request cap, no server cap → unlimited.
        assert_eq!(effective_max_pages(None, 0), None);
        // Server cap only.
        assert_eq!(effective_max_pages(None, 50), Some(50));
        // Request cap only.
        assert_eq!(effective_max_pages(Some(10), 0), Some(10));
        // Both → the smaller wins (server protects regardless of request).
        assert_eq!(effective_max_pages(Some(10), 50), Some(10));
        assert_eq!(effective_max_pages(Some(100), 50), Some(50));
    }

    #[test]
    fn effective_max_pages_request_zero_does_not_fall_back_to_server_cap() {
        // BUG: the doc comment says "0 means unlimited on either side", but a
        // request `maxPages: 0` combined with a server cap returns `Some(0)`,
        // not the server cap. Downstream `crw_extract::pdf::convert` treats
        // `max_pages` via `Some(n) if n > 0 => capped, _ => unlimited`, so
        // `Some(0)` is parsed as UNLIMITED — meaning a client sending
        // `parsers:[{type:"pdf", maxPages:0}]` silently bypasses the server's
        // page cap entirely, contradicting "the smaller wins (server protects
        // regardless of request)" documented a few lines above. Asserting the
        // current behaviour, not fixing it.
        assert_eq!(effective_max_pages(Some(0), 50), Some(0));
        assert_eq!(effective_max_pages(Some(0), 0), Some(0));
    }

    // ── pdf_parse_requested ──────────────────────────────────────────────

    #[test]
    fn pdf_parse_requested_true_when_parsers_omitted() {
        assert!(pdf_parse_requested(&ScrapeRequest::default()));
    }

    #[test]
    fn pdf_parse_requested_false_on_explicit_empty_list() {
        let req = ScrapeRequest {
            parsers: Some(vec![]),
            ..Default::default()
        };
        assert!(!pdf_parse_requested(&req));
    }

    #[test]
    fn pdf_parse_requested_true_when_pdf_entry_present() {
        let req = ScrapeRequest {
            parsers: Some(vec![ParserSpec::pdf()]),
            ..Default::default()
        };
        assert!(pdf_parse_requested(&req));
    }

    #[test]
    fn pdf_parse_requested_false_when_only_other_parsers() {
        let req = ScrapeRequest {
            parsers: Some(vec![ParserSpec {
                parser_type: "docx".into(),
                mode: None,
                max_pages: None,
            }]),
            ..Default::default()
        };
        assert!(!pdf_parse_requested(&req));
    }

    #[test]
    fn pdf_parse_requested_case_insensitive_type_match() {
        let req = ScrapeRequest {
            parsers: Some(vec![ParserSpec {
                parser_type: "PDF".into(),
                mode: None,
                max_pages: None,
            }]),
            ..Default::default()
        };
        assert!(pdf_parse_requested(&req));
    }

    #[test]
    fn pdf_parse_requested_true_when_pdf_is_not_the_only_entry() {
        let req = ScrapeRequest {
            parsers: Some(vec![
                ParserSpec {
                    parser_type: "docx".into(),
                    mode: None,
                    max_pages: None,
                },
                ParserSpec::pdf(),
            ]),
            ..Default::default()
        };
        assert!(pdf_parse_requested(&req));
    }

    // ── pdf_spec / pdf_max_pages / pdf_mode ─────────────────────────────

    #[test]
    fn pdf_max_pages_none_when_no_parsers() {
        assert_eq!(pdf_max_pages(&ScrapeRequest::default()), None);
    }

    #[test]
    fn pdf_max_pages_none_when_pdf_entry_has_no_cap() {
        let req = ScrapeRequest {
            parsers: Some(vec![ParserSpec::pdf()]),
            ..Default::default()
        };
        assert_eq!(pdf_max_pages(&req), None);
    }

    #[test]
    fn pdf_max_pages_reads_the_pdf_entrys_cap() {
        let req = ScrapeRequest {
            parsers: Some(vec![ParserSpec {
                parser_type: "pdf".into(),
                mode: None,
                max_pages: Some(7),
            }]),
            ..Default::default()
        };
        assert_eq!(pdf_max_pages(&req), Some(7));
    }

    #[test]
    fn pdf_max_pages_ignores_a_non_pdf_entrys_cap() {
        let req = ScrapeRequest {
            parsers: Some(vec![ParserSpec {
                parser_type: "docx".into(),
                mode: None,
                max_pages: Some(7),
            }]),
            ..Default::default()
        };
        assert_eq!(pdf_max_pages(&req), None);
    }

    #[test]
    fn pdf_mode_none_when_unset() {
        let req = ScrapeRequest {
            parsers: Some(vec![ParserSpec::pdf()]),
            ..Default::default()
        };
        assert_eq!(pdf_mode(&req), None);
    }

    #[test]
    fn pdf_mode_lowercases_the_requested_mode() {
        let req = ScrapeRequest {
            parsers: Some(vec![ParserSpec {
                parser_type: "pdf".into(),
                mode: Some("OCR".into()),
                max_pages: None,
            }]),
            ..Default::default()
        };
        assert_eq!(pdf_mode(&req).as_deref(), Some("ocr"));
    }

    #[test]
    fn pdf_spec_finds_pdf_case_insensitively_among_multiple_entries() {
        let req = ScrapeRequest {
            parsers: Some(vec![
                ParserSpec {
                    parser_type: "docx".into(),
                    mode: None,
                    max_pages: None,
                },
                ParserSpec {
                    parser_type: "PDF".into(),
                    mode: Some("fast".into()),
                    max_pages: Some(2),
                },
            ]),
            ..Default::default()
        };
        let spec = pdf_spec(&req).expect("pdf entry present");
        assert_eq!(spec.max_pages, Some(2));
        assert_eq!(spec.mode.as_deref(), Some("fast"));
    }

    // ── pdf_error_to_crw ─────────────────────────────────────────────────

    #[test]
    fn pdf_error_to_crw_not_a_pdf_is_a_client_error() {
        assert!(matches!(
            pdf_error_to_crw(&PdfError::NotAPdf),
            CrwError::InvalidRequest(_)
        ));
    }

    #[test]
    fn pdf_error_to_crw_other_variants_are_extraction_errors() {
        for err in [
            PdfError::Encrypted,
            PdfError::Corrupt("x".into()),
            PdfError::Disabled,
            PdfError::Timeout,
            PdfError::TooLarge,
        ] {
            assert!(
                matches!(pdf_error_to_crw(&err), CrwError::ExtractionError(_)),
                "{err:?} should map to ExtractionError"
            );
        }
    }

    // ── build_scrape_data ────────────────────────────────────────────────

    fn test_source() -> PdfSource {
        PdfSource {
            source_url: "https://example.com/doc.pdf".to_string(),
            status_code: 200,
            elapsed_ms: 5,
            source_filename: None,
        }
    }

    fn sample_extract() -> PdfExtract {
        PdfExtract {
            markdown: "# Title\n\nBody text.".to_string(),
            plain_text: "Title\n\nBody text.".to_string(),
            page_count: 3,
            title: Some("Sample".to_string()),
            is_scanned: false,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn build_scrape_data_markdown_only_leaves_other_formats_none() {
        let req = ScrapeRequest {
            formats: vec![OutputFormat::Markdown],
            ..Default::default()
        };
        let data = build_scrape_data(sample_extract(), &req, &test_source(), 42);
        assert_eq!(data.markdown.as_deref(), Some("# Title\n\nBody text."));
        assert!(data.plain_text.is_none());
        assert!(data.links.is_none());
        assert_eq!(data.metadata.elapsed_ms, 42);
    }

    #[test]
    fn build_scrape_data_json_format_still_produces_markdown() {
        // markdown is the input to `json`/`summary`, so it must be populated
        // even when the caller only asked for `json`.
        let req = ScrapeRequest {
            formats: vec![OutputFormat::Json],
            ..Default::default()
        };
        let data = build_scrape_data(sample_extract(), &req, &test_source(), 0);
        assert!(data.markdown.is_some());
    }

    #[test]
    fn build_scrape_data_links_format_yields_empty_list_with_warning() {
        let req = ScrapeRequest {
            formats: vec![OutputFormat::Markdown, OutputFormat::Links],
            ..Default::default()
        };
        let data = build_scrape_data(sample_extract(), &req, &test_source(), 0);
        assert_eq!(data.links, Some(Vec::new()));
        assert!(
            data.warnings
                .iter()
                .any(|w| w.contains("pdf_links_unavailable"))
        );
    }

    #[test]
    fn build_scrape_data_ocr_mode_warns_but_still_returns_text_layer() {
        let req = ScrapeRequest {
            formats: vec![OutputFormat::Markdown],
            parsers: Some(vec![ParserSpec {
                parser_type: "pdf".into(),
                mode: Some("OCR".into()),
                max_pages: None,
            }]),
            ..Default::default()
        };
        let data = build_scrape_data(sample_extract(), &req, &test_source(), 0);
        assert!(
            data.warnings
                .iter()
                .any(|w| w.contains("pdf_ocr_unsupported"))
        );
        assert!(
            data.markdown.is_some(),
            "OCR unsupported still returns the text layer"
        );
    }

    #[test]
    fn build_scrape_data_credit_cost_floors_at_one_page() {
        let mut extract = sample_extract();
        extract.page_count = 0;
        let req = ScrapeRequest::default();
        let data = build_scrape_data(extract, &req, &test_source(), 0);
        assert_eq!(data.credit_cost, 1);
    }

    #[test]
    fn build_scrape_data_credit_cost_equals_page_count_above_one() {
        let extract = sample_extract(); // page_count: 3
        let req = ScrapeRequest::default();
        let data = build_scrape_data(extract, &req, &test_source(), 0);
        assert_eq!(data.credit_cost, 3);
    }

    #[test]
    fn build_scrape_data_carries_source_and_title_metadata() {
        let req = ScrapeRequest::default();
        let source = PdfSource {
            source_url: "https://example.com/report.pdf".to_string(),
            status_code: 206,
            elapsed_ms: 11,
            source_filename: Some("report.pdf".to_string()),
        };
        let data = build_scrape_data(sample_extract(), &req, &source, 7);
        assert_eq!(data.metadata.source_url, "https://example.com/report.pdf");
        assert_eq!(data.metadata.status_code, 206);
        assert_eq!(data.metadata.source_filename.as_deref(), Some("report.pdf"));
        assert_eq!(data.metadata.title.as_deref(), Some("Sample"));
        assert_eq!(data.metadata.rendered_with.as_deref(), Some("pdf"));
        assert_eq!(data.metadata.elapsed_ms, 7);
        assert!(data.html.is_none());
        assert!(data.images.is_none());
        assert!(data.screenshot.is_none());
        assert!(!data.truncated);
    }

    #[test]
    fn build_scrape_data_chunks_stay_none_without_markdown_format() {
        // Format asks only for `links` — no markdown/json/summary — so chunks
        // must stay None even though a chunk_strategy is set.
        let req = ScrapeRequest {
            formats: vec![OutputFormat::Links],
            chunk_strategy: Some(ChunkStrategy::Sentence {
                max_chars: None,
                overlap_chars: None,
                dedupe: None,
            }),
            ..Default::default()
        };
        let data = build_scrape_data(sample_extract(), &req, &test_source(), 0);
        assert!(data.chunks.is_none());
    }

    #[test]
    fn build_scrape_data_chunks_populated_when_markdown_wanted() {
        let req = ScrapeRequest {
            formats: vec![OutputFormat::Markdown],
            chunk_strategy: Some(ChunkStrategy::Sentence {
                max_chars: None,
                overlap_chars: None,
                dedupe: None,
            }),
            ..Default::default()
        };
        let data = build_scrape_data(sample_extract(), &req, &test_source(), 0);
        assert!(data.chunks.is_some());
    }

    // ── empty_pdf_scrape_data ────────────────────────────────────────────

    #[test]
    fn empty_pdf_scrape_data_has_floor_credit_and_zero_pages() {
        let req = ScrapeRequest {
            formats: vec![OutputFormat::Markdown],
            ..Default::default()
        };
        let data = empty_pdf_scrape_data(&req, &test_source(), 3);
        assert_eq!(data.credit_cost, 1);
        assert_eq!(data.metadata.page_count, Some(0));
        assert_eq!(data.markdown.as_deref(), Some(""));
        assert_eq!(data.metadata.elapsed_ms, 3);
    }

    // ── compute_chunks ───────────────────────────────────────────────────

    #[test]
    fn compute_chunks_none_without_a_strategy() {
        let req = ScrapeRequest::default();
        assert!(compute_chunks("some text.", &req).is_none());
    }

    #[test]
    fn compute_chunks_none_on_empty_markdown() {
        let req = ScrapeRequest {
            chunk_strategy: Some(ChunkStrategy::Sentence {
                max_chars: None,
                overlap_chars: None,
                dedupe: None,
            }),
            ..Default::default()
        };
        assert!(compute_chunks("   \n  ", &req).is_none());
    }

    #[test]
    fn compute_chunks_splits_and_indexes_sequentially() {
        let req = ScrapeRequest {
            chunk_strategy: Some(ChunkStrategy::Sentence {
                max_chars: Some(20),
                overlap_chars: None,
                dedupe: Some(false),
            }),
            ..Default::default()
        };
        let md = "First sentence here. Second sentence here. Third sentence here.";
        let chunks = compute_chunks(md, &req).expect("non-empty markdown with a strategy chunks");
        assert!(chunks.len() >= 2, "got {chunks:?}");
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.index, i);
            assert!(c.score.is_none(), "no query → unscored chunks");
        }
    }

    #[test]
    fn compute_chunks_top_k_truncates_without_a_query() {
        let req = ScrapeRequest {
            chunk_strategy: Some(ChunkStrategy::Sentence {
                max_chars: Some(10),
                overlap_chars: None,
                dedupe: Some(false),
            }),
            top_k: Some(1),
            ..Default::default()
        };
        let md = "One. Two. Three. Four. Five.";
        let chunks = compute_chunks(md, &req).expect("chunks");
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn compute_chunks_query_and_filter_mode_scores_every_result() {
        let req = ScrapeRequest {
            chunk_strategy: Some(ChunkStrategy::Sentence {
                max_chars: Some(50),
                overlap_chars: None,
                dedupe: Some(false),
            }),
            query: Some("apples".to_string()),
            filter_mode: Some(FilterMode::Bm25),
            top_k: Some(5),
            ..Default::default()
        };
        let md = "Apples are red. Oranges are orange. Bananas are yellow.";
        let chunks = compute_chunks(md, &req).expect("chunks");
        assert!(
            chunks.iter().all(|c| c.score.is_some()),
            "a query + filter_mode scores every chunk"
        );
    }

    // ── convert_pdf_bytes / convert_pdf_bytes_strict ────────────────────
    //
    // SAMPLE_PDF is the same 2-page, text-based fixture already used by
    // `tests/pdf_tests.rs` (the public-surface integration suite for this
    // module); these tests instead target the private helpers around it.

    const SAMPLE_PDF: &[u8] = include_bytes!("../tests/fixtures/sample.pdf");

    /// Minimal well-formed PDF assembler: `objs[i]` becomes object `i + 1`.
    /// Mirrors the object/xref/trailer shape crw-extract's own bomb-guard test
    /// uses, so the synthetic documents built here are known-parseable.
    fn build_pdf(objs: &[Vec<u8>], root: usize, encrypt: Option<usize>) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n");
        let mut offs = Vec::new();
        for (i, body) in objs.iter().enumerate() {
            offs.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes(),
        );
        for o in &offs {
            pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        let mut trailer = format!("trailer\n<< /Size {} /Root {} 0 R", objs.len() + 1, root);
        if let Some(enc) = encrypt {
            trailer.push_str(&format!(" /Encrypt {enc} 0 R"));
        }
        trailer.push_str(&format!(" >>\nstartxref\n{xref}\n%%EOF\n"));
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// A structurally valid PDF whose page tree declares zero pages.
    fn zero_page_pdf() -> Vec<u8> {
        let objs = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec(),
        ];
        build_pdf(&objs, 1, None)
    }

    /// A PDF whose trailer declares a non-"Standard" security handler.
    /// lopdf refuses to decode this regardless of password
    /// (`Error::UnsupportedSecurityHandler`, mapped to `PdfError::Encrypted`
    /// by pdf-inspector) — the same observable failure as a real
    /// password-protected PDF, without needing to implement RC4/AES key
    /// derivation just to build a fixture.
    fn bogus_encrypted_pdf() -> Vec<u8> {
        let content = b"BT /F1 12 Tf 72 700 Td (secret) Tj ET";
        let mut stream_obj = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        stream_obj.extend_from_slice(content);
        stream_obj.extend_from_slice(b"\nendstream");
        let objs = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
            stream_obj,
            b"<< /Filter /FastCrwBogusHandler /V 1 /R 2 /O (owner) /U (user) /P -4 >>".to_vec(),
        ];
        build_pdf(&objs, 1, Some(5))
    }

    /// A PDF whose single content stream is a FlateDecode bomb: `cap + 2 MiB`
    /// of zeros compress down to a few KB and would decompress back to the
    /// full size. Exercises the same `check_decompression_bomb` guard
    /// `crw-extract` unit-tests directly, but through this crate's
    /// `run_parse` choke point and its process-wide default cap.
    fn decompression_bomb_pdf(cap: usize) -> Vec<u8> {
        use std::io::Write;
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        let chunk = vec![0u8; 1024 * 1024];
        let mb_needed = cap / (1024 * 1024) + 2;
        for _ in 0..mb_needed {
            enc.write_all(&chunk).unwrap();
        }
        let comp = enc.finish().unwrap();
        let mut stream_obj = format!(
            "<< /Length {} /Filter /FlateDecode >>\nstream\n",
            comp.len()
        )
        .into_bytes();
        stream_obj.extend_from_slice(&comp);
        stream_obj.extend_from_slice(b"\nendstream");
        let objs = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
            stream_obj,
        ];
        build_pdf(&objs, 1, None)
    }

    #[tokio::test]
    async fn convert_pdf_bytes_extracts_real_text_and_page_count() {
        let req = ScrapeRequest {
            formats: vec![OutputFormat::Markdown],
            ..Default::default()
        };
        let data = convert_pdf_bytes(SAMPLE_PDF.to_vec(), &req, test_source())
            .await
            .unwrap();
        assert!(data.markdown.unwrap().contains("Hello fastCRW PDF parsing"));
        assert_eq!(data.metadata.page_count, Some(2));
        assert_eq!(data.credit_cost, 2);
        assert!(data.warning.is_none());
    }

    #[tokio::test]
    async fn convert_pdf_bytes_corrupt_bytes_soft_fail_with_warning() {
        let req = ScrapeRequest::default();
        let data = convert_pdf_bytes(b"%PDF-1.4 not a real pdf".to_vec(), &req, test_source())
            .await
            .expect("URL path never hard-fails");
        assert!(data.markdown.unwrap_or_default().is_empty());
        assert!(data.warning.is_some());
        assert_eq!(data.credit_cost, 1, "page_count 0 floors to 1 credit");
    }

    #[tokio::test]
    async fn convert_pdf_bytes_zero_page_pdf_does_not_panic() {
        let req = ScrapeRequest::default();
        let data = convert_pdf_bytes(zero_page_pdf(), &req, test_source())
            .await
            .expect("must not panic or hard-fail on a structurally valid zero-page PDF");
        assert_eq!(data.metadata.page_count, Some(0));
        assert_eq!(data.credit_cost, 1, "floor(0) == 1 credit, never 0");
    }

    #[tokio::test]
    async fn convert_pdf_bytes_strict_zero_page_pdf_ok() {
        let req = ScrapeRequest::default();
        let data = convert_pdf_bytes_strict(zero_page_pdf(), &req, test_source())
            .await
            .expect("zero pages is a valid document, not an error");
        assert_eq!(data.metadata.page_count, Some(0));
    }

    #[tokio::test]
    async fn convert_pdf_bytes_encrypted_pdf_soft_fails_with_encrypted_warning() {
        let req = ScrapeRequest::default();
        let data = convert_pdf_bytes(bogus_encrypted_pdf(), &req, test_source())
            .await
            .unwrap();
        assert!(
            data.warnings.iter().any(|w| w.contains("pdf_encrypted")),
            "expected an encrypted warning, got {:?}",
            data.warnings
        );
    }

    #[tokio::test]
    async fn convert_pdf_bytes_strict_encrypted_pdf_errors() {
        let req = ScrapeRequest::default();
        let (crw_err, pdf_err) =
            convert_pdf_bytes_strict(bogus_encrypted_pdf(), &req, test_source())
                .await
                .expect_err("upload path must hard-fail on an encrypted PDF");
        assert_eq!(pdf_err, PdfError::Encrypted);
        assert!(matches!(crw_err, CrwError::ExtractionError(_)));
    }

    #[tokio::test]
    async fn convert_pdf_bytes_strict_corrupt_bytes_errors() {
        let req = ScrapeRequest::default();
        let (crw_err, pdf_err) =
            convert_pdf_bytes_strict(b"garbage not a pdf".to_vec(), &req, test_source())
                .await
                .expect_err("upload path must hard-fail on unparseable bytes");
        assert!(matches!(pdf_err, PdfError::NotAPdf | PdfError::Corrupt(_)));
        match pdf_err {
            PdfError::NotAPdf => assert!(matches!(crw_err, CrwError::InvalidRequest(_))),
            _ => assert!(matches!(crw_err, CrwError::ExtractionError(_))),
        }
    }

    #[tokio::test]
    async fn convert_pdf_bytes_respects_per_request_max_pages() {
        let req = ScrapeRequest {
            formats: vec![OutputFormat::Markdown],
            parsers: Some(vec![ParserSpec {
                parser_type: "pdf".into(),
                mode: None,
                max_pages: Some(1),
            }]),
            ..Default::default()
        };
        let data = convert_pdf_bytes(SAMPLE_PDF.to_vec(), &req, test_source())
            .await
            .unwrap();
        let md = data.markdown.unwrap_or_default();
        assert!(md.contains("Hello fastCRW PDF parsing"));
        assert!(
            !md.contains("Second page content"),
            "max_pages=1 caps output"
        );
        // page_count is the source document's page count, not the number of
        // pages returned: max_pages caps the OUTPUT, the parser still processes
        // the whole document.
        assert_eq!(data.metadata.page_count, Some(2));
        // Billed per page PROCESSED, not per page returned. max_pages trims the
        // output, it does not reduce the parsing work, so both pages are charged.
        assert_eq!(data.credit_cost, 2);
    }

    #[tokio::test]
    async fn convert_pdf_bytes_decompression_bomb_rejected_not_oom() {
        let req = ScrapeRequest::default();
        let data = convert_pdf_bytes(decompression_bomb_pdf(104_857_600), &req, test_source())
            .await
            .expect("bomb guard soft-fails through the URL path, never panics/OOMs");
        assert!(
            data.warnings.iter().any(|w| w.contains("pdf_too_large")),
            "expected a too_large warning, got {:?}",
            data.warnings
        );
    }

    // ── apply_llm_formats ────────────────────────────────────────────────

    #[tokio::test]
    async fn apply_llm_formats_json_without_schema_is_invalid_request() {
        let req = ScrapeRequest {
            formats: vec![OutputFormat::Json],
            ..Default::default()
        };
        let mut data = empty_pdf_scrape_data(&req, &test_source(), 0);
        let err = apply_llm_formats(&mut data, &req, None).await.unwrap_err();
        assert!(matches!(err, CrwError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn apply_llm_formats_json_with_schema_but_no_llm_config_errors() {
        let req = ScrapeRequest {
            formats: vec![OutputFormat::Json],
            json_schema: Some(serde_json::json!({"type": "object"})),
            ..Default::default()
        };
        let mut data = empty_pdf_scrape_data(&req, &test_source(), 0);
        let err = apply_llm_formats(&mut data, &req, None).await.unwrap_err();
        assert!(matches!(err, CrwError::ExtractionError(_)));
    }

    #[tokio::test]
    async fn apply_llm_formats_summary_without_llm_config_errors() {
        let req = ScrapeRequest {
            formats: vec![OutputFormat::Summary],
            ..Default::default()
        };
        let mut data = empty_pdf_scrape_data(&req, &test_source(), 0);
        let err = apply_llm_formats(&mut data, &req, None).await.unwrap_err();
        assert!(matches!(err, CrwError::ExtractionError(_)));
    }

    #[tokio::test]
    async fn apply_llm_formats_is_a_noop_without_json_or_summary_formats() {
        let req = ScrapeRequest {
            formats: vec![OutputFormat::Markdown],
            ..Default::default()
        };
        let mut data = empty_pdf_scrape_data(&req, &test_source(), 0);
        data.markdown = Some("unchanged".to_string());
        apply_llm_formats(&mut data, &req, None).await.unwrap();
        assert_eq!(data.markdown.as_deref(), Some("unchanged"));
        assert!(data.json.is_none());
        assert!(data.summary.is_none());
    }
}
