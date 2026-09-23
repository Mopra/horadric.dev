//! A single agent session and the state machine that drives it.

use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::event::HookEvent;

/// Why a session is waiting on the human.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaitReason {
    /// A tool call needs approval.
    Permission,
    /// Claude asked a question or the prompt has sat idle.
    Input,
    /// An MCP server or the CLI opened a dialog.
    Dialog,
    /// The turn failed and will not continue on its own.
    Error(String),
}

/// The three states a tile shows, plus the two bookends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Session started, no prompt sent yet.
    Idle,
    /// Claude is doing something.
    Working,
    /// Claude is blocked on the human. This is the one that lights up.
    Waiting(WaitReason),
    /// The turn finished. The human has not looked yet.
    Done,
    /// The CLI exited.
    Ended,
}

impl Phase {
    /// Short label for a tile or a table cell.
    pub fn label(&self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Working => "working",
            Phase::Waiting(WaitReason::Permission) => "permission",
            Phase::Waiting(WaitReason::Input) => "question",
            Phase::Waiting(WaitReason::Dialog) => "dialog",
            Phase::Waiting(WaitReason::Error(_)) => "error",
            Phase::Done => "done",
            Phase::Ended => "ended",
        }
    }

    pub fn is_waiting(&self) -> bool {
        matches!(self, Phase::Waiting(_))
    }
}

/// One running (or finished) agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Glance's id. Set by Glance when it spawns the CLI and carried back in
    /// every hook via the environment.
    pub id: String,
    /// A name a human can recognise at tile size.
    pub name: String,
    /// Claude's own session id, once the first hook arrives.
    pub claude_session_id: Option<String>,
    pub cwd: String,
    pub phase: Phase,
    /// When the current phase began. The tile's age line counts from here.
    pub since: SystemTime,
    /// The last thing worth showing: a tool name, a question, a final message.
    pub last_line: String,
    pub created: SystemTime,
}

impl Session {
    pub fn new(id: impl Into<String>, name: impl Into<String>, cwd: impl Into<String>) -> Self {
        let now = SystemTime::now();
        Session {
            id: id.into(),
            name: name.into(),
            claude_session_id: None,
            cwd: cwd.into(),
            phase: Phase::Idle,
            since: now,
            last_line: String::new(),
            created: now,
        }
    }

    /// How long the session has been in its current phase.
    pub fn age(&self) -> Duration {
        self.since.elapsed().unwrap_or_default()
    }

    /// Applies a hook event. Returns true when the phase changed.
    pub fn apply(&mut self, event: &HookEvent, now: SystemTime) -> bool {
        if self.claude_session_id.is_none() {
            self.claude_session_id = Some(event.session_id.clone());
        }
        if !event.cwd.is_empty() {
            self.cwd = event.cwd.clone();
        }
        // Subagents run inside a turn that is already Working. Their events
        // carry no new information about whether the human is needed.
        if event.is_subagent() {
            return false;
        }

        let next = match event.hook_event_name.as_str() {
            "SessionStart" => match event.source.as_deref() {
                // Compaction happens mid turn. Nothing changed for the human.
                Some("compact") => None,
                _ => Some(Phase::Idle),
            },
            "UserPromptSubmit" => {
                if let Some(p) = &event.user_prompt {
                    self.last_line = first_line(p);
                }
                Some(Phase::Working)
            }
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => {
                if let Some(t) = &event.tool_name {
                    self.last_line = t.clone();
                }
                Some(Phase::Working)
            }
            "PermissionRequest" => {
                if let Some(t) = &event.tool_name {
                    self.last_line = format!("allow {t}?");
                }
                Some(Phase::Waiting(WaitReason::Permission))
            }
            "Notification" => self.apply_notification(event),
            "Stop" => {
                if let Some(m) = &event.last_assistant_message {
                    self.last_line = first_line(m);
                }
                Some(Phase::Done)
            }
            "StopFailure" => {
                let kind = event.error_type.clone().unwrap_or_else(|| "unknown".into());
                if let Some(m) = &event.error_message {
                    self.last_line = first_line(m);
                }
                Some(Phase::Waiting(WaitReason::Error(kind)))
            }
            "SessionEnd" => Some(Phase::Ended),
            _ => None,
        };

        match next {
            Some(phase) if phase != self.phase => {
                self.phase = phase;
                self.since = now;
                true
            }
            _ => false,
        }
    }

    fn apply_notification(&mut self, event: &HookEvent) -> Option<Phase> {
        let kind = event.notification_type.as_deref().unwrap_or("");
        let next = match kind {
            "permission_prompt" => Phase::Waiting(WaitReason::Permission),
            // Idle after a finished turn is just "done, unread". Idle while
            // Claude was supposedly working means it asked and is waiting.
            "idle_prompt" | "agent_needs_input" => match self.phase {
                Phase::Working => Phase::Waiting(WaitReason::Input),
                _ => return None,
            },
            "elicitation_dialog" | "elicitation_url_dialog" => Phase::Waiting(WaitReason::Dialog),
            "elicitation_complete" | "elicitation_response" => Phase::Working,
            _ => return None,
        };
        if let Some(m) = &event.message {
            self.last_line = first_line(m);
        }
        Some(next)
    }
}

/// "40 s", "12 min", "2 h 05 min". No seconds past a minute: nobody reads them.
pub fn format_age(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else {
        format!("{} h {:02} min", s / 3600, (s % 3600) / 60)
    }
}

/// A session id that is unique enough and readable in a table:
/// `<base>-<seconds into the UTC day>`. Callers that can see the registry add
/// a suffix on the rare collision.
pub fn session_id(base: &str, now: SystemTime) -> String {
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() % 86_400)
        .unwrap_or(0);
    format!("{base}-{secs}")
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(name: &str) -> HookEvent {
        HookEvent {
            session_id: "c1".into(),
            cwd: "C:/repo".into(),
            ..HookEvent::synthetic(name)
        }
    }

    fn now() -> SystemTime {
        SystemTime::now()
    }

    #[test]
    fn happy_path_turn() {
        let mut s = Session::new("g1", "fix-login", "C:/repo");
        assert!(!s.apply(&ev("SessionStart"), now()));
        assert!(s.apply(&ev("UserPromptSubmit"), now()));
        assert_eq!(s.phase, Phase::Working);
        let mut t = ev("PreToolUse");
        t.tool_name = Some("Bash".into());
        assert!(!s.apply(&t, now()));
        assert_eq!(s.last_line, "Bash");
        let mut stop = ev("Stop");
        stop.last_assistant_message = Some("All green.\nDetails...".into());
        assert!(s.apply(&stop, now()));
        assert_eq!(s.phase, Phase::Done);
        assert_eq!(s.last_line, "All green.");
    }

    #[test]
    fn permission_lights_up_and_clears() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("UserPromptSubmit"), now());
        let mut p = ev("PermissionRequest");
        p.tool_name = Some("Edit".into());
        assert!(s.apply(&p, now()));
        assert_eq!(s.phase, Phase::Waiting(WaitReason::Permission));
        assert_eq!(s.last_line, "allow Edit?");
        assert!(s.apply(&ev("PostToolUse"), now()));
        assert_eq!(s.phase, Phase::Working);
    }

    #[test]
    fn idle_after_done_stays_done() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("UserPromptSubmit"), now());
        s.apply(&ev("Stop"), now());
        let mut n = ev("Notification");
        n.notification_type = Some("idle_prompt".into());
        assert!(!s.apply(&n, now()));
        assert_eq!(s.phase, Phase::Done);
    }

    #[test]
    fn idle_while_working_means_question() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("UserPromptSubmit"), now());
        let mut n = ev("Notification");
        n.notification_type = Some("idle_prompt".into());
        n.message = Some("Claude is waiting for your input".into());
        assert!(s.apply(&n, now()));
        assert_eq!(s.phase, Phase::Waiting(WaitReason::Input));
    }

    #[test]
    fn subagent_events_are_ignored() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("UserPromptSubmit"), now());
        let mut stop = ev("Stop");
        stop.agent_id = Some("sub-1".into());
        assert!(!s.apply(&stop, now()));
        assert_eq!(s.phase, Phase::Working);
    }

    #[test]
    fn compaction_does_not_reset() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("UserPromptSubmit"), now());
        let mut c = ev("SessionStart");
        c.source = Some("compact".into());
        assert!(!s.apply(&c, now()));
        assert_eq!(s.phase, Phase::Working);
    }

    #[test]
    fn failure_waits_with_reason() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("UserPromptSubmit"), now());
        let mut f = ev("StopFailure");
        f.error_type = Some("rate_limit".into());
        assert!(s.apply(&f, now()));
        assert_eq!(
            s.phase,
            Phase::Waiting(WaitReason::Error("rate_limit".into()))
        );
    }

    #[test]
    fn ages_read_like_a_human_wrote_them() {
        assert_eq!(format_age(Duration::from_secs(40)), "40 s");
        assert_eq!(format_age(Duration::from_secs(12 * 60 + 30)), "12 min");
        assert_eq!(
            format_age(Duration::from_secs(2 * 3600 + 5 * 60)),
            "2 h 05 min"
        );
    }

    #[test]
    fn session_ids_count_seconds_into_the_day() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(3 * 86_400 + 75);
        assert_eq!(session_id("fix-login", t), "fix-login-75");
    }

    #[test]
    fn parses_real_payload_shape() {
        let body = br#"{"session_id":"abc","transcript_path":"x","cwd":"C:/r","hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Bash","permission_mode":"default","unknown_field":1}"#;
        let e = HookEvent::from_json(body).unwrap();
        assert_eq!(e.notification_type.as_deref(), Some("permission_prompt"));
    }
}
