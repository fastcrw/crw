//! LLM provider setup for AI-powered features (summary, search answers, structured extraction).

use crate::commands::setup::ui::{self, SetupError};
use console::style;
use dialoguer::{Input, Select};

/// Supported LLM providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    Anthropic,
    OpenAI,
    Responses,
    DeepSeek,
    Azure,
    OpenRouter,
    Custom,
    Skip,
}

impl LlmProvider {
    pub fn name(&self) -> &'static str {
        match self {
            LlmProvider::Anthropic => "Anthropic (Claude)",
            LlmProvider::OpenAI => "OpenAI (GPT)",
            LlmProvider::Responses => "OpenAI Responses-compatible",
            LlmProvider::DeepSeek => "DeepSeek",
            LlmProvider::Azure => "Azure OpenAI",
            LlmProvider::OpenRouter => "OpenRouter",
            LlmProvider::Custom => "Custom (OpenAI-compatible)",
            LlmProvider::Skip => "Skip",
        }
    }

    pub fn config_value(&self) -> &'static str {
        match self {
            LlmProvider::Anthropic => "anthropic",
            LlmProvider::OpenAI => "openai",
            LlmProvider::Responses => "openai-responses",
            LlmProvider::DeepSeek => "deepseek",
            LlmProvider::Azure => "azure",
            LlmProvider::OpenRouter => "openai", // OpenRouter uses OpenAI protocol
            LlmProvider::Custom => "openai",     // Custom uses OpenAI protocol
            LlmProvider::Skip => "",
        }
    }

    pub fn default_base_url(&self) -> Option<&'static str> {
        match self {
            LlmProvider::Anthropic => None, // Uses default
            LlmProvider::OpenAI => None,    // Uses default
            LlmProvider::Responses => None, // User must provide (OpenAI or gateway base)
            LlmProvider::DeepSeek => Some("https://api.deepseek.com/v1"),
            LlmProvider::Azure => None, // User must provide
            LlmProvider::OpenRouter => Some("https://openrouter.ai/api/v1"),
            LlmProvider::Custom => None, // User must provide
            LlmProvider::Skip => None,
        }
    }

    pub fn default_models(&self) -> Vec<(&'static str, &'static str)> {
        match self {
            LlmProvider::Anthropic => vec![
                (
                    "claude-sonnet-4-6",
                    "Claude Sonnet 4.6 (Recommended, $3/$15 per M tokens)",
                ),
                (
                    "claude-haiku-5-5",
                    "Claude Haiku 5.5 (Fast & cheap, $0.10/$0.50 per M tokens up to 100K prompt tokens)",
                ),
                (
                    "claude-opus-5-5",
                    "Claude Opus 5.5 (Most capable, $4/$20 per M tokens)",
                ),
            ],
            LlmProvider::OpenAI => vec![
                (
                    "gpt-4o-mini",
                    "GPT-4o Mini (Recommended, $0.15/$0.6 per M tokens)",
                ),
                ("gpt-4o", "GPT-4o (More capable, $2.5/$10 per M tokens)"),
                ("gpt-4-turbo", "GPT-4 Turbo"),
            ],
            LlmProvider::Responses => vec![],
            LlmProvider::DeepSeek => vec![
                (
                    "deepseek-chat",
                    "DeepSeek Chat (Recommended, $0.27/$1.1 per M tokens)",
                ),
                (
                    "deepseek-reasoner",
                    "DeepSeek Reasoner ($0.55/$2.19 per M tokens)",
                ),
            ],
            LlmProvider::Azure => vec![
                ("gpt-4o-mini", "GPT-4o Mini deployment"),
                ("gpt-4o", "GPT-4o deployment"),
            ],
            LlmProvider::OpenRouter => vec![
                ("anthropic/claude-sonnet-4", "Claude Sonnet 4"),
                ("openai/gpt-4o-mini", "GPT-4o Mini"),
                ("deepseek/deepseek-chat", "DeepSeek Chat"),
                ("google/gemini-pro-1.5", "Gemini Pro 1.5"),
            ],
            LlmProvider::Custom => vec![],
            LlmProvider::Skip => vec![],
        }
    }

    #[allow(dead_code)]
    pub fn api_key_hint(&self) -> &'static str {
        match self {
            LlmProvider::Anthropic => "sk-ant-... (from console.anthropic.com)",
            LlmProvider::OpenAI => "sk-... (from platform.openai.com)",
            LlmProvider::Responses => "Provider API key",
            LlmProvider::DeepSeek => "sk-... (from platform.deepseek.com)",
            LlmProvider::Azure => "Azure API key (from Azure Portal)",
            LlmProvider::OpenRouter => "sk-or-... (from openrouter.ai)",
            LlmProvider::Custom => "Your API key",
            LlmProvider::Skip => "",
        }
    }

    pub fn dashboard_url(&self) -> Option<&'static str> {
        match self {
            LlmProvider::Anthropic => Some("https://console.anthropic.com/settings/keys"),
            LlmProvider::OpenAI => Some("https://platform.openai.com/api-keys"),
            LlmProvider::Responses => None,
            LlmProvider::DeepSeek => Some("https://platform.deepseek.com/api_keys"),
            LlmProvider::Azure => Some("https://portal.azure.com"),
            LlmProvider::OpenRouter => Some("https://openrouter.ai/keys"),
            LlmProvider::Custom => None,
            LlmProvider::Skip => None,
        }
    }
}

/// LLM configuration result.
#[derive(Debug, Clone)]
pub struct LlmSetupResult {
    pub provider: LlmProvider,
    pub api_key: String,
    pub model: String,
    pub base_url: Option<String>,
    pub azure_api_version: Option<String>,
}

/// Run the LLM setup flow.
pub async fn run() -> Result<Option<LlmSetupResult>, SetupError> {
    println!();
    println!("  CRW supports AI-powered features for smarter scraping:");
    println!();
    println!(
        "  • {} - Generate concise summaries of pages",
        style("Summary").cyan()
    );
    println!(
        "  • {} - Answer questions using search results",
        style("Search Answers").cyan()
    );
    println!(
        "  • {} - Extract data into JSON schema",
        style("Structured Extraction").cyan()
    );
    println!();
    println!("  These features require an LLM API key (BYOK - Bring Your Own Key).");
    println!();

    // Provider selection
    let provider = prompt_provider()?;

    if provider == LlmProvider::Skip {
        ui::print_info("Skipping LLM setup (you can configure later)");
        return Ok(None);
    }

    // Show dashboard URL
    if let Some(url) = provider.dashboard_url() {
        println!();
        println!("  Get your API key: {}", style(url).cyan().underlined());
        println!();
    }

    // Get API key
    let api_key = prompt_api_key(provider)?;

    // Get model
    let model = prompt_model(provider)?;

    // Get base URL if needed
    let base_url = prompt_base_url(provider)?;

    // Get Azure API version if needed
    let azure_api_version = if provider == LlmProvider::Azure {
        Some(prompt_azure_version()?)
    } else {
        None
    };

    println!();
    ui::print_success(&format!("LLM configured: {} / {}", provider.name(), model));

    Ok(Some(LlmSetupResult {
        provider,
        api_key,
        model,
        base_url,
        azure_api_version,
    }))
}

/// Prompt for LLM provider selection.
fn prompt_provider() -> Result<LlmProvider, SetupError> {
    let items = vec![
        format!(
            "{}\n      • Best reasoning, great for summaries\n      • Models: Claude Sonnet 4.6, Haiku 5.5, Opus 5.5",
            style("Anthropic (Claude) - Recommended").bold()
        ),
        format!(
            "{}\n      • Wide compatibility, good pricing\n      • Models: GPT-4o, GPT-4o-mini",
            "OpenAI (GPT)"
        ),
        format!(
            "{}\n      • Native /v1/responses protocol\n      • OpenAI or any Responses-compatible gateway",
            "OpenAI Responses-compatible"
        ),
        format!(
            "{}\n      • Best value, excellent performance\n      • Models: DeepSeek Chat, Reasoner",
            "DeepSeek"
        ),
        format!(
            "{}\n      • Enterprise deployments\n      • Requires Azure subscription",
            "Azure OpenAI"
        ),
        format!(
            "{}\n      • Access multiple providers with one key\n      • Models from OpenAI, Anthropic, Google, etc.",
            "OpenRouter"
        ),
        format!(
            "{}\n      • Any OpenAI-compatible API\n      • Ollama, vLLM, LocalAI, etc.",
            "Custom endpoint"
        ),
        format!(
            "{}\n      • Configure later or use without LLM features\n      • Basic scraping still works",
            style("Skip for now").dim()
        ),
    ];

    let providers = [
        LlmProvider::Anthropic,
        LlmProvider::OpenAI,
        LlmProvider::Responses,
        LlmProvider::DeepSeek,
        LlmProvider::Azure,
        LlmProvider::OpenRouter,
        LlmProvider::Custom,
        LlmProvider::Skip,
    ];

    let choice = Select::with_theme(&ui::select_style())
        .with_prompt("  Which LLM provider would you like to use?")
        .items(&items)
        .default(0)
        .interact_opt()
        .map_err(ui::handle_dialoguer_error)?
        .ok_or(SetupError::Cancelled)?;

    Ok(providers[choice])
}

/// Prompt for API key.
fn prompt_api_key(provider: LlmProvider) -> Result<String, SetupError> {
    let api_key: String = Input::with_theme(&ui::select_style())
        .with_prompt(format!("  Enter your {} API key", provider.name()))
        .validate_with(|input: &String| {
            if input.trim().is_empty() {
                Err("API key cannot be empty")
            } else if input.trim().len() < 10 {
                Err("API key seems too short")
            } else {
                Ok(())
            }
        })
        .interact_text()
        .map_err(ui::handle_dialoguer_error)?;

    Ok(api_key.trim().to_string())
}

/// Prompt for model selection.
fn prompt_model(provider: LlmProvider) -> Result<String, SetupError> {
    let models = provider.default_models();

    if models.is_empty() {
        // Custom provider - ask for model name
        let model: String = Input::with_theme(&ui::select_style())
            .with_prompt("  Enter the model name")
            .default("gpt-4o-mini".to_string())
            .interact_text()
            .map_err(ui::handle_dialoguer_error)?;

        return Ok(model.trim().to_string());
    }

    let items: Vec<String> = models.iter().map(|(_, desc)| desc.to_string()).collect();

    let choice = Select::with_theme(&ui::select_style())
        .with_prompt("  Which model would you like to use?")
        .items(&items)
        .default(0)
        .interact_opt()
        .map_err(ui::handle_dialoguer_error)?
        .ok_or(SetupError::Cancelled)?;

    Ok(models[choice].0.to_string())
}

/// Prompt for base URL (for custom/Azure providers).
fn prompt_base_url(provider: LlmProvider) -> Result<Option<String>, SetupError> {
    // Return default URL if provider has one
    if let Some(url) = provider.default_base_url() {
        return Ok(Some(url.to_string()));
    }

    // Only ask for custom, Responses-compatible, and Azure providers.
    if provider != LlmProvider::Custom
        && provider != LlmProvider::Responses
        && provider != LlmProvider::Azure
    {
        return Ok(None);
    }

    let prompt = if provider == LlmProvider::Azure {
        "  Enter your Azure OpenAI endpoint (e.g., https://myresource.openai.azure.com)"
    } else if provider == LlmProvider::Responses {
        "  Enter the Responses API base URL (e.g., https://gateway.example.com/v1)"
    } else {
        "  Enter the API base URL (e.g., http://localhost:11434/v1)"
    };

    let url: String = Input::with_theme(&ui::select_style())
        .with_prompt(prompt)
        .validate_with(|input: &String| {
            if input.trim().is_empty() {
                Err("URL cannot be empty")
            } else if !input.starts_with("http://") && !input.starts_with("https://") {
                Err("URL must start with http:// or https://")
            } else {
                Ok(())
            }
        })
        .interact_text()
        .map_err(ui::handle_dialoguer_error)?;

    Ok(Some(url.trim().to_string()))
}

/// Prompt for Azure API version.
fn prompt_azure_version() -> Result<String, SetupError> {
    let version: String = Input::with_theme(&ui::select_style())
        .with_prompt("  Enter the Azure API version")
        .default("2024-05-01-preview".to_string())
        .interact_text()
        .map_err(ui::handle_dialoguer_error)?;

    Ok(version.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_wizard_default_is_the_config_default() {
        assert_eq!(
            LlmProvider::Anthropic.default_models()[0].0,
            crw_core::config::LlmConfig::default().model
        );
    }
}
