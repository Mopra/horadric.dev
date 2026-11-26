//! The subset of a Claude Code hook payload that Horadric cares about.
//!
//! Claude Code posts the full payload as JSON. We keep the fields that drive
//! state and drop the rest, so a new field upstream never breaks parsing.

use serde::Deserialize;
use serde_json::Value;

use crate::title::Title;
use crate::usage::Status;

/// A hook event as received from Claude Code.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HookEvent {
    /// Claude's own id for the session. Stable across the session's life.
    pub session_id: String,
    /// Which hook fired, for example `Stop` or `Notification`.
    pub hook_event_name: String,
    /// Working directory of the session when the hook fired.
    #[serde(default)]
    pub cwd: String,
    /// The conversation's JSONL file. Where Claude Code keeps its title.
    #[serde(default)]
    pub transcript_path: String,
    /// Present when the hook fired inside a subagent. Subagent activity
    /// never changes the phase of the parent session.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// `SessionStart` only: `startup`, `resume`, `clear` or `compact`.
    #[serde(default)]
    pub source: Option<String>,
    /// `SessionEnd` only: `clear`, `resume`, `logout`, `prompt_input_exit`
    /// and others.
    #[serde(default)]
    pub reason: Option<String>,
    /// `Notification` only: `permission_prompt`, `idle_prompt` and friends.
    #[serde(default)]
    pub notification_type: Option<String>,
    /// `Notification` only: human readable text.
    #[serde(default)]
    pub message: Option<String>,
    /// Tool events only.
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Tool events only: what the tool was given, such as a shell's
    /// `command`. Kept whole, since each tool gives its own shape.
    #[serde(default)]
    pub tool_input: Option<Value>,
    /// Tool events only: Claude Code's id for the call, which the
    /// `<task-notification>` of background work quotes back.
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// `PostToolUse` only: what the tool gave back. A shell command that
    /// ran past its timeout carries a `backgroundTaskId` here.
    #[serde(default)]
    pub tool_response: Option<Value>,
    /// `PermissionRequest` only: the rules Claude Code offers to add so it
    /// stops asking. Present means the prompt has an "always" choice.
    #[serde(default)]
    pub permission_suggestions: Option<Value>,
    /// `Stop` only: the final assistant text of the turn.
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    /// `UserPromptSubmit` only.
    #[serde(default)]
    pub user_prompt: Option<String>,
    /// `StopFailure` as Horadric makes one up itself (a Folio's stream):
    /// the kind and the words. Claude Code's own says `error` and
    /// `error_details`, see [`HookEvent::failure`].
    #[serde(default)]
    pub error_type: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
    /// `StopFailure`: the kind, such as `rate_limit` or
    /// `authentication_failed`. `PostToolUseFailure` has one too, the
    /// tool's own words. Kept whole, so a shape not seen yet cannot cost
    /// the event.
    #[serde(default)]
    pub error: Option<Value>,
    /// `StopFailure` only: what the API said, "429 Too Many Requests".
    #[serde(default)]
    pub error_details: Option<String>,
    /// The prompt the event belongs to. Absent before the first one.
    #[serde(default)]
    pub prompt_id: Option<String>,
    /// Events inside a turn: `{"level": "high"}`, the effort it runs at.
    /// Read with [`HookEvent::effort_level`].
    #[serde(default)]
    pub effort: Option<Value>,
    /// `PreCompact` and `PostCompact`: `manual` or `auto`.
    #[serde(default)]
    pub trigger: Option<String>,
    /// `TaskCreated` and `TaskCompleted`: the task in the session's list.
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub task_subject: Option<String>,
    /// `PostModelSwitch` only: the model's id before and after.
    #[serde(default)]
    pub from_model: Option<String>,
    #[serde(default)]
    pub to_model: Option<String>,
    /// Horadric's own `Register` event only: the name the user gave the
    /// session. Not part of any Claude Code payload.
    #[serde(default)]
    pub name: Option<String>,
    /// [`HookEvent::SPAWN`] and `SubagentStart`: the subagent's type, such
    /// as `Explore`.
    #[serde(default)]
    pub agent_type: Option<String>,
    /// [`HookEvent::SPAWN`] only: what the subagent was asked to do.
    #[serde(default)]
    pub description: Option<String>,
    /// [`HookEvent::SPAWN`] only: the model it runs on, when not the
    /// session's.
    #[serde(default)]
    pub model: Option<String>,
    /// [`HookEvent::SPAWN`] only: the subagent that sent it, none when the
    /// main loop did.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// `SubagentStop` only: the subagent's own transcript.
    #[serde(default)]
    pub agent_transcript_path: Option<String>,
    /// [`HookEvent::AGENTS`] only: every subagent and teammate the session
    /// has, as the mod last saw them.
    #[serde(default)]
    pub agents: Option<Vec<ModAgent>>,
    /// The conversation's title, read from the transcript by the listener
    /// before the event is passed on. Never in a payload.
    #[serde(skip)]
    pub title: Option<Title>,
    /// Horadric's own `Status` event only: what the status line was told.
    #[serde(skip)]
    pub status: Option<Status>,
    /// Horadric's own `Spent` event only: the API requests OpenTelemetry
    /// told of.
    #[serde(skip)]
    pub spent: Vec<crate::usage::Spent>,
}

/// One subagent or teammate as Horadric's Claude mod reports it, from
/// Claude Code's `$.agent.list()`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ModAgent {
    pub agent_id: String,
    /// `Explore`, `general-purpose`, a plugin's `<plugin>:<name>`, or
    /// `teammate`.
    #[serde(default)]
    pub agent_type: String,
    /// `pending`, `running`, `waiting`, `idle`, `completed`, `failed` or
    /// `killed`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub description: String,
    /// The agent that spawned it; none when the main loop did.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// When it started, in milliseconds since the Unix epoch, as the mod
    /// saw it spawn or first listed it. None from a mod too old to say.
    #[serde(default)]
    pub started_ms: Option<u64>,
}

impl HookEvent {
    /// The event `horadric run` sends before Claude starts, so a session shows
    /// up as idle instead of appearing on its first prompt.
    pub const REGISTER: &'static str = "HoradricRegister";

    /// Horadric's own event for a session whose process is gone but whose
    /// conversation can be resumed.
    pub const PAUSE: &'static str = "HoradricPause";

    /// Horadric's own event for what Claude Code gave the status line: the
    /// model, the context and the usage limits. It never changes a phase.
    pub const STATUS: &'static str = "HoradricStatus";

    /// What Horadric's Claude mod reports on every change: the session's
    /// subagents and teammates, each with its type and status.
    pub const AGENTS: &'static str = "HoradricAgents";

    /// What Horadric's Claude mod reports as a subagent starts, carrying its
    /// `agent_id` and `agent_type`.
    pub const SPAWN: &'static str = "HoradricSpawn";

    /// Horadric's own event for what a lore tool or a context pack showed
    /// a session, for the Arcane Sanctuary to light: `tool_input` holds the
    /// nodes and the touch (see `glow::told`).
    pub const GLOW: &'static str = "HoradricGlow";

    /// Horadric's own event for a session whose `horadric heavy` waits for
    /// a slot, or has stopped waiting: `tool_input` holds a
    /// [`crate::heavy::Queued`]. It never changes a phase.
    pub const HEAVY: &'static str = "HoradricHeavy";

    /// What Claude Code's OpenTelemetry export said a session's API
    /// requests cost, read by the listener's OTLP receiver.
    pub const SPENT: &'static str = "HoradricSpent";

    /// An event Horadric makes up itself, with every optional field empty.
    pub fn synthetic(hook_event_name: &str) -> Self {
        HookEvent {
            hook_event_name: hook_event_name.to_string(),
            ..Default::default()
        }
    }

    /// Parses a raw hook payload. Unknown fields are ignored.
    pub fn from_json(body: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(body)
    }

    /// Why a `StopFailure` stopped the turn, and what was said about it.
    /// Claude Code's own fields win over the ones Horadric makes up.
    pub fn failure(&self) -> (String, Option<&str>) {
        let kind = self
            .error
            .as_ref()
            .and_then(Value::as_str)
            .or(self.error_type.as_deref())
            .filter(|k| !k.is_empty())
            .unwrap_or("unknown");
        let said = self
            .error_details
            .as_deref()
            .or(self.error_message.as_deref())
            .filter(|m| !m.trim().is_empty());
        (kind.to_string(), said)
    }

    /// The effort the turn runs at: `low` up to `max`.
    pub fn effort_level(&self) -> Option<&str> {
        self.effort.as_ref()?.get("level")?.as_str()
    }

    /// True when the event came from a subagent rather than the main loop.
    pub fn is_subagent(&self) -> bool {
        self.agent_id.as_deref().is_some_and(|id| !id.is_empty())
    }

    /// Whether the conversation's title may have changed since the last
    /// event that said so. Claude Code writes it early in the first turn,
    /// `/rename` changes it between turns, and a resume brings an old one.
    pub fn may_retitle(&self) -> bool {
        !self.is_subagent()
            && !self.transcript_path.is_empty()
            && matches!(
                self.hook_event_name.as_str(),
                "SessionStart" | "UserPromptSubmit" | "Stop"
            )
    }

    /// Whether the end of the transcript is worth a look for a title. The
    /// first turn may run for an hour of tools, and Claude Code writes the
    /// title early in it, so the tile would keep the folder name until the
    /// turn ends. Only the end is read: this comes with every tool.
    pub fn may_peek_title(&self) -> bool {
        !self.is_subagent()
            && !self.transcript_path.is_empty()
            && self.hook_event_name == "PostToolUse"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mods_reports_parse() {
        let roster = br#"{"session_id":"c","hook_event_name":"HoradricAgents","agents":[
            {"agent_id":"a1","agent_type":"Explore","status":"completed","description":"look"},
            {"agent_id":"a2","status":"running","parent_id":"a1","teammate_id":"x@t"}]}"#;
        let e = HookEvent::from_json(roster).unwrap();
        assert_eq!(e.hook_event_name, HookEvent::AGENTS);
        let agents = e.agents.unwrap();
        assert_eq!(agents[0].agent_type, "Explore");
        assert_eq!(agents[0].status, "completed");
        assert_eq!(agents[1].parent_id.as_deref(), Some("a1"));
        assert!(!HookEvent::from_json(roster).unwrap().is_subagent());

        let spawn = br#"{"session_id":"c","hook_event_name":"HoradricSpawn",
            "agent_id":"a1","agent_type":"Explore","model":"claude-haiku-4-5"}"#;
        let e = HookEvent::from_json(spawn).unwrap();
        assert_eq!(e.agent_type.as_deref(), Some("Explore"));
        assert!(e.is_subagent());
    }

    #[test]
    fn claude_codes_newer_fields_parse() {
        let fail = br#"{"session_id":"c","hook_event_name":"StopFailure",
            "prompt_id":"550e8400","error":"rate_limit",
            "error_details":"429 Too Many Requests"}"#;
        let e = HookEvent::from_json(fail).unwrap();
        assert_eq!(e.failure(), ("rate_limit".into(), Some("429 Too Many Requests")));
        assert_eq!(e.prompt_id.as_deref(), Some("550e8400"));

        let tool = br#"{"session_id":"c","hook_event_name":"PreToolUse",
            "effort":{"level":"xhigh"},"tool_name":"Bash"}"#;
        assert_eq!(HookEvent::from_json(tool).unwrap().effort_level(), Some("xhigh"));
        // A shape the docs do not give must not lose the event.
        let odd = br#"{"session_id":"c","hook_event_name":"Stop","effort":"high","error":{"a":1}}"#;
        let e = HookEvent::from_json(odd).unwrap();
        assert_eq!(e.effort_level(), None);
        assert_eq!(e.failure().0, "unknown");

        let own = HookEvent {
            error_type: Some("billing_error".into()),
            error_message: Some("Credit balance is too low".into()),
            ..HookEvent::synthetic("StopFailure")
        };
        assert_eq!(own.failure(), ("billing_error".into(), Some("Credit balance is too low")));

        let switch = br#"{"session_id":"c","hook_event_name":"PostModelSwitch",
            "from_model":"claude-sonnet-5","to_model":"claude-opus-5","source":"command",
            "requested_model":null,"context_tokens":182340}"#;
        assert_eq!(HookEvent::from_json(switch).unwrap().to_model.as_deref(), Some("claude-opus-5"));

        let task = br#"{"session_id":"c","hook_event_name":"TaskCreated",
            "task_id":"task-001","task_subject":"Implement user authentication"}"#;
        assert_eq!(HookEvent::from_json(task).unwrap().task_id.as_deref(), Some("task-001"));
    }

    #[test]
    fn only_main_loop_turn_edges_with_a_transcript_retitle() {
        let e = |name: &str, path: &str, agent: Option<&str>| HookEvent {
            transcript_path: path.into(),
            agent_id: agent.map(str::to_string),
            ..HookEvent::synthetic(name)
        };
        assert!(e("Stop", "t.jsonl", None).may_retitle());
        assert!(e("UserPromptSubmit", "t.jsonl", None).may_retitle());
        assert!(!e("PreToolUse", "t.jsonl", None).may_retitle());
        assert!(!e("Stop", "", None).may_retitle());
        assert!(!e("Stop", "t.jsonl", Some("sub")).may_retitle());
    }

    #[test]
    fn a_tool_that_finished_peeks_at_the_title() {
        let e = |name: &str, agent: Option<&str>| HookEvent {
            transcript_path: "t.jsonl".into(),
            agent_id: agent.map(str::to_string),
            ..HookEvent::synthetic(name)
        };
        assert!(e("PostToolUse", None).may_peek_title());
        assert!(!e("PreToolUse", None).may_peek_title());
        assert!(!e("PostToolUse", Some("sub")).may_peek_title());
    }
}
