//! CLI subcommand implementations.
//!
//! Each subcommand is a separate module with a `run()` async function.

pub mod bench;
pub mod browse;
pub mod crawl;
mod diag;
pub mod doctor;
pub mod map;
pub mod mcp;
pub mod scrape;
pub mod search;
pub mod serve;
pub mod setup;
pub mod smoke;
