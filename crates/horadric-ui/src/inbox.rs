//! Which waiting session the hotkey brings up next, and what the
//! notification says when one starts waiting.
//!
//! Pressing the hotkey again without answering moves on to the next one, so
//! a session you are not ready for does not block the rest.

use horadric_core::ask::{Ask, Kind};
use horadric_core::{Phase, WaitReason};

/// A session that has just started waiting, as the notification names it.
pub struct Waiting<'a> {
    pub name: &'a str,
    pub phase: &'a Phase,
    /// What it is waiting for: the tool, the question, the error.
    pub line: &'a str,
    /// What it asks, when the agent said: a question or a plan is not a
    /// permission, and the tool and its command say more than the line.
    pub ask: Option<&'a Ask>,
}

/// A notification's title and text.
#[derive(Debug, PartialEq, Eq)]
pub struct Alert {
    pub title: String,
    pub text: String,
}

/// What to say about sessions that have just started waiting. Several at
/// once are one notification, so a burst does not stack up.
pub fn alert(new: &[Waiting]) -> Option<Alert> {
    match new {
        [] => None,
        [one] => {
            let asked = one.ask.filter(|_| one.phase.asks());
            let tool = asked.and_then(|a| a.tool.as_deref());
            let what = match (one.phase, asked.map(|a| &a.kind), tool) {
                (_, Some(&Kind::Question), _) => "has a question".to_string(),
                (_, _, Some("ExitPlanMode")) => "has a plan for you".to_string(),
                (Phase::Waiting(WaitReason::Permission), _, Some(t)) => {
                    format!("needs permission for {}", short_tool(t))
                }
                (Phase::Waiting(WaitReason::Permission), _, _) => "needs permission".to_string(),
                (Phase::Waiting(WaitReason::Dialog), _, _) => "opened a dialog".to_string(),
                (Phase::Waiting(WaitReason::Error(_)), _, _) => "stopped with an error".to_string(),
                _ => "has a question".to_string(),
            };
            let detail = asked.map_or("", |a| a.detail.trim());
            let text = if !detail.is_empty() {
                detail.to_string()
            } else if one.line.trim().is_empty() {
                "Click to answer.".to_string()
            } else {
                one.line.trim().to_string()
            };
            Some(Alert {
                title: format!("{} {what}", one.name),
                text,
            })
        }
        many => Some(Alert {
            title: format!("{} sessions need you", many.len()),
            text: many.iter().map(|w| w.name).collect::<Vec<_>>().join(", "),
        }),
    }
}

/// An MCP tool by its own name, which is what the human recognises.
fn short_tool(tool: &str) -> &str {
    tool.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .map_or(tool, |(_, name)| name)
}

/// The session after `current` in `waiting`, oldest wait first, wrapping
/// round. The oldest when `current` is not waiting, or nothing is shown.
pub fn next<'a>(waiting: &'a [String], current: Option<&str>) -> Option<&'a str> {
    let at = current.and_then(|c| waiting.iter().position(|w| w == c));
    let i = at.map_or(0, |i| (i + 1) % waiting.len());
    waiting.get(i).map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn one_alert_says_what_it_waits_for() {
        let perm = Phase::Waiting(WaitReason::Permission);
        let a = alert(&[Waiting {
            name: "fix-login",
            phase: &perm,
            line: "allow Bash?",
            ask: None,
        }])
        .unwrap();
        assert_eq!(a.title, "fix-login needs permission");
        assert_eq!(a.text, "allow Bash?");
        let asked = Phase::Waiting(WaitReason::Input);
        let a = alert(&[Waiting {
            name: "docs",
            phase: &asked,
            line: " ",
            ask: None,
        }])
        .unwrap();
        assert_eq!(a.title, "docs has a question");
        assert_eq!(a.text, "Click to answer.");
        assert_eq!(alert(&[]), None);
    }

    #[test]
    fn an_ask_says_its_tool_or_that_it_is_a_question() {
        let perm = Phase::Waiting(WaitReason::Permission);
        let now = std::time::SystemTime::UNIX_EPOCH;
        let say = |ask: &Ask| {
            alert(&[Waiting {
                name: "s",
                phase: &perm,
                line: "waiting on x",
                ask: Some(ask),
            }])
            .unwrap()
        };
        let bash = Ask::permission(
            "Bash",
            Some(&serde_json::json!({"command": "ls"})),
            false,
            now,
        );
        let a = say(&bash);
        assert_eq!(a.title, "s needs permission for Bash");
        assert_eq!(a.text, bash.detail);
        let input = serde_json::json!({"questions": [{"question": "Tabs?", "options": []}]});
        let q = Ask::permission("AskUserQuestion", Some(&input), false, now);
        let a = say(&q);
        assert_eq!(
            (a.title.as_str(), a.text.as_str()),
            ("s has a question", "Tabs?")
        );
        let plan = Ask::permission(
            "ExitPlanMode",
            Some(&serde_json::json!({"plan": "x"})),
            false,
            now,
        );
        assert_eq!(say(&plan).title, "s has a plan for you");
    }

    #[test]
    fn several_at_once_are_one_alert() {
        let perm = Phase::Waiting(WaitReason::Permission);
        let w = |name| Waiting {
            name,
            phase: &perm,
            line: "",
            ask: None,
        };
        let a = alert(&[w("a"), w("b"), w("c")]).unwrap();
        assert_eq!(a.title, "3 sessions need you");
        assert_eq!(a.text, "a, b, c");
    }

    #[test]
    fn nothing_waiting_is_nothing() {
        assert_eq!(next(&[], Some("a")), None);
        assert_eq!(next(&[], None), None);
    }

    #[test]
    fn starts_with_the_oldest() {
        let w = ids(&["old", "new"]);
        assert_eq!(next(&w, None), Some("old"));
        assert_eq!(next(&w, Some("working-one")), Some("old"));
    }

    #[test]
    fn moves_on_and_wraps() {
        let w = ids(&["a", "b", "c"]);
        assert_eq!(next(&w, Some("a")), Some("b"));
        assert_eq!(next(&w, Some("c")), Some("a"));
        assert_eq!(next(&ids(&["a"]), Some("a")), Some("a"));
    }
}
