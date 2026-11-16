//! Optional post-setup registration for detected AI coding tools.
//!
//! The npm `crw-mcp` package owns the per-agent config formats. This module
//! deliberately does only three things: mirror its lightweight directory
//! detection, establish consent, and delegate the selected targets to that
//! installer. Keeping the mutation logic in one place prevents the Rust
//! wizard and npm entry point from drifting.
//!
//! Consent has two shapes. The interactive wizard asks which tools to touch.
//! The scripted paths, `--api-key` (what `curl … | CRW_API_KEY=… sh` runs) and
//! `--local --non-interactive`, take the command the user deliberately ran as
//! the consent and register every detected tool, because there is no tty to
//! prompt on. Either way
//! `--no-agents` opts out, only already-configured tools are touched, and the
//! installer merges rather than replaces.

use crate::commands::setup::{shell, ui};
use dialoguer::{MultiSelect, Select};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Agent {
    name: &'static str,
    flag: &'static str,
    config_dir: &'static str,
}

const AGENTS: &[Agent] = &[
    Agent {
        name: "Claude Code",
        flag: "--claude-code",
        config_dir: ".claude",
    },
    Agent {
        name: "Cursor",
        flag: "--cursor",
        config_dir: ".cursor",
    },
    Agent {
        name: "Gemini CLI",
        flag: "--gemini-cli",
        config_dir: ".gemini",
    },
    Agent {
        name: "Codex",
        flag: "--codex",
        config_dir: ".codex",
    },
    Agent {
        name: "OpenCode",
        flag: "--opencode",
        config_dir: ".opencode",
    },
    Agent {
        name: "Windsurf",
        flag: "--windsurf",
        config_dir: ".codeium",
    },
];

/// Offer MCP + skill registration after the primary Cloud/Local setup has
/// already succeeded. Every failure below is non-fatal: an integration helper
/// must never turn a valid CRW configuration into a failed setup.
pub fn offer_install() {
    let Some(home) = shell::home_dir() else {
        return;
    };
    let detected = detect_agents(&home);
    if detected.is_empty() {
        return;
    }

    println!();
    ui::print_section_header("AI TOOL INTEGRATION (OPTIONAL)");
    println!(
        "  Detected: {}",
        detected
            .iter()
            .map(|agent| agent.name)
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("  Add the CRW MCP server and skill to your AI tools?");
    println!();

    let choices = if detected.len() == 1 {
        vec![
            format!("Install for {} (Recommended)", detected[0].name),
            "Skip".to_string(),
        ]
    } else {
        vec![
            "Install for all detected tools (Recommended)".to_string(),
            "Choose tools".to_string(),
            "Skip".to_string(),
        ]
    };

    let choice = match Select::with_theme(&ui::select_style())
        .items(&choices)
        .default(0)
        .interact_opt()
    {
        Ok(Some(choice)) => choice,
        Ok(None) => {
            ui::print_info("Skipping AI tool integration.");
            return;
        }
        Err(error) => {
            ui::print_warning(&format!(
                "Could not open the AI tool prompt ({error}); CRW setup is still complete."
            ));
            return;
        }
    };

    let selected = if choice == 0 {
        detected
    } else if detected.len() > 1 && choice == 1 {
        choose_agents(&detected)
    } else {
        ui::print_info("Skipping AI tool integration.");
        return;
    };

    if selected.is_empty() {
        ui::print_info("No AI tools selected; skipping integration.");
        return;
    }

    run_installer(&selected);
}

/// Register with every detected tool without prompting.
///
/// Used by the scripted `--api-key` path, which runs under `curl … | sh` and
/// therefore has no tty to ask on. Announces what it touched instead of asking
/// first; `--no-agents` is the opt-out. Non-fatal like [`offer_install`]: a
/// failed integration must never fail an otherwise valid setup.
pub fn install_detected() {
    let Some(home) = shell::home_dir() else {
        return;
    };
    let detected = detect_agents(&home);
    if detected.is_empty() {
        return;
    }

    println!();
    ui::print_section_header("AI TOOL INTEGRATION");
    ui::print_detail(&format!(
        "Detected: {}",
        detected
            .iter()
            .map(|agent| agent.name)
            .collect::<Vec<_>>()
            .join(", ")
    ));
    ui::print_detail("Skip this by adding `--no-agents` to `crw setup`.");

    run_installer(&detected);
}

fn detect_agents(home: &Path) -> Vec<Agent> {
    AGENTS
        .iter()
        .copied()
        .filter(|agent| home.join(agent.config_dir).is_dir())
        .collect()
}

fn choose_agents(detected: &[Agent]) -> Vec<Agent> {
    let names: Vec<&str> = detected.iter().map(|agent| agent.name).collect();
    let defaults = vec![true; detected.len()];
    match MultiSelect::with_theme(&ui::select_style())
        .with_prompt("  Select tools")
        .items(&names)
        .defaults(&defaults)
        .interact_opt()
    {
        Ok(Some(indices)) => indices
            .into_iter()
            .filter_map(|index| detected.get(index).copied())
            .collect(),
        Ok(None) => Vec::new(),
        Err(error) => {
            ui::print_warning(&format!("Could not read tool selection ({error})."));
            Vec::new()
        }
    }
}

fn installer_args(selected: &[Agent]) -> Vec<String> {
    let mut args = vec![
        "-y".to_string(),
        format!("crw-mcp@{}", env!("CARGO_PKG_VERSION")),
        "install".to_string(),
        "--from-config".to_string(),
    ];
    args.extend(selected.iter().map(|agent| agent.flag.to_string()));
    args
}

fn manual_command(selected: &[Agent]) -> String {
    format!("npx {}", installer_args(selected).join(" "))
}

fn run_installer(selected: &[Agent]) {
    println!();
    ui::print_info("Installing CRW MCP and skill…");
    let args = installer_args(selected);
    let result = Command::new("npx")
        .args(&args)
        // The MCP launcher reads the config.toml just written by setup. Do not
        // copy credentials into every agent config, even if the parent shell
        // still has legacy CRW_* exports.
        .env_remove("CRW_API_KEY")
        .env_remove("CRW_API_URL")
        .status();

    match result {
        Ok(status) if status.success() => {
            ui::print_success(&format!(
                "MCP installed for {}",
                selected
                    .iter()
                    .map(|agent| agent.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            ui::print_detail("Restart those tools to load the CRW MCP server.");
        }
        Ok(status) => {
            ui::print_warning(&format!(
                "MCP installer exited with status {status}; CRW setup is still complete."
            ));
            ui::print_detail(&format!("Retry: {}", manual_command(selected)));
        }
        Err(error) => {
            ui::print_warning(&format!(
                "Could not start npx ({error}); CRW setup is still complete."
            ));
            ui::print_detail(&format!(
                "After installing Node.js, run: {}",
                manual_command(selected)
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_matches_only_existing_agent_directories() {
        let home = tempfile::tempdir().expect("temp home");
        std::fs::create_dir_all(home.path().join(".codex")).expect("codex dir");
        std::fs::create_dir_all(home.path().join(".cursor")).expect("cursor dir");
        std::fs::write(home.path().join(".claude"), "not a directory").expect("claude marker file");

        let detected = detect_agents(home.path());
        assert_eq!(
            detected.iter().map(|agent| agent.name).collect::<Vec<_>>(),
            vec!["Cursor", "Codex"]
        );
    }

    #[test]
    fn delegated_command_is_version_pinned_and_contains_no_secret() {
        let selected = [AGENTS[0], AGENTS[3]];
        let args = installer_args(&selected);
        assert_eq!(args[0], "-y");
        assert_eq!(args[1], format!("crw-mcp@{}", env!("CARGO_PKG_VERSION")));
        assert!(args.contains(&"install".to_string()));
        assert!(args.contains(&"--from-config".to_string()));
        assert!(args.contains(&"--claude-code".to_string()));
        assert!(args.contains(&"--codex".to_string()));
        assert!(!args.iter().any(|arg| arg.contains("api-key")));
    }
}
