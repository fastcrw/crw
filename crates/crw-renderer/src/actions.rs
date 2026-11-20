//! Runs a scrape's browser `actions` on a live CDP page.
//!
//! Called by the Chrome-family CDP tiers once the page has loaded, before the
//! HTML is read (`cdp.rs`). The techniques are the ones `crw-browse` settled on
//! for the same jobs: a click is `scrollIntoView` + `focus()` + `click()` on a
//! JS handle rather than synthesised mouse coordinates, an element lookup polls
//! `document.querySelector`, and a key press is an `Input.dispatchKeyEvent`
//! sequence. Only Chrome tiers reach this (the ladder filter in `lib.rs`), so
//! `Input.insertText` is safe for `write`.

use std::time::Duration;

use crw_core::Deadline;
use crw_core::actions::{
    ActionsOutcome, ELEMENT_LOOKUP_MS, SELECTOR_WAIT_MS, ScrapeAction, ScrollDirection,
};
use serde_json::{Value, json};

use crate::cdp_conn::CdpConnection;

const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Cap on one CDP round trip of a step.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// Kept back from the request deadline for what follows the list: the
/// post-action HTML read and a screenshot. Without it the last step would run
/// to the deadline and the tier's outer timeout would drop the whole render.
const READ_RESERVE: Duration = Duration::from_secs(3);
/// Non-zero when typing would land in a password input: 1 for one, 2 when
/// focus sits in a frame the page cannot see into (cross-origin), which is
/// refused too because it cannot be checked. Follows focus through open
/// shadow roots and same-origin frames. A closed shadow root cannot be
/// inspected from the page and is a known gap.
const FOCUSED_PASSWORD_JS: &str = "(() => { let el = document.activeElement; \
    for (let i = 0; i < 20 && el; i++) { \
      if (el.shadowRoot && el.shadowRoot.activeElement) { el = el.shadowRoot.activeElement; continue; } \
      if (el.tagName === 'IFRAME' || el.tagName === 'FRAME') { \
        let d = null; try { d = el.contentDocument; } catch (e) {} \
        if (!d) return 2; el = d.activeElement; continue; } \
      break; } \
    return el && el.tagName === 'INPUT' && (el.type || '').toLowerCase() === 'password' ? 1 : 0; })()";

/// The instant the list must be done by.
#[derive(Clone, Copy)]
struct Budget(std::time::Instant);

impl Budget {
    fn left(self) -> Duration {
        self.0.saturating_duration_since(std::time::Instant::now())
    }
}

/// Why a step stopped.
enum StepError {
    /// The element never appeared within the step's window.
    Missing,
    /// The page was between documents (a navigation was committing).
    Navigating,
    /// The page or the protocol failed the step.
    Failed(String),
}

/// Run `actions` in order on the page behind `session_id`.
///
/// Never returns an error: a step that cannot be carried out is reported in
/// [`ActionsOutcome::failed`] and stops the list, so the caller still holds a
/// live page and decides what the failure means for the scrape.
pub async fn run(
    conn: &CdpConnection,
    session_id: &str,
    actions: &[ScrapeAction],
    deadline: Deadline,
) -> ActionsOutcome {
    let mut outcome = ActionsOutcome::default();
    // The earlier of: the request deadline less the read reserve, and the
    // list's own budget from now. The CDP tier sized its outer timeout with
    // that same budget (`cdp.rs` `fetch`), so stopping here leaves the tier
    // time to read the page instead of being cancelled mid-list.
    let by_deadline = deadline
        .absolute()
        .checked_sub(READ_RESERVE)
        .unwrap_or_else(std::time::Instant::now);
    let by_budget = std::time::Instant::now()
        + Duration::from_millis(crw_core::actions::actions_budget_ms(actions));
    let end = Budget(by_deadline.min(by_budget));
    for (i, action) in actions.iter().enumerate() {
        if end.left().is_zero() {
            outcome.failed = Some(format!(
                "{} did not run: the request deadline ran out; pass a larger timeout",
                action.describe(i)
            ));
            return outcome;
        }
        match step(conn, session_id, action, end).await {
            Ok(()) => {}
            Err(StepError::Missing) if action.is_optional() => outcome.skipped.push(format!(
                "{} skipped: no element matched",
                action.describe(i)
            )),
            Err(StepError::Missing) => {
                outcome.failed = Some(format!(
                    "{}: no element matched the selector",
                    action.describe(i)
                ));
                return outcome;
            }
            Err(StepError::Failed(why)) => {
                outcome.failed = Some(format!("{}: {why}", action.describe(i)));
                return outcome;
            }
            Err(StepError::Navigating) => {
                outcome.failed = Some(format!(
                    "{}: the page was still navigating; add a wait before this step",
                    action.describe(i)
                ));
                return outcome;
            }
        }
    }
    outcome
}

async fn step(
    conn: &CdpConnection,
    session_id: &str,
    action: &ScrapeAction,
    end: Budget,
) -> Result<(), StepError> {
    match action {
        ScrapeAction::Wait {
            milliseconds: Some(ms),
            ..
        } => {
            let want = Duration::from_millis(*ms);
            let left = end.left();
            tokio::time::sleep(want.min(left)).await;
            // A wait cut short would hand back the page before it settled, the
            // silent partial result this feature exists to remove.
            if want > left {
                return Err(StepError::Failed(
                    "the wait ran past the request deadline; pass a larger timeout".into(),
                ));
            }
            Ok(())
        }
        ScrapeAction::Wait {
            selector: Some(sel),
            ..
        } => {
            let expr = format!("document.querySelector({}) ? 1 : 0", js_str(sel));
            poll(conn, session_id, &expr, SELECTOR_WAIT_MS, end).await
        }
        ScrapeAction::Wait { .. } => Ok(()),
        ScrapeAction::Click { selector, all, .. } => {
            let expr = click_expression(selector, *all);
            poll(conn, session_id, &expr, ELEMENT_LOOKUP_MS, end).await
        }
        ScrapeAction::Scroll {
            direction,
            selector,
            ..
        } => {
            // The page scroll answers 1 on the first evaluate; the window only
            // matters for an element to find or a navigation to wait out.
            let expr = scroll_expression(*direction, selector.as_deref());
            poll(conn, session_id, &expr, ELEMENT_LOOKUP_MS, end).await
        }
        ScrapeAction::Write { text } => {
            // Never into a password field: scraping does not log in, and the
            // box's egress IP is shared by every customer's scrapes, so it must
            // not become a credential-stuffing source.
            if focused_password(conn, session_id).await? {
                return Err(StepError::Failed(
                    "writing into a password field (or a frame that cannot be checked) is not \
                     allowed"
                        .into(),
                ));
            }
            call(
                conn,
                session_id,
                "Input.insertText",
                json!({ "text": text }),
            )
            .await?;
            Ok(())
        }
        ScrapeAction::Press { key } => {
            // The same rule for typing a password one character at a time.
            if named_key(key).is_none()
                && key.chars().count() == 1
                && focused_password(conn, session_id).await?
            {
                return Err(StepError::Failed(
                    "typing into a password field (or a frame that cannot be checked) is not \
                     allowed"
                        .into(),
                ));
            }
            for params in key_events(key) {
                call(conn, session_id, "Input.dispatchKeyEvent", params).await?;
            }
            Ok(())
        }
    }
}

/// Whether typing now would land in a password field. Fails closed: a page
/// still between documents is asked once more after a beat, and if it is
/// still navigating the step fails rather than typing unchecked.
async fn focused_password(conn: &CdpConnection, session_id: &str) -> Result<bool, StepError> {
    for _ in 0..2 {
        match evaluate(conn, session_id, FOCUSED_PASSWORD_JS).await {
            Err(StepError::Navigating) => tokio::time::sleep(Duration::from_millis(500)).await,
            other => return other,
        }
    }
    Err(StepError::Navigating)
}

/// Evaluate `expr` until it returns a non-zero number or `window_ms` passes.
/// `expr` returns 0 for "element not there yet" and does its work in the same
/// round trip once the element is there, so the action and the lookup cannot
/// race. A `window_ms` of 0 evaluates once.
async fn poll(
    conn: &CdpConnection,
    session_id: &str,
    expr: &str,
    window_ms: u64,
    end: Budget,
) -> Result<(), StepError> {
    let until = tokio::time::Instant::now() + Duration::from_millis(window_ms).min(end.left());
    loop {
        // A navigation committing under the evaluate (a previous click, or a
        // form submit) means this step has not acted yet: clicks are deferred
        // by `click_expression`, so look again on the new document.
        let (found, navigating) = match evaluate(conn, session_id, expr).await {
            Ok(found) => (found, false),
            Err(StepError::Navigating) => (false, true),
            Err(e) => return Err(e),
        };
        if found {
            return Ok(());
        }
        if tokio::time::Instant::now() + POLL_INTERVAL > until {
            return Err(if navigating {
                StepError::Navigating
            } else {
                StepError::Missing
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `Runtime.evaluate` returning whether the expression yielded a non-zero
/// number. A thrown exception (an invalid selector, a page handler throwing)
/// is a failed step, not a missing element.
async fn evaluate(conn: &CdpConnection, session_id: &str, expr: &str) -> Result<bool, StepError> {
    let res = match call(
        conn,
        session_id,
        "Runtime.evaluate",
        json!({ "expression": expr, "returnByValue": true, "awaitPromise": false }),
    )
    .await
    {
        Ok(res) => res,
        // A navigation is committing and the old document is gone. What that
        // means depends on the step (see `poll` and `focused_password`).
        Err(StepError::Failed(m)) if is_navigation_gap(&m) => return Err(StepError::Navigating),
        Err(e) => return Err(e),
    };
    if let Some(details) = res.get("exceptionDetails") {
        let msg = details
            .pointer("/exception/description")
            .and_then(Value::as_str)
            .or_else(|| details.get("text").and_then(Value::as_str))
            .unwrap_or("the page threw");
        // First line only: a stack trace is page internals, not an answer.
        let first = msg.lines().next().unwrap_or(msg);
        return Err(StepError::Failed(first.chars().take(300).collect()));
    }
    Ok(res
        .pointer("/result/value")
        .and_then(Value::as_f64)
        .is_some_and(|n| n != 0.0))
}

/// The protocol errors Chrome gives while a page is between documents.
fn is_navigation_gap(msg: &str) -> bool {
    msg.contains("Execution context was destroyed")
        || msg.contains("Cannot find context with specified id")
        || msg.contains("Inspected target navigated or closed")
}

async fn call(
    conn: &CdpConnection,
    session_id: &str,
    method: &str,
    params: Value,
) -> Result<Value, StepError> {
    conn.send_recv(method, params, Some(session_id), CALL_TIMEOUT)
        .await
        .map_err(|e| StepError::Failed(format!("{method} failed: {e}")))
}

/// A JS string literal for `s` (JSON is a subset of JS string syntax).
fn js_str(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

/// Scrolls each match into view and focuses it now, and clicks it on the next
/// task. Deferring the click is what makes a navigation gap unambiguous: the
/// evaluate always answers before any navigation the click starts, so a
/// document that vanished under it means the click has not run and polling
/// again is safe. Focus stays synchronous so a following `write` finds it.
fn click_expression(selector: &str, all: bool) -> String {
    let sel = js_str(selector);
    let pick = if all {
        format!("Array.from(document.querySelectorAll({sel}))")
    } else {
        format!("[document.querySelector({sel})].filter(Boolean)")
    };
    format!(
        "(() => {{ const els = {pick}; \
         for (const el of els) {{ \
           try {{ el.scrollIntoView({{block: 'center'}}); }} catch (e) {{}} \
           try {{ el.focus({{preventScroll: true}}); }} catch (e) {{}} \
         }} \
         if (els.length) setTimeout(() => {{ for (const el of els) el.click(); }}, 0); \
         return els.length; }})()"
    )
}

fn scroll_expression(direction: ScrollDirection, selector: Option<&str>) -> String {
    let sign = match direction {
        ScrollDirection::Down => "",
        ScrollDirection::Up => "-",
    };
    match selector {
        None => format!("(() => {{ window.scrollBy(0, {sign}window.innerHeight); return 1; }})()"),
        Some(sel) => format!(
            "(() => {{ const el = document.querySelector({}); if (!el) return 0; \
             el.scrollBy(0, {sign}(el.clientHeight || window.innerHeight)); return 1; }})()",
            js_str(sel)
        ),
    }
}

/// `(key, code, windowsVirtualKeyCode)` for the named keys a scrape needs.
fn named_key(key: &str) -> Option<(&'static str, &'static str, u32)> {
    Some(match key {
        "Enter" => ("Enter", "Enter", 13),
        "Tab" => ("Tab", "Tab", 9),
        "Escape" | "Esc" => ("Escape", "Escape", 27),
        "Backspace" => ("Backspace", "Backspace", 8),
        "Delete" => ("Delete", "Delete", 46),
        "Space" | " " => (" ", "Space", 32),
        "ArrowUp" => ("ArrowUp", "ArrowUp", 38),
        "ArrowDown" => ("ArrowDown", "ArrowDown", 40),
        "ArrowLeft" => ("ArrowLeft", "ArrowLeft", 37),
        "ArrowRight" => ("ArrowRight", "ArrowRight", 39),
        "PageUp" => ("PageUp", "PageUp", 33),
        "PageDown" => ("PageDown", "PageDown", 34),
        "Home" => ("Home", "Home", 36),
        "End" => ("End", "End", 35),
        _ => return None,
    })
}

/// The `Input.dispatchKeyEvent` params for one press of `key`.
///
/// A named key is keyDown + keyUp; Enter carries `text: "\r"` on keyDown so
/// the page also sees `keypress` (what form submit handlers listen for). Space
/// carries its text so it types and activates. Any other single character is
/// keyDown + char + keyUp with the text only on `char`: Chromium treats text on
/// keyDown as input and would insert it twice (see `crw-browse` `type_text`).
/// An unknown multi-character name is sent as a bare key name, which page
/// listeners reading `event.key` still see.
fn key_events(key: &str) -> Vec<Value> {
    if let Some((k, code, vk)) = named_key(key) {
        let mut down = json!({
            "type": "keyDown", "key": k, "code": code,
            "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk,
        });
        let text = match k {
            "Enter" => Some("\r"),
            " " => Some(" "),
            _ => None,
        };
        if let Some(t) = text {
            down["text"] = json!(t);
            down["unmodifiedText"] = json!(t);
        }
        let up = json!({
            "type": "keyUp", "key": k, "code": code,
            "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk,
        });
        return vec![down, up];
    }
    let mut chars = key.chars();
    if let (Some(_), None) = (chars.next(), chars.next()) {
        return vec![
            json!({ "type": "keyDown", "key": key }),
            json!({ "type": "char", "text": key, "unmodifiedText": key }),
            json!({ "type": "keyUp", "key": key }),
        ];
    }
    vec![
        json!({ "type": "keyDown", "key": key }),
        json!({ "type": "keyUp", "key": key }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_str_escapes_quotes_and_backslashes() {
        assert_eq!(js_str(r#"a[href="x"]\"#), r#""a[href=\"x\"]\\""#);
    }

    #[test]
    fn click_expression_picks_first_or_all() {
        let one = click_expression("button.more", false);
        assert!(one.contains(r#"[document.querySelector("button.more")].filter(Boolean)"#));
        assert!(one.contains("el.click()") && one.contains("return els.length"));
        let all = click_expression("a", true);
        assert!(all.contains(r#"document.querySelectorAll("a")"#));
    }

    #[test]
    fn scroll_expression_signs_and_targets() {
        assert!(scroll_expression(ScrollDirection::Down, None).contains("scrollBy(0, window"));
        assert!(scroll_expression(ScrollDirection::Up, None).contains("scrollBy(0, -window"));
        let el = scroll_expression(ScrollDirection::Down, Some("#feed"));
        assert!(el.contains(r##"querySelector("#feed")"##) && el.contains("return 0"));
    }

    #[test]
    fn enter_fires_keypress_text_on_keydown() {
        let ev = key_events("Enter");
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0]["type"], "keyDown");
        assert_eq!(ev[0]["text"], "\r");
        assert_eq!(ev[0]["windowsVirtualKeyCode"], 13);
        assert_eq!(ev[1]["type"], "keyUp");
        assert!(ev[1].get("text").is_none());
    }

    #[test]
    fn arrow_keys_carry_no_text() {
        let ev = key_events("ArrowDown");
        assert_eq!(ev[0]["key"], "ArrowDown");
        assert_eq!(ev[0]["windowsVirtualKeyCode"], 40);
        assert!(ev[0].get("text").is_none());
    }

    #[test]
    fn single_char_puts_text_only_on_char() {
        let ev = key_events("a");
        assert_eq!(ev.len(), 3);
        assert!(ev[0].get("text").is_none());
        assert_eq!(ev[1]["type"], "char");
        assert_eq!(ev[1]["text"], "a");
        assert!(ev[2].get("text").is_none());
    }

    #[test]
    fn unknown_name_is_a_bare_key() {
        let ev = key_events("F5");
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0]["key"], "F5");
    }
}
