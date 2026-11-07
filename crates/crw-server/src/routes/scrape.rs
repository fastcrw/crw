use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use crw_core::Deadline;
use crw_core::error::CrwError;
use crw_core::types::{ApiResponse, OutputFormat, ScrapeData, ScrapeRequest};
use crw_crawl::single::scrape_url;

use crate::error::AppError;
use crate::state::{AppState, validate_renderer_pin};

pub async fn scrape(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: Result<Json<ScrapeRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<ScrapeData>>, AppError> {
    let Json(mut req) = body.map_err(AppError::from)?;
    // `force_cloak` is `skip_deserializing`, so it is never read from the body.
    // The only source is this trusted, SaaS-set header (the engine is not
    // internet-exposed in the managed deployment), so a caller cannot force the
    // expensive cloak tier.
    req.force_cloak = Some(
        headers
            .get("x-crw-force-cloak")
            .and_then(|v| v.to_str().ok())
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false),
    );
    // Same trusted channel: `cloak_allowed` is `skip_deserializing`, and only
    // matters when the deployment runs with `renderer.cloak_opt_in_only`.
    req.cloak_allowed = Some(
        headers
            .get("x-crw-cloak-allow")
            .and_then(|v| v.to_str().ok())
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false),
    );
    // Same trusted channel: the SaaS derives this byte ceiling from the
    // credits the customer allowed for the page. A malformed value closes the
    // cloak tier (0) rather than falling back to the sidecar's larger default.
    req.cloak_max_bytes = headers.get("x-crw-cloak-max-bytes").map(|v| {
        v.to_str()
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(0)
    });
    // Same trusted channel as `x-crw-force-cloak`: `cache_scope` is
    // `skip_deserializing`, so this header is its only source and a caller
    // cannot put themselves in someone else's cache scope.
    req.cache_scope = headers
        .get("x-crw-cache-scope")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let parsed_url = url::Url::parse(&req.url)
        .map_err(|e| CrwError::InvalidRequest(format!("Invalid URL: {e}")))?;
    crw_core::url_safety::validate_safe_url_resolved(&parsed_url)
        .await
        .map_err(CrwError::InvalidRequest)?;
    validate_renderer_pin(req.renderer, req.render_js, &state)?;

    let llm_config = state.config.extraction.llm.as_ref();

    // Validate LLM-touching formats up front so we fail fast with a clear
    // message rather than running the full scrape and then erroring.
    if req.formats.contains(&OutputFormat::Summary)
        && llm_config.is_none()
        && req.llm_api_key.is_none()
    {
        return Err(AppError::from(CrwError::InvalidRequest(
            "summary format requires LLM config: set CRW_EXTRACTION__LLM__API_KEY \
             in server config or pass llm_api_key in the request body"
                .into(),
        )));
    }
    if let Some(cfg) = llm_config
        && let Some(header_name) = cfg.require_byok_header.as_deref()
        && (req.formats.contains(&OutputFormat::Summary)
            || req.formats.contains(&OutputFormat::Json))
        && req.llm_api_key.is_none()
    {
        // SaaS-fronted deploy: the request transformer must have set the
        // tenant header. Reject direct public callers.
        // NOTE: actually checking the header is done at the middleware
        // layer (see Phase 7a); this catches the missing-header case at
        // the request level when no BYOK key is supplied.
        let _ = header_name;
        return Err(AppError::from(CrwError::InvalidRequest(
            "LLM features require a per-request llm_api_key (BYOK header guard active)".into(),
        )));
    }

    let user_agent = &state.config.crawler.user_agent;
    let default_stealth =
        state.config.crawler.stealth.enabled && state.config.crawler.stealth.inject_headers;
    let deadline = Deadline::from_request_ms(
        state
            .config
            .effective_deadline_ms(req.deadline_ms, req.wait_for),
    );
    let data = scrape_url(
        &req,
        &state.renderer,
        llm_config,
        &state.config.extraction,
        user_agent,
        default_stealth,
        state.config.renderer.render_js_default,
        deadline,
    )
    .await?;

    // The origin answered with an error status and what came back is its error
    // page, so this is `success:false` however the caller framed the request.
    // `ScrapeData::http_error` owns both halves of that judgement (see its
    // measured text bar) and is shared with v2, crawl and batch — the same URL
    // must not be refunded on one surface and billed on another.
    //
    // The body stays in the envelope: the caller can still read the error page
    // and `metadata.statusCode`. Only a genuine wall gets `clear_body()` below.
    if let Some(error_msg) = data.http_error() {
        return Ok(Json(ApiResponse {
            success: false,
            data: Some(data),
            error: Some(error_msg),
            error_code: Some("http_error".into()),
            code: None,
            warning: None,
        }));
    }

    // Anti-bot block: the verdict is now classified ONCE at the scrape choke
    // (`single::classify_block`) and stamped onto `data.block`, so v1/v2/crawl
    // share one decision.
    if let Some(b) = data.block.clone() {
        let error_msg = b.message();
        // A structural failure is not a wall: it is "we fetched the page fine and
        // there was nothing usable in it". The customer-facing message has split
        // these since `BlockOutcome::message()`; the machine-readable code now
        // does too, so a client can branch without matching English. Refunds are
        // unaffected — the SaaS keys on `success:false`, not on this code.
        let error_code = if b.vendor == crw_core::types::STRUCTURAL_FAILURE_VENDOR
            || b.vendor == crw_core::types::PARKED_DOMAIN_VENDOR
        {
            "no_usable_content"
        } else {
            "anti_bot"
        };
        // Drop the interstitial/challenge shell so the caller gets a clean block
        // instead of the "Just a moment / Humans only" wall as markdown. Runs
        // after the http_error gate above, which reads the content lengths.
        let mut data = data;
        data.clear_body();
        return Ok(Json(ApiResponse {
            success: false,
            data: Some(data),
            error: Some(error_msg),
            error_code: Some(error_code.into()),
            code: None,
            warning: None,
        }));
    }

    // Nothing the caller asked for came back. Until now this shipped as a
    // success: a PDF the decompression-bomb guard refused, or an origin that
    // answered 200 with nothing extractable, returned `success:true` with an
    // empty body — and the SaaS refunds on `success:false`, so a `true` here is
    // a charge for a response with no content in it. Measured on prod: four
    // customer scrapes in six days, one of them carrying no warning at all,
    // which is why `has_no_content` keys on the outcome rather than on warning
    // text. `no_usable_content` is the code the structural/parked verdicts
    // already use for exactly this meaning.
    if data.has_no_content(&req.formats) {
        // Several paths record why in `warnings` (plural) without ever setting
        // the singular `warning` — a failed summary is one — so reading only the
        // singular would hand the caller the generic sentence while the real
        // reason sat one field over.
        let error_msg = data
            .warning
            .clone()
            .or_else(|| data.warnings.first().cloned())
            .unwrap_or_else(|| "No content could be extracted from the page".to_string());
        return Ok(Json(ApiResponse {
            success: false,
            data: Some(data),
            error: Some(error_msg),
            error_code: Some("no_usable_content".into()),
            code: None,
            warning: None,
        }));
    }

    // Promote data.warning to ApiResponse.warning so it's visible at top level
    let warning = data.warning.clone();
    let mut resp = ApiResponse::ok(data);
    resp.warning = warning;
    Ok(Json(resp))
}
