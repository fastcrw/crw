//! Interactive setup wizard for CRW.
//!
//! Guides users through Cloud or Local installation with clear
//! explanations at each step.

mod agents;
mod browser;
mod cloud;
pub(crate) use crw_core::user_config as config_file;
mod docker;
pub(crate) mod llm;
mod local;
mod searxng;
mod shell;
pub mod ui;
mod wizard;

use clap::Args;

/// Setup command arguments.
#[derive(Args)]
pub struct SetupArgs {
    /// Skip interactive prompts and use defaults (for scripting).
    #[arg(long)]
    pub non_interactive: bool,

    /// Force cloud setup mode.
    #[arg(long, conflicts_with = "local")]
    pub cloud: bool,

    /// Force local setup mode.
    #[arg(long, conflicts_with = "cloud")]
    pub local: bool,

    /// Connect to CRW Cloud non-interactively with this API key. Validates the
    /// key, writes ~/.config/crw/config.toml (pointed at api.fastcrw.com), and
    /// skips every prompt. Implies cloud mode — get a key at
    /// https://fastcrw.com/dashboard (1000 free credits).
    #[arg(long, value_name = "KEY", conflicts_with_all = ["local", "reset", "reset_shell"])]
    pub api_key: Option<String>,

    /// Skip registering the CRW MCP server and skill with your AI coding tools.
    ///
    /// Registration is on by default: the interactive wizard asks which tools
    /// to use, and `--api-key` installs into every detected one. Only tools
    /// that are already configured are touched, and the installer merges into
    /// their existing MCP config rather than replacing it.
    #[arg(long)]
    pub no_agents: bool,

    /// Disable colored output.
    #[arg(long)]
    pub no_color: bool,

    /// Strip every `# CRW Configuration` block from the shell rc file and exit.
    ///
    /// Run this once after upgrading to the config.toml-first setup if your
    /// `.zshrc` / `.bashrc` accumulated duplicate `export CRW_*` lines from
    /// earlier setup runs. Won't touch `~/.config/crw/config.toml`.
    #[arg(long, conflicts_with_all = ["cloud", "local", "reset"])]
    pub reset_shell: bool,

    /// Remove all CRW setup state: `~/.config/crw/config.toml`,
    /// the first-run-hint sentinel, and any `# CRW Configuration`
    /// blocks in your shell rc. Asks for confirmation unless `--yes`
    /// is also passed.
    #[arg(long, conflicts_with_all = ["cloud", "local", "reset_shell"])]
    pub reset: bool,

    /// Skip the confirmation prompt for `--reset` (for scripting).
    #[arg(long, requires = "reset")]
    pub yes: bool,
}

/// Run the setup command.
pub async fn run(args: SetupArgs) {
    // Initialize color settings
    ui::init_color(args.no_color);

    // Short-circuit: --reset-shell does one job and exits.
    if args.reset_shell {
        let res = run_reset_shell();
        match res {
            Ok(()) => return,
            Err(e) => {
                eprintln!();
                eprintln!("  Reset failed: {}", e);
                eprintln!();
                std::process::exit(1);
            }
        }
    }

    // Short-circuit: --reset wipes everything setup created.
    if args.reset {
        match run_full_reset(args.yes) {
            Ok(()) => return,
            Err(e) => {
                if matches!(e, ui::SetupError::Cancelled) {
                    println!();
                    println!("  Reset cancelled. Nothing was removed.");
                    println!();
                    std::process::exit(130);
                }
                eprintln!();
                eprintln!("  Reset failed: {}", e);
                eprintln!();
                std::process::exit(1);
            }
        }
    }

    // MCP registration runs by default, because connecting an agent is the
    // point of setup rather than an afterthought. `--api-key` arrives from a
    // piped installer with no tty, so it registers every detected tool and
    // announces it; the interactive wizard still asks which ones. `--no-agents`
    // opts out of both.
    let scripted_agent_setup = args.api_key.is_some();
    let agent_setup = !args.no_agents && (scripted_agent_setup || !args.non_interactive);

    // If specific mode is requested, run that directly
    let result = if let Some(key) = args.api_key.clone() {
        // Non-interactive cloud connect (--api-key / installer pass-through).
        cloud::run_with_key(key).await
    } else if args.cloud {
        if args.non_interactive {
            // Cloud setup has no safe default for the API key prompt — fail
            // fast instead of guessing or hanging on stdin.
            Err(ui::SetupError::Other(
                "cloud setup needs an API key in --non-interactive mode: pass --api-key <KEY> \
                 (get one at https://fastcrw.com/dashboard) or drop --non-interactive."
                    .to_string(),
            ))
        } else {
            cloud::run().await
        }
    } else if args.local {
        local::run(args.non_interactive).await
    } else if args.non_interactive {
        Err(ui::SetupError::Other(
            "--non-interactive needs --local, or --cloud with --api-key, to know which setup to run."
                .to_string(),
        ))
    } else {
        // Interactive wizard
        wizard::run_wizard().await
    };

    match result {
        Ok(()) => {
            if agent_setup {
                if scripted_agent_setup {
                    agents::install_detected();
                } else {
                    agents::offer_install();
                }
            }
        }
        Err(e) => {
            // Check if it was a cancellation
            if let ui::SetupError::Cancelled = e {
                ui::print_cancelled();
                std::process::exit(130); // Standard exit code for Ctrl+C
            } else {
                eprintln!();
                eprintln!("  Setup failed: {}", e);
                eprintln!();
                std::process::exit(1);
            }
        }
    }
}

/// Implementation of `--reset`. Wipes every piece of state setup creates:
/// the per-user config.toml, the first-run hint sentinel, and any
/// `# CRW Configuration` blocks in the shell rc. Asks once for confirmation
/// unless `--yes` was passed.
fn run_full_reset(assume_yes: bool) -> Result<(), ui::SetupError> {
    use dialoguer::{Confirm, theme::ColorfulTheme};

    let cfg_path = config_file::user_config_path().map_err(ui::SetupError::Other)?;
    let sentinel = cfg_path.with_file_name(".first-run-hint-shown");
    let shell_kind = shell::detect_shell();

    println!();
    println!("  This will remove:");
    println!("    • {}", cfg_path.display());
    println!("    • {}", sentinel.display());
    if shell_kind != shell::Shell::Unknown {
        println!("    • any `# CRW Configuration` blocks in your shell rc");
    }
    println!();

    if !assume_yes {
        let confirm = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("Proceed?")
            .default(false)
            .interact()
            .map_err(ui::handle_dialoguer_error)?;
        if !confirm {
            return Err(ui::SetupError::Cancelled);
        }
    }

    let mut removed: Vec<String> = Vec::new();

    if cfg_path.exists() {
        std::fs::remove_file(&cfg_path)
            .map_err(|e| ui::SetupError::Other(format!("remove {}: {}", cfg_path.display(), e)))?;
        removed.push(cfg_path.display().to_string());
    }

    if sentinel.exists() {
        std::fs::remove_file(&sentinel)
            .map_err(|e| ui::SetupError::Other(format!("remove {}: {}", sentinel.display(), e)))?;
        removed.push(sentinel.display().to_string());
    }

    if shell_kind != shell::Shell::Unknown {
        match shell::reset_rc(shell_kind) {
            Ok(report) if report.lines_removed > 0 => {
                removed.push(format!(
                    "{} ({} line(s))",
                    report.rc_path.display(),
                    report.lines_removed
                ));
            }
            Ok(_) => {}
            Err(e) => return Err(ui::SetupError::Other(e)),
        }
    }

    println!();
    if removed.is_empty() {
        println!("  Nothing to clean up — setup state was already empty.");
    } else {
        println!("  Removed:");
        for entry in &removed {
            println!("    • {}", entry);
        }
    }
    println!();
    println!("  Run `crw setup` any time to reconfigure.");
    println!();
    Ok(())
}

/// Implementation of `--reset-shell`. Pulled out of `run()` so the early-exit
/// path stays readable and the error handling lives in one place.
fn run_reset_shell() -> Result<(), String> {
    let shell_kind = shell::detect_shell();
    if shell_kind == shell::Shell::Unknown {
        return Err("Could not detect your shell. Edit your rc file manually.".into());
    }

    let report = shell::reset_rc(shell_kind)?;
    if report.lines_removed == 0 {
        println!(
            "  No CRW Configuration blocks found in {}",
            report.rc_path.display()
        );
        println!("  Nothing to clean up.");
        return Ok(());
    }
    println!(
        "  Cleaned {} line(s) from {}",
        report.lines_removed,
        report.rc_path.display()
    );
    println!(
        "  Open a new shell or run `source {}` to apply.",
        report.rc_path.display()
    );
    Ok(())
}
