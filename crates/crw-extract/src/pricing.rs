//! Best-effort LLM pricing table for cost estimation.
//!
//! Source: provider docs as of snapshot date 2026-05-12.
//! - Anthropic: <https://docs.anthropic.com/en/docs/about-claude/models>
//! - OpenAI:    <https://openai.com/api/pricing/>
//! - DeepSeek:  <https://api-docs.deepseek.com/quick_start/pricing>
//!
//! Prices drift; this table is a snapshot, not authoritative.
//! `estimated_cost_usd` returned to API consumers is best-effort and
//! MUST NOT be used for customer billing.

use phf::phf_map;

/// (input $/M tokens, output $/M tokens)
///
/// Keys are matched by prefix — anything starting with a known model
/// family gets its rate. Unknown models return `None`.
static PRICING: phf::Map<&'static str, (f64, f64)> = phf_map! {
    // Anthropic
    "claude-sonnet-5-5"     => (2.0, 10.0),
    "claude-opus-5-5"       => (4.0, 20.0),
    "claude-sonnet-4"       => (3.0, 15.0),
    "claude-haiku-4-5"      => (1.0, 5.0),
    "claude-opus-4"         => (15.0, 75.0),
    "claude-3-5-sonnet"     => (3.0, 15.0),
    "claude-3-5-haiku"      => (0.8, 4.0),
    // OpenAI
    "gpt-4o-mini"           => (0.15, 0.6),
    "gpt-4o"                => (2.5, 10.0),
    "gpt-4.1-mini"          => (0.4, 1.6),
    "gpt-4.1"               => (2.0, 8.0),
    "gpt-4-turbo"           => (10.0, 30.0),
    // DeepSeek
    "deepseek-chat"         => (0.27, 1.1),
    "deepseek-coder"        => (0.27, 1.1),
    "deepseek-reasoner"     => (0.55, 2.19),
};

/// Returns `(input_rate, output_rate)` in USD per million tokens for the
/// given model, or `None` if the model is not in the pricing table.
fn lookup_rates(model: &str) -> Option<(f64, f64)> {
    // Longest-prefix wins so `gpt-4o-mini-...` matches `gpt-4o-mini`, not
    // `gpt-4o`. `phf::Map` iteration is unordered, so we can't rely on
    // declaration order.
    let mut best: Option<(&&str, &(f64, f64))> = None;
    for entry in PRICING.entries() {
        if model.starts_with(entry.0) && best.map(|(p, _)| entry.0.len() > p.len()).unwrap_or(true)
        {
            best = Some(entry);
        }
    }
    best.map(|(_, rates)| *rates)
}

/// Best-effort cost in USD for a given model and token counts.
/// Returns `None` if the model is unknown — never panics.
pub fn calculate_cost(model: &str, input_tokens: u32, output_tokens: u32) -> Option<f64> {
    let (input_rate, output_rate) = lookup_rates(model)?;
    let input_cost = (input_tokens as f64 / 1_000_000.0) * input_rate;
    let output_cost = (output_tokens as f64 / 1_000_000.0) * output_rate;
    Some(input_cost + output_cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_models_have_cost() {
        let cost = calculate_cost("gpt-4o-mini", 1_000_000, 1_000_000).unwrap();
        assert!((cost - 0.75).abs() < 1e-9);
        let cost = calculate_cost("claude-sonnet-5-5", 1_000_000, 1_000_000).unwrap();
        assert!((cost - 12.0).abs() < 1e-9);
    }

    #[test]
    fn prefix_match_picks_up_versioned_ids() {
        assert!(calculate_cost("claude-sonnet-4-20250514", 100, 100).is_some());
        assert!(calculate_cost("gpt-4o-mini-2024-07-18", 100, 100).is_some());
        assert!(calculate_cost("deepseek-chat", 100, 100).is_some());
    }

    #[test]
    fn unknown_model_returns_none() {
        assert!(calculate_cost("totally-fake-model", 100, 100).is_none());
    }

    #[test]
    fn zero_tokens_yields_zero_cost() {
        let cost = calculate_cost("gpt-4o-mini", 0, 0).unwrap();
        assert_eq!(cost, 0.0);
    }
}
