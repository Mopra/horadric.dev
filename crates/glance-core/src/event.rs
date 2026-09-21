//! The subset of a Claude Code hook payload that Glance cares about.
//!
//! Claude Code posts the full payload as JSON. We keep the fields that drive
//! state and drop the rest, so a new field upstream never breaks parsing.

use serde::Deserialize;

/// A hook event as received from Claude Code.
#[derive(Debug, Clone, Deserialize)]
pub struct HookEvent {
    /// Claude's own id for the session. Stable across the session's life.
    pub session_id: String,
    /// Which hook fired, for example `Stop` or `Notification`.
    pub hook_event_name: String,
    /// Working directory of the session when the hook fired.
    #[serde(default)]
    pub cwd: String,
    /// Present when the hook fired inside a subagent. Subagent activity
    /// never changes the phase of the parent session.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// `SessionStart` only: `startup`, `resume`, `clear` or `compact`.
    #[serde(default)]
    pub source: Option<String>,
    /// `Notification` only: `permission_prompt`, `idle_prompt` and friends.
    #[serde(default)]
    pub notification_type: Option<String>,
    /// `Notification` only: human readable text.
    #[serde(default)]
    pub message: Option<String>,
    /// Tool events only.
    #[serde(default)]
    pub tool_name: Option<String>,
    /// `Stop` only: the final assistant text of the turn.
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    /// `UserPromptSubmit` only.
    #[serde(default)]
    pub user_prompt: Option<String>,
    /// `StopFailure` only.
    #[serde(default)]
    pub error_type: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
}

impl HookEvent {
    /// Parses a raw hook payload. Unknown fields are ignored.
    pub fn from_json(body: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(body)
    }

    /// True when the event came from a subagent rather than the main loop.
    pub fn is_subagent(&self) -> bool {
        self.agent_id.as_deref().is_some_and(|id| !id.is_empty())
    }
}
