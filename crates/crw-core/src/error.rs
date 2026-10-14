use thiserror::Error;

#[derive(Debug, Error)]
pub enum CrwError {
    #[error("HTTP request failed: {0}")]
    HttpError(String),

    #[error("Target unreachable: {0}")]
    TargetUnreachable(String),

    #[error("URL parse error: {0}")]
    UrlParseError(#[from] url::ParseError),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Renderer error: {0}")]
    RendererError(String),

    #[error("Extraction error: {0}")]
    ExtractionError(String),

    /// The origin returned a body that is neither HTML nor a document crw can
    /// parse (a ZIP-container office file, an image, an archive). Distinct from
    /// `HttpError` because it must NOT escalate to the JS renderer ladder: no
    /// browser turns a .docx into a page, so climbing the ladder only burns a
    /// Chromium/Camoufox session before failing anyway.
    #[error("Unsupported content type: {0}")]
    UnsupportedContentType(String),

    #[error("Crawl error: {0}")]
    CrawlError(String),

    #[error("Timeout after {0}ms")]
    Timeout(u64),

    #[error("Config error: {0}")]
    ConfigError(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Rate limited")]
    RateLimited,

    #[error("{0}")]
    SearchDisabled(String),

    #[error("{0}")]
    SearchDegraded(String),

    #[error("{0}")]
    Internal(String),

    #[error("Renderer pool shutting down")]
    Shutdown,
}

impl CrwError {
    /// Machine-readable error code for API consumers.
    pub fn error_code(&self) -> &'static str {
        match self {
            CrwError::HttpError(_) => "http_error",
            CrwError::TargetUnreachable(_) => "target_unreachable",
            CrwError::UrlParseError(_) => "invalid_url",
            CrwError::InvalidRequest(_) => "invalid_request",
            CrwError::RendererError(_) => "renderer_error",
            CrwError::ExtractionError(_) => "extraction_error",
            CrwError::UnsupportedContentType(_) => "unsupported_content_type",
            CrwError::CrawlError(_) => "crawl_error",
            CrwError::Timeout(_) => "timeout",
            CrwError::ConfigError(_) => "config_error",
            CrwError::NotFound(_) => "not_found",
            CrwError::RateLimited => "rate_limited",
            CrwError::SearchDisabled(_) => "search_disabled",
            CrwError::SearchDegraded(_) => "search_degraded",
            CrwError::Internal(_) => "internal_error",
            CrwError::Shutdown => "shutdown",
        }
    }
}

pub type CrwResult<T> = Result<T, CrwError>;

/// Display string for a `reqwest::Error` with the request URL stripped.
///
/// `reqwest::Error`'s `Display` appends `" for url (<url>)"`, and these strings
/// reach API callers verbatim in the scrape `error` field, a crawl document's
/// `block.reason`, and the `/v2` error envelopes. That URL is frequently
/// internal infrastructure (a CDP endpoint, a sidecar host, the managed LLM
/// provider) or carries credentials, none of which a caller may see. Log the
/// full error with `tracing` at the call site when the operator needs the URL.
pub fn reqwest_message(e: reqwest::Error) -> String {
    e.without_url().to_string()
}

/// A `reqwest::Error` and its whole `source()` chain, for an operator log line:
/// request URL stripped, URL userinfo masked.
///
/// `reqwest::Error`'s own `Display` is only the outer layer ("error sending
/// request"); why a proxy hop failed (`tunnel error: unsuccessful`, a 407 as
/// `proxy authorization required`, a refused connect) lives in the sources.
/// Proxy URLs carry `user:pass@`, so the joined text is masked before it can
/// reach a log line.
pub fn reqwest_error_chain(e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut out = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(cause) = source {
        let text = cause.to_string();
        // Some layers repeat their source in their own Display.
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = cause.source();
    }
    crate::proxy::redact_url_userinfo(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reqwest_message_drops_the_request_url() {
        // Loopback port 1 is privileged, so no test process can be listening
        // on it: the connect is refused at once, nothing leaves the machine,
        // no proxy is consulted, and the resulting error still carries the
        // request URL.
        let err = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get("http://127.0.0.1:1/internal-secret-path")
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("for url"),
            "precondition: reqwest still appends the URL, got {err}"
        );

        let msg = reqwest_message(err);
        assert!(!msg.contains("for url"), "URL not stripped: {msg}");
        assert!(!msg.contains("internal-secret-path"), "URL leaked: {msg}");
    }

    /// A real proxy failure: the client is pointed at a credentialed proxy on
    /// loopback port 1, so the CONNECT is refused locally and nothing leaves
    /// the machine. The chain must name the cause the bare `Display` hides, and
    /// carry neither the proxy credentials nor the target URL.
    #[tokio::test]
    async fn reqwest_error_chain_keeps_the_cause_and_drops_secrets() {
        let proxy = reqwest::Proxy::all("http://proxyuser:pr0xy-s3cret@127.0.0.1:1").unwrap();
        let err = reqwest::Client::builder()
            .proxy(proxy)
            .build()
            .unwrap()
            .get("http://target.invalid/customer-path?token=t0ken")
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .unwrap_err();

        let chain = reqwest_error_chain(err);
        println!("{chain}");
        assert!(
            chain.starts_with("error sending request: "),
            "outer layer missing or URL kept: {chain}"
        );
        assert!(
            chain.len() > "error sending request: ".len(),
            "no cause after the outer layer: {chain}"
        );
        for secret in ["pr0xy-s3cret", "proxyuser", "customer-path", "t0ken"] {
            assert!(!chain.contains(secret), "{secret} leaked: {chain}");
        }
    }
}
