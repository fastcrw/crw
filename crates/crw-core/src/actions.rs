//! Browser actions for a scrape: an ordered list of steps (`wait`, `click`,
//! `write`, `press`, `scroll`) run on the rendered page before its HTML is read.
//!
//! The wire shape is Firecrawl's `actions` for those five types, so a Firecrawl
//! client's list is accepted unchanged. One addition: `optional` on the steps
//! that look up an element, which turns "no element matched" into a skipped
//! step (reported in `warnings`) instead of a failed scrape. That is what lets
//! a caller click "Load more" up to N times on a list whose length it does not
//! know.
//!
//! The request carries the list as raw JSON (`ScrapeRequest::actions`) and it
//! is parsed here, so every surface reports the same, specific 400.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::MAX_WAIT_FOR_MS;
use crate::error::{CrwError, CrwResult};

/// Most actions one scrape may carry (Firecrawl's limit).
pub const MAX_ACTIONS: usize = 50;
/// Longest selector accepted, in bytes.
pub const MAX_SELECTOR_LEN: usize = 1_000;
/// Longest `write` text accepted, in chars.
pub const MAX_WRITE_CHARS: usize = 10_000;
/// How long a `wait` on a selector looks for its element.
pub const SELECTOR_WAIT_MS: u64 = 10_000;
/// How long `click` and `scroll` look for their element before giving up.
pub const ELEMENT_LOOKUP_MS: u64 = 5_000;
/// Per-step allowance for the CDP round trips of a step, on top of its waits.
const STEP_OVERHEAD_MS: u64 = 250;

/// The Firecrawl action types that produce an output. Named in the error so a
/// Firecrawl caller learns what to use instead.
const OUTPUT_TYPES: &[&str] = &["screenshot", "scrape", "executeJavascript", "pdf"];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrollDirection {
    Up,
    #[default]
    Down,
}

/// One step. Field names are camelCase on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ScrapeAction {
    /// Sleep `milliseconds`, or wait until `selector` matches an element.
    /// Exactly one of the two.
    Wait {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        milliseconds: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        optional: bool,
    },
    /// Click the first element matching `selector`, or every match with `all`.
    Click {
        selector: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        all: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        optional: bool,
    },
    /// Type `text` into the focused element (click the field first).
    Write { text: String },
    /// Press one key: a named key (`Enter`, `Tab`, `Escape`, `ArrowDown`, ...)
    /// or a single character.
    Press { key: String },
    /// Scroll the page, or the element matching `selector`, by one screen.
    Scroll {
        #[serde(default)]
        direction: ScrollDirection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        optional: bool,
    },
}

impl ScrapeAction {
    /// The wire `type`, for messages.
    pub fn kind(&self) -> &'static str {
        match self {
            ScrapeAction::Wait { .. } => "wait",
            ScrapeAction::Click { .. } => "click",
            ScrapeAction::Write { .. } => "write",
            ScrapeAction::Press { .. } => "press",
            ScrapeAction::Scroll { .. } => "scroll",
        }
    }

    /// The selector the step looks up, if any.
    pub fn selector(&self) -> Option<&str> {
        match self {
            ScrapeAction::Wait { selector, .. } | ScrapeAction::Scroll { selector, .. } => {
                selector.as_deref()
            }
            ScrapeAction::Click { selector, .. } => Some(selector),
            ScrapeAction::Write { .. } | ScrapeAction::Press { .. } => None,
        }
    }

    /// Whether a missing element skips this step instead of failing the scrape.
    pub fn is_optional(&self) -> bool {
        match self {
            ScrapeAction::Wait { optional, .. }
            | ScrapeAction::Click { optional, .. }
            | ScrapeAction::Scroll { optional, .. } => *optional,
            ScrapeAction::Write { .. } | ScrapeAction::Press { .. } => false,
        }
    }

    /// `action 3 (click "button.more")`, for warnings and errors.
    pub fn describe(&self, index: usize) -> String {
        match self.selector() {
            Some(sel) => format!("action {} ({} {:?})", index + 1, self.kind(), sel),
            None => format!("action {} ({})", index + 1, self.kind()),
        }
    }
}

/// What running a request's actions on one page produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionsOutcome {
    /// The first strict step that could not be carried out, described for the
    /// caller. Steps after it did not run.
    pub failed: Option<String>,
    /// One line per optional step skipped because its element was missing.
    pub skipped: Vec<String>,
}

/// Parse and validate the raw `actions` value of a scrape request.
///
/// `null`, absent and `[]` are all "no actions" and give an empty list.
/// `wait_for` is the request's `waitFor`, which shares the 60 s wait ceiling
/// with the `wait` steps (Firecrawl's rule).
pub fn parse_actions(raw: Option<&Value>, wait_for: Option<u64>) -> CrwResult<Vec<ScrapeAction>> {
    let items = match raw {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => return Err(invalid("'actions' must be an array of action objects")),
    };
    if items.len() > MAX_ACTIONS {
        return Err(invalid(format!(
            "'actions' has {} steps; at most {MAX_ACTIONS} are allowed",
            items.len()
        )));
    }
    let mut actions = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let kind = item.get("type").and_then(Value::as_str);
        if let Some(kind) = kind.filter(|k| OUTPUT_TYPES.contains(k)) {
            return Err(invalid(format!(
                "actions[{i}]: type '{kind}' is not supported. Supported types: wait, click, \
                 write, press, scroll. For a capture of the final page, add \"screenshot\" to \
                 'formats'."
            )));
        }
        let action: ScrapeAction = serde_json::from_value(item.clone())
            .map_err(|e| invalid(format!("actions[{i}]: {e}")))?;
        check_action(i, &action)?;
        actions.push(action);
    }
    let waited: u64 = wait_for.unwrap_or(0).saturating_add(
        actions
            .iter()
            .filter_map(|a| match a {
                ScrapeAction::Wait { milliseconds, .. } => *milliseconds,
                _ => None,
            })
            .fold(0u64, u64::saturating_add),
    );
    if waited > MAX_WAIT_FOR_MS {
        return Err(invalid(format!(
            "total wait time (waitFor plus every wait action) is {waited} ms; at most \
             {MAX_WAIT_FOR_MS} ms is allowed"
        )));
    }
    Ok(actions)
}

fn check_action(i: usize, action: &ScrapeAction) -> CrwResult<()> {
    if let Some(sel) = action.selector() {
        if sel.trim().is_empty() {
            return Err(invalid(format!(
                "actions[{i}]: 'selector' must not be empty"
            )));
        }
        if sel.len() > MAX_SELECTOR_LEN {
            return Err(invalid(format!(
                "actions[{i}]: 'selector' is longer than {MAX_SELECTOR_LEN} bytes"
            )));
        }
    }
    match action {
        ScrapeAction::Wait {
            milliseconds,
            selector,
            ..
        } => match (milliseconds, selector) {
            (Some(_), Some(_)) | (None, None) => Err(invalid(format!(
                "actions[{i}]: a wait takes exactly one of 'milliseconds' or 'selector'"
            ))),
            (Some(0), None) => Err(invalid(format!(
                "actions[{i}]: 'milliseconds' must be greater than 0"
            ))),
            _ => Ok(()),
        },
        ScrapeAction::Write { text } if text.chars().count() > MAX_WRITE_CHARS => Err(invalid(
            format!("actions[{i}]: 'text' is longer than {MAX_WRITE_CHARS} characters"),
        )),
        ScrapeAction::Press { key } if key.is_empty() || key.chars().count() > 32 => Err(invalid(
            format!("actions[{i}]: 'key' must be a key name or one character"),
        )),
        ScrapeAction::Press { key } if key.chars().count() > 1 && key.contains('+') => {
            Err(invalid(format!(
                "actions[{i}]: key combinations like {key:?} are not supported; press one key"
            )))
        }
        _ => Ok(()),
    }
}

/// Upper bound on the time a list of actions can take, in ms: every fixed
/// wait, every element lookup at its full window, and a round-trip allowance
/// per step. Used to size the request deadline and the CDP tier's timeout, so
/// a valid list is never cut short by a budget sized for a plain page load.
/// Capped at [`MAX_WAIT_FOR_MS`], the same ceiling `waitFor` has.
pub fn actions_budget_ms(actions: &[ScrapeAction]) -> u64 {
    actions
        .iter()
        .map(|a| {
            let wait = match a {
                ScrapeAction::Wait {
                    milliseconds: Some(ms),
                    ..
                } => *ms,
                ScrapeAction::Wait { .. } => SELECTOR_WAIT_MS,
                // A scroll without a selector normally answers at once, but it
                // waits out a navigation for up to the same window.
                ScrapeAction::Click { .. } | ScrapeAction::Scroll { .. } => ELEMENT_LOOKUP_MS,
                _ => 0,
            };
            wait.saturating_add(STEP_OVERHEAD_MS)
        })
        .fold(0u64, u64::saturating_add)
        .min(MAX_WAIT_FOR_MS)
}

/// The `wait_for` to size a scrape's implicit deadline with, so a valid action
/// list fits inside it. Pass the result to
/// [`crate::AppConfig::effective_deadline_ms`] in place of the bare `waitFor`.
///
/// Unchanged when there are no actions (or the list is invalid, which fails
/// validation before any fetch). Otherwise the CDP wait the tier would have
/// spent anyway (`waitFor`, or the 8 s SPA poll when it is absent, as
/// `effective_deadline_ms` already assumes) plus the list's budget, capped at
/// [`MAX_WAIT_FOR_MS`] so the Tower envelope, sized from that same ceiling,
/// still covers it.
pub fn deadline_wait_for(raw: Option<&Value>, wait_for: Option<u64>) -> Option<u64> {
    // Mirrors `crw_renderer::cdp::SPA_SELECTOR_MAX_MS`, like the copy in
    // `effective_deadline_ms`.
    const SPA_DEFAULT_MS: u64 = 8_000;
    match parse_actions(raw, wait_for) {
        Ok(list) if !list.is_empty() => Some(
            wait_for
                .unwrap_or(SPA_DEFAULT_MS)
                .saturating_add(actions_budget_ms(&list))
                .min(MAX_WAIT_FOR_MS),
        ),
        _ => wait_for,
    }
}

fn invalid(msg: impl Into<String>) -> CrwError {
    CrwError::InvalidRequest(msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: Value) -> CrwResult<Vec<ScrapeAction>> {
        parse_actions(Some(&v), None)
    }

    fn err(v: Value) -> String {
        match parse(v) {
            Err(CrwError::InvalidRequest(m)) => m,
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn absent_null_and_empty_are_no_actions() {
        assert!(parse_actions(None, None).unwrap().is_empty());
        assert!(parse(Value::Null).unwrap().is_empty());
        assert!(parse(json!([])).unwrap().is_empty());
    }

    #[test]
    fn parses_the_firecrawl_shapes() {
        let got = parse(json!([
            {"type": "wait", "milliseconds": 500},
            {"type": "wait", "selector": "#list"},
            {"type": "click", "selector": "button.more", "all": true},
            {"type": "write", "text": "tv"},
            {"type": "press", "key": "Enter"},
            {"type": "scroll", "direction": "up", "selector": "main"},
            {"type": "scroll"}
        ]))
        .unwrap();
        assert_eq!(
            got,
            vec![
                ScrapeAction::Wait {
                    milliseconds: Some(500),
                    selector: None,
                    optional: false
                },
                ScrapeAction::Wait {
                    milliseconds: None,
                    selector: Some("#list".into()),
                    optional: false
                },
                ScrapeAction::Click {
                    selector: "button.more".into(),
                    all: true,
                    optional: false
                },
                ScrapeAction::Write { text: "tv".into() },
                ScrapeAction::Press {
                    key: "Enter".into()
                },
                ScrapeAction::Scroll {
                    direction: ScrollDirection::Up,
                    selector: Some("main".into()),
                    optional: false
                },
                ScrapeAction::Scroll {
                    direction: ScrollDirection::Down,
                    selector: None,
                    optional: false
                },
            ]
        );
    }

    #[test]
    fn optional_parses_on_lookup_steps() {
        let got = parse(json!([{"type": "click", "selector": "a", "optional": true}])).unwrap();
        assert!(got[0].is_optional());
    }

    #[test]
    fn rejects_output_types_with_a_pointer_to_formats() {
        for kind in OUTPUT_TYPES {
            let m = err(json!([{"type": kind}]));
            assert!(m.contains(&format!("'{kind}' is not supported")), "{m}");
            assert!(m.contains("formats"), "{m}");
        }
    }

    #[test]
    fn rejects_unknown_type_and_missing_fields() {
        assert!(err(json!([{"type": "hover", "selector": "a"}])).contains("actions[0]"));
        assert!(err(json!([{"type": "click"}])).contains("selector"));
        assert!(err(json!([{"selector": "a"}])).contains("actions[0]"));
        assert!(err(json!({"type": "wait"})).contains("must be an array"));
    }

    #[test]
    fn wait_takes_exactly_one_of_ms_or_selector() {
        assert!(err(json!([{"type": "wait"}])).contains("exactly one"));
        assert!(
            err(json!([{"type": "wait", "milliseconds": 5, "selector": "a"}]))
                .contains("exactly one")
        );
        assert!(err(json!([{"type": "wait", "milliseconds": 0}])).contains("greater than 0"));
    }

    #[test]
    fn caps_count_selector_text_and_key() {
        let many: Vec<Value> = (0..=MAX_ACTIONS)
            .map(|_| json!({"type": "scroll"}))
            .collect();
        assert!(err(Value::Array(many)).contains("at most 50"));
        assert!(err(json!([{"type": "click", "selector": "  "}])).contains("empty"));
        let long = "a".repeat(MAX_SELECTOR_LEN + 1);
        assert!(err(json!([{"type": "click", "selector": long}])).contains("longer"));
        let text = "x".repeat(MAX_WRITE_CHARS + 1);
        assert!(err(json!([{"type": "write", "text": text}])).contains("longer"));
        assert!(err(json!([{"type": "press", "key": ""}])).contains("key"));
        assert!(err(json!([{"type": "press", "key": "Control+a"}])).contains("combinations"));
        assert!(parse(json!([{"type": "press", "key": "+"}])).is_ok());
    }

    #[test]
    fn total_wait_includes_wait_for() {
        let list = json!([{"type": "wait", "milliseconds": 40_000}]);
        assert!(parse_actions(Some(&list), Some(20_000)).is_ok());
        let m = match parse_actions(Some(&list), Some(20_001)) {
            Err(CrwError::InvalidRequest(m)) => m,
            other => panic!("{other:?}"),
        };
        assert!(m.contains("60000"), "{m}");
    }

    #[test]
    fn budget_counts_waits_lookups_and_overhead_and_is_capped() {
        let list = parse(json!([
            {"type": "wait", "milliseconds": 1000},
            {"type": "wait", "selector": "a"},
            {"type": "click", "selector": "a"},
            {"type": "scroll"},
            {"type": "press", "key": "Tab"}
        ]))
        .unwrap();
        assert_eq!(
            actions_budget_ms(&list),
            1000 + SELECTOR_WAIT_MS + 2 * ELEMENT_LOOKUP_MS + 5 * STEP_OVERHEAD_MS
        );
        let many = vec![
            ScrapeAction::Wait {
                milliseconds: None,
                selector: Some("a".into()),
                optional: false
            };
            50
        ];
        assert_eq!(actions_budget_ms(&many), MAX_WAIT_FOR_MS);
    }

    #[test]
    fn deadline_wait_for_adds_the_budget_only_with_actions() {
        assert_eq!(deadline_wait_for(None, Some(3000)), Some(3000));
        assert_eq!(deadline_wait_for(Some(&json!([])), None), None);
        // An invalid list fails validation later; sizing stays unchanged.
        assert_eq!(
            deadline_wait_for(Some(&json!([{"type": "hover"}])), Some(1)),
            Some(1)
        );
        let one = json!([{"type": "wait", "milliseconds": 2000}]);
        assert_eq!(
            deadline_wait_for(Some(&one), None),
            Some(8_000 + 2000 + STEP_OVERHEAD_MS)
        );
        assert_eq!(
            deadline_wait_for(Some(&one), Some(1000)),
            Some(1000 + 2000 + STEP_OVERHEAD_MS)
        );
        let long = json!([{"type": "wait", "milliseconds": 59_000}]);
        assert_eq!(deadline_wait_for(Some(&long), None), Some(MAX_WAIT_FOR_MS));
    }

    #[test]
    fn describe_names_index_type_and_selector() {
        let a = ScrapeAction::Click {
            selector: "button.more".into(),
            all: false,
            optional: true,
        };
        assert_eq!(a.describe(2), "action 3 (click \"button.more\")");
        assert_eq!(
            ScrapeAction::Press {
                key: "Enter".into()
            }
            .describe(0),
            "action 1 (press)"
        );
    }
}
