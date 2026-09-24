//! Which waiting session the hotkey brings up next, and what the
//! notification says when one starts waiting.
//!
//! Pressing the hotkey again without answering moves on to the next one, so
//! a session you are not ready for does not block the rest.

use horadric_core::{Phase, WaitReason};

/// A session that has just started waiting, as the notification names it.
pub struct Waiting<'a> {
    pub name: &'a str,
    pub phase: &'a Phase,
    /// What it is waiting for: the tool, the question, the error.
    pub line: &'a str,
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
            let what = match one.phase {
                Phase::Waiting(WaitReason::Permission) => "needs permission",
                Phase::Waiting(WaitReason::Dialog) => "opened a dialog",
                Phase::Waiting(WaitReason::Error(_)) => "stopped with an error",
                _ => "has a question",
            };
            let text = if one.line.trim().is_empty() {
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
        }])
        .unwrap();
        assert_eq!(a.title, "fix-login needs permission");
        assert_eq!(a.text, "allow Bash?");
        let asked = Phase::Waiting(WaitReason::Input);
        let a = alert(&[Waiting {
            name: "docs",
            phase: &asked,
            line: " ",
        }])
        .unwrap();
        assert_eq!(a.title, "docs has a question");
        assert_eq!(a.text, "Click to answer.");
        assert_eq!(alert(&[]), None);
    }

    #[test]
    fn several_at_once_are_one_alert() {
        let perm = Phase::Waiting(WaitReason::Permission);
        let w = |name| Waiting {
            name,
            phase: &perm,
            line: "",
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
