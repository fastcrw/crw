//! CRW — Unified CLI for web scraping, crawling, and search.
//!
//! # Usage
//!
//! ```bash
//! # Default: scrape (backwards compatible)
//! crw example.com
//! crw example.com --format json
//!
//! # Explicit subcommands
//! crw scrape example.com
//! crw search "rust web scraper"
//! crw crawl example.com --depth 3
//! crw map example.com
//! crw serve --port 3000
//! crw mcp
//! crw browse
//! crw setup
//! ```

mod commands;
mod teardown;

use clap::{Parser, Subcommand};
use commands::scrape::Format;
use teardown::{CmdError, finish, install_signal_teardown};

#[derive(Parser)]
#[command(
    name = "crw",
    version,
    // Default-scrape-mode args (url, format, --reset, …) are mutually exclusive
    // with using a subcommand. Expressed at the container level because a
    // `#[command(subcommand)]` field is NOT a conflictable arg id — pointing
    // `conflicts_with = "command"` at it tripped clap's debug-build assertion.
    args_conflicts_with_subcommands = true,
    about = "Web scraper for AI agents",
    long_about = "Unified CLI for web scraping, crawling, search, and serving.\n\n\
        The fastest web scraper built for AI agents and LLM data pipelines.\n\n\
        Examples:\n  \
        crw example.com                                                 # Scrape URL (default mode)\n  \
        crw scrape example.com --format json\n  \
        crw search \"rust web scraper\" --json --fields title,url,snippet  # LLM-ready JSON\n  \
        crw crawl example.com --depth 3                                 # BFS crawl\n  \
        crw map example.com                                             # Discover URLs\n  \
        crw serve --port 3000                                           # Start REST API server\n  \
        crw mcp                                                         # Start MCP server\n  \
        crw browse                                                      # Start browser automation MCP\n  \
        crw setup                                                       # Interactive setup wizard",
    after_help = "INSTALL:\n  \
        brew install fastcrw/crw/crw                                         # macOS / Linux\n  \
        cargo install crw-cli                                           # Any Rust toolchain\n  \
        curl -fsSL https://fastcrw.com/install | sh\n\n\
        DOCS:    https://docs.fastcrw.com  ·  https://github.com/fastcrw/crw\n\
        CLOUD:   https://fastcrw.com (1000 free credits, no monthly reset)\n\
        SEARCH:  `crw setup` connects Cloud; `crw setup --local` adds a private local backend.\n\
        "
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    // --- Default scrape mode (backwards compat) ---
    /// URL to scrape (when no subcommand is given)
    #[arg(value_name = "URL")]
    url: Option<String>,

    /// Output format (for default scrape mode)
    #[arg(short, long, value_enum, default_value = "markdown")]
    format: Option<Format>,

    /// Write output to file instead of stdout
    #[arg(short, long, value_name = "FILE")]
    output: Option<String>,

    /// Disable main content extraction (return full page content)
    #[arg(long)]
    raw: bool,

    /// Enable JavaScript rendering
    #[arg(long)]
    js: bool,

    /// Extract only elements matching this CSS selector
    #[arg(long, value_name = "SELECTOR")]
    css: Option<String>,

    /// Extract only elements matching this XPath expression
    #[arg(long, value_name = "EXPR")]
    xpath: Option<String>,

    /// HTTP, HTTPS, or SOCKS5 proxy URL
    #[arg(long, value_name = "URL")]
    proxy: Option<String>,

    /// Enable stealth mode
    #[arg(long)]
    stealth: bool,

    /// Generate an AI summary of the page using the configured LLM
    #[arg(long, conflicts_with = "extract")]
    summary: bool,

    /// Style/format hint for --summary (e.g. "in 3 bullet points")
    #[arg(long, value_name = "TEXT", requires = "summary")]
    prompt: Option<String>,

    /// Extract structured data using a JSON Schema (inline JSON or @path/to/schema.json)
    #[arg(long, value_name = "SCHEMA")]
    extract: Option<String>,

    /// Override LLM provider (anthropic, openai, openai-responses, deepseek, azure, openrouter)
    #[arg(long, value_name = "NAME")]
    llm_provider: Option<String>,

    /// Override LLM API key for this request
    #[arg(long, value_name = "KEY")]
    llm_key: Option<String>,

    /// Override LLM model for this request
    #[arg(long, value_name = "MODEL")]
    llm_model: Option<String>,

    /// Override LLM base URL (for Chat Completions-compatible, Responses-compatible, or Azure endpoints)
    #[arg(long, value_name = "URL")]
    llm_base_url: Option<String>,

    /// Shortcut for `crw setup --reset` — wipe config.toml, sentinel, and shell blocks.
    #[arg(long, conflicts_with = "url")]
    reset: bool,

    /// Skip confirmation prompt for `--reset`.
    #[arg(long, requires = "reset")]
    yes: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Scrape a single URL and output content
    Scrape(commands::scrape::ScrapeArgs),

    /// Web search via CRW Cloud or a configured local backend
    Search(commands::search::SearchArgs),

    /// BFS crawl a website starting from a URL
    Crawl(commands::crawl::CrawlArgs),

    /// Discover URLs on a website via sitemap and crawling
    Map(commands::map::MapArgs),

    /// Start the REST API server (Firecrawl-compatible)
    Serve(commands::serve::ServeArgs),

    /// Run a search-quality benchmark (FRAMES) against a running server
    Bench(commands::bench::BenchArgs),

    /// Start the MCP (Model Context Protocol) server
    Mcp(commands::mcp::McpArgs),

    /// Start the browser automation MCP server
    Browse(commands::browse::BrowseArgs),

    /// Interactive setup wizard (Cloud or Local)
    Setup(commands::setup::SetupArgs),

    /// Read-only diagnostics: config, renderer, search, LLM, proxy, ports
    Doctor(commands::doctor::DoctorArgs),

    /// Deterministic, offline-by-default sanity checks across surfaces
    Smoke(commands::smoke::SmokeArgs),
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Top-level `--reset` shortcut → delegate to `setup --reset`.
    if cli.reset {
        let args = commands::setup::SetupArgs {
            non_interactive: false,
            cloud: false,
            local: false,
            api_key: None,
            no_agents: false,
            no_color: false,
            reset_shell: false,
            reset: true,
            yes: cli.yes,
        };
        commands::setup::run(args).await;
        return;
    }

    // Browser-using commands route through one consolidated exit
    // (`finish`) so `kill_all_browsers()` runs exactly once on every path.
    // The signal teardown task is installed *before* their `run()` (and
    // therefore before any browser spawn inside it). Non-browser commands
    // (search/serve/setup) keep their own lifecycle and own no browser.
    let result: Result<(), CmdError> = match cli.command {
        Some(Commands::Scrape(args)) => {
            install_signal_teardown();
            commands::scrape::run(args).await
        }
        Some(Commands::Search(args)) => {
            commands::search::run(args).await;
            Ok(())
        }
        Some(Commands::Crawl(args)) => {
            install_signal_teardown();
            commands::crawl::run(args).await
        }
        Some(Commands::Map(args)) => {
            install_signal_teardown();
            commands::map::run(args).await
        }
        Some(Commands::Serve(args)) => {
            commands::serve::run(args).await;
            Ok(())
        }
        Some(Commands::Bench(args)) => commands::bench::run(args).await,
        Some(Commands::Mcp(args)) => {
            install_signal_teardown();
            commands::mcp::run(args).await
        }
        Some(Commands::Browse(args)) => {
            if let Err(e) = commands::browse::run(args).await {
                eprintln!("error: {e}");
                // browse connects to an external ws_url and owns no
                // ManagedBrowser, so there is no registry teardown to run.
                std::process::exit(1); // teardown-exit-ok
            }
            Ok(())
        }
        Some(Commands::Setup(args)) => {
            commands::setup::run(args).await;
            Ok(())
        }
        Some(Commands::Doctor(args)) => commands::doctor::run(args).await,
        Some(Commands::Smoke(args)) => commands::smoke::run(args).await,
        None => {
            // Default mode: scrape (backwards compatible). This is the most
            // common invocation (`crw example.com --js`) — it must route
            // through the same teardown wrapper as the explicit Scrape arm.
            if let Some(url) = cli.url {
                install_signal_teardown();
                let args = commands::scrape::ScrapeArgs {
                    url,
                    format: cli.format.unwrap_or(Format::Markdown),
                    output: cli.output,
                    raw: cli.raw,
                    js: cli.js,
                    css: cli.css,
                    xpath: cli.xpath,
                    proxy: cli.proxy,
                    stealth: cli.stealth,
                    summary: cli.summary,
                    prompt: cli.prompt,
                    extract: cli.extract,
                    llm_provider: cli.llm_provider,
                    llm_key: cli.llm_key,
                    llm_model: cli.llm_model,
                    llm_base_url: cli.llm_base_url,
                };
                commands::scrape::run(args).await
            } else {
                // No URL provided — show help
                use clap::CommandFactory;
                Cli::command().print_help().unwrap();
                println!();
                Ok(())
            }
        }
    };

    finish(result);
}
