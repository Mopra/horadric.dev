//! The coding agent a session runs, which is Claude Code. It is a type of
//! its own, not a bare string, because old saved state names the agent each
//! session ran, and a session saved by another agent must not load as
//! Claude's: its conversation id means nothing to `claude --resume`. A
//! session saved before there was a choice has none on disk, and reads as
//! Claude.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::usage::{has_flag, Setting};
use crate::HookEvent;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
    Claude,
}

impl Agent {
    /// The program on `PATH`, without its extension.
    pub fn program(self) -> &'static str {
        "claude"
    }

    /// What a person calls it.
    pub fn label(self) -> &'static str {
        "Claude Code"
    }

    /// Whose subscription it runs on, which is whose limits it spends.
    pub fn provider(self) -> &'static str {
        "Claude"
    }

    /// What goes in front to carry on conversation `id`, or to open the
    /// agent's own picker when there is none.
    pub fn resume_args(self, id: Option<&str>) -> Vec<String> {
        let mut out = vec!["--resume".to_string()];
        out.extend(id.map(str::to_string));
        out
    }

    /// Whether `args` already carry on a conversation, which the agent
    /// keeps by the folder it was held in.
    pub fn carries_on(self, args: &[String]) -> bool {
        ["--resume", "-r", "--continue", "-c"]
            .iter()
            .any(|f| has_flag(args, f))
    }

    /// The arguments to carry on: `args` with `--resume <id>` in front when
    /// there is conversation `id` to resume, and without any earlier resume
    /// or continue, which would fight it.
    pub fn carry_on(self, id: Option<&str>, args: &[String]) -> Vec<String> {
        let mut out = id.map_or_else(Vec::new, |id| self.resume_args(Some(id)));
        if id.is_some() {
            // The prompt it was started with was sent then, and a resume
            // would send it again.
            out.extend(without_claude_prompt(&without_resume(args)));
        } else {
            out.extend(without_resume(args));
        }
        out
    }

    /// The flags that give setting `s` the value `value`. None where the
    /// setting is no flag.
    pub fn setting_args(self, s: Setting, value: &str) -> Option<Vec<String>> {
        let flag = match s {
            Setting::Model => "--model",
            Setting::Effort => "--effort",
            Setting::Permissions => "--permission-mode",
            Setting::Workflow => return None,
        };
        Some(vec![flag.to_string(), value.to_string()])
    }

    /// Whether a session started with `args` chose setting `s` itself, so
    /// the defaults leave it alone.
    pub fn chosen(self, s: Setting, args: &[String]) -> bool {
        let flags: &[&str] = match s {
            Setting::Model => &["--model"],
            Setting::Effort => &["--effort"],
            Setting::Permissions => &["--permission-mode", "--dangerously-skip-permissions"],
            Setting::Workflow => &[],
        };
        flags.iter().any(|f| has_flag(args, f))
    }

    /// The flags that start it with no permission prompt at all, for a
    /// quest in a worktree of its own, where a wrong command touches only
    /// that worktree. In the `--permission-mode` form the other modes take.
    pub fn bypass_args(self) -> Vec<String> {
        vec!["--permission-mode".into(), "bypassPermissions".into()]
    }

    /// The flags that give it Horadric's MCP server, the tools for the
    /// project's browser pane, read from `config`, a file, in the `=` form
    /// so a prompt after it is not taken for a second file.
    pub fn mcp_args(self, config: Option<&str>) -> Vec<String> {
        config
            .map(|c| vec![format!("--mcp-config={c}")])
            .unwrap_or_default()
    }

    /// The hook payload `body` as a [`HookEvent`].
    pub fn event(self, body: &[u8]) -> Option<HookEvent> {
        HookEvent::from_json(body).ok()
    }
}

/// `args` less any resume or continue flag and the value it took.
fn without_resume(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut args = args.iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--resume" | "-r" | "--session-id" => {
                // Their value, unless the next thing is another flag.
                if args.as_slice().first().is_some_and(|v| !v.starts_with('-')) {
                    args.next();
                }
            }
            "--continue" | "-c" => {}
            _ if a.starts_with("--resume=") || a.starts_with("--session-id=") => {}
            _ => out.push(a.clone()),
        }
    }
    out
}

/// Claude's flags that take one value, as the next argument, or may
/// (`--debug`, `--worktree` and the like), which then take the next
/// argument that is not a flag, prompt or not.
pub(crate) const CLAUDE_VALUE_FLAGS: [&str; 38] = [
    "--agent",
    "--agents",
    "--append-system-prompt",
    "--autocompact",
    "--client-data-url",
    "--cloud",
    "-d",
    "--debug",
    "--debug-file",
    "--effort",
    "--environment",
    "--fallback-model",
    "--from-pr",
    "--input-format",
    "--json-schema",
    "--max-budget-usd",
    "--model",
    "-n",
    "--name",
    "--output-format",
    "--permission-mode",
    "--permission-prompts",
    "--plugin-dir",
    "--plugin-url",
    "--prompt-suggestions",
    "--remote-control",
    "--remote-control-session-name-prefix",
    "--session-id",
    "--setting-sources",
    "--settings",
    "--system-prompt",
    "--system-prompt-snapshot",
    "--teleport",
    "-w",
    "--worktree",
    "--permission-prompt-tool",
    "--system-prompt-file",
    "--append-system-prompt-file",
];

/// Claude's flags that take every argument after them up to the next flag,
/// so no prompt can follow one.
pub(crate) const CLAUDE_LIST_FLAGS: [&str; 9] = [
    "--add-dir",
    "--allowedTools",
    "--allowed-tools",
    "--betas",
    "--disallowedTools",
    "--disallowed-tools",
    "--file",
    "--mcp-config",
    "--tools",
];

/// Claude's `args` less the prompt. A value that starts with a dash stays
/// by being a flag itself.
fn without_claude_prompt(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut args = args.iter().peekable();
    while let Some(a) = args.next() {
        if a == "--" {
            break;
        }
        if CLAUDE_LIST_FLAGS.contains(&a.as_str()) {
            out.push(a.clone());
            while let Some(v) = args.next_if(|v| !v.starts_with('-')) {
                out.push(v.clone());
            }
        } else if CLAUDE_VALUE_FLAGS.contains(&a.as_str()) {
            out.push(a.clone());
            out.extend(args.next_if(|v| !v.starts_with('-')).cloned());
        } else if a.starts_with('-') {
            out.push(a.clone());
        }
    }
    out
}

/// The MCP config Horadric hands each `claude` it starts with
/// `--mcp-config` (see [`Agent::mcp_args`]): `horadric mcp`, run from
/// `exe`, which finds its session by the environment it inherits.
pub fn mcp_config(exe: &str) -> Value {
    serde_json::json!({
        "mcpServers": {
            "horadric": { "type": "stdio", "command": exe, "args": ["mcp"] }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn the_mcp_config_runs_horadric_mcp_from_the_exe() {
        let c = mcp_config("/home/a/.local/bin/horadric");
        let h = &c["mcpServers"]["horadric"];
        assert_eq!(h["type"], "stdio");
        assert_eq!(h["command"], "/home/a/.local/bin/horadric");
        assert_eq!(h["args"], serde_json::json!(["mcp"]));
    }

    #[test]
    fn claude_payloads_read_as_they_always_have() {
        let e = Agent::Claude
            .event(br#"{"session_id":"c","hook_event_name":"Stop","extra":1}"#)
            .unwrap();
        assert_eq!(e.session_id, "c");
        assert_eq!(e.hook_event_name, "Stop");
        assert!(Agent::Claude.event(b"not json").is_none());
    }

    #[test]
    fn the_browser_server_comes_from_its_config_file() {
        assert_eq!(
            Agent::Claude.mcp_args(Some(r"C:\a\claude-mcp.json")),
            [r"--mcp-config=C:\a\claude-mcp.json"]
        );
        assert!(Agent::Claude.mcp_args(None).is_empty());
    }

    #[test]
    fn a_session_saved_without_an_agent_is_claude() {
        #[derive(Deserialize)]
        struct Old {
            #[serde(default)]
            agent: Agent,
        }
        let old: Old = serde_json::from_str("{}").unwrap();
        assert_eq!(old.agent, Agent::Claude);
        let claude: Old = serde_json::from_str(r#"{"agent":"claude"}"#).unwrap();
        assert_eq!(claude.agent, Agent::Claude);
        // Another agent's session is not taken for Claude's.
        assert!(serde_json::from_str::<Old>(r#"{"agent":"codex"}"#).is_err());
        assert_eq!(
            serde_json::to_string(&Agent::Claude).unwrap(),
            r#""claude""#
        );
    }

    #[test]
    fn claude_carries_on_as_it_always_has() {
        let a = Agent::Claude;
        assert_eq!(
            a.carry_on(
                Some("abc"),
                &args("-c --resume old --model x --session-id=z")
            ),
            args("--resume abc --model x")
        );
        assert_eq!(
            a.carry_on(Some("abc"), &args("--resume --verbose")),
            args("--resume abc --verbose")
        );
        assert_eq!(a.carry_on(None, &args("--model x")), args("--model x"));
    }

    #[test]
    fn claude_resumes_without_its_first_prompt() {
        let a = Agent::Claude;
        assert_eq!(
            a.carry_on(
                Some("id"),
                &args("--model x -p pong --settings s --dangerously-skip-permissions")
            ),
            args("--resume id --model x -p --settings s --dangerously-skip-permissions")
        );
        assert_eq!(
            a.carry_on(Some("id"), &args("--add-dir a b --verbose pong")),
            args("--resume id --add-dir a b --verbose")
        );
        assert_eq!(
            a.carry_on(Some("id"), &args("-w tree --debug -- pong")),
            args("--resume id -w tree --debug")
        );
        assert_eq!(
            a.carry_on(None, &args("--model x pong")),
            args("--model x pong")
        );
    }

    #[test]
    fn what_counts_as_carrying_on() {
        assert!(Agent::Claude.carries_on(&args("--resume x")));
        assert!(Agent::Claude.carries_on(&args("--resume=x")));
        assert!(Agent::Claude.carries_on(&args("-c")));
        assert!(!Agent::Claude.carries_on(&args("--model x")));
    }

    #[test]
    fn each_setting_has_its_flag() {
        let flags = |s| Agent::Claude.setting_args(s, "v");
        assert_eq!(flags(Setting::Model), Some(args("--model v")));
        assert_eq!(flags(Setting::Effort), Some(args("--effort v")));
        assert_eq!(
            flags(Setting::Permissions),
            Some(args("--permission-mode v"))
        );
        assert_eq!(flags(Setting::Workflow), None);
    }

    #[test]
    fn a_bypass_is_a_permission_choice() {
        let a = Agent::Claude;
        assert!(a.chosen(Setting::Permissions, &a.bypass_args()));
        assert_eq!(a.bypass_args(), args("--permission-mode bypassPermissions"));
    }

    #[test]
    fn a_setting_the_session_chose_itself_is_seen() {
        assert!(Agent::Claude.chosen(
            Setting::Permissions,
            &args("--dangerously-skip-permissions")
        ));
        assert!(Agent::Claude.chosen(Setting::Model, &args("--model x")));
        assert!(!Agent::Claude.chosen(Setting::Model, &args("-m x")));
    }
}
