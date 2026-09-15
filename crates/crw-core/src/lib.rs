//! Core types, configuration, and error handling for the CRW web scraper.
//!
//! This crate provides the foundational building blocks shared across all CRW crates:
//!
//! - [`config`] — Layered TOML configuration with environment variable overrides
//! - [`error`] — Unified error types ([`CrwError`]) and result alias ([`CrwResult`])
//! - [`types`] — Shared data structures (`ScrapeData`, `FetchResult`, `OutputFormat`, etc.)
//! - [`url_safety`] — SSRF protection (blocks private IPs, cloud metadata, non-HTTP schemes)
//!
//! # Example
//!
//! ```rust
//! use crw_core::{AppConfig, CrwError, CrwResult};
//!
//! let config = AppConfig::load().unwrap();
//! assert!(config.server.port > 0);
//! ```

pub mod body;
pub mod config;
pub mod deadline;
pub mod error;
pub mod evidence;
pub mod mcp;
pub mod metrics;
pub mod proxy;
pub mod research_types;
pub mod reserved_sem;
pub mod scrape_class;
pub mod types;
pub mod url_safety;

pub use config::AppConfig;
pub use deadline::Deadline;
pub use error::{CrwError, CrwResult};
pub use proxy::{ProxyEntry, ProxyRotation, ProxyRotator, redact_proxy_url};
pub use reserved_sem::{BatchGate, LanePermit, ReservedSemaphore};
pub use scrape_class::{REQUEST_CLASS, ScrapeClass, current_scrape_class};
pub use types::is_html_like_content_type;
