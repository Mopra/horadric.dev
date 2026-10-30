//! How a session ended, in Diablo's item colours: what it changed, whether
//! the tests passed after, and whether its work landed on the main branch.
//! Heard from the tool hooks, never read off the terminal. The UI prints
//! the key's name in the colour, so it lives apart from the phase (light,
//! in the lamp) and the project (accent, on the cluster).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::HookEvent;

/// The colour a session's name takes, from least to most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rarity {
    /// White: it changed nothing.
    Normal,
    /// Blue: it changed files.
    Magic,
    /// Yellow: it changed files and the tests passed after the last change.
    Rare,
    /// Green: one of a batch, items the runner held at once, that changed
    /// something.
    Set,
    /// Gold: what it committed is on the main branch.
    Unique,
}

/// What a session has done that decides its rarity. Saved, since a
/// session paused by a restart ended the same way it did before.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loot {
    /// A tool that writes files ran without failing.
    #[serde(default)]
    pub changed: bool,
    /// How the last test run since the last change went, None when none
    /// ran since.
    #[serde(default)]
    pub tests: Option<bool>,
    /// A `git commit` or `git merge` went through.
    #[serde(default)]
    pub committed: bool,
    /// Its branch was, when last looked at, in what the main tree has
    /// checked out. Only looked at once it committed.
    #[serde(default)]
    pub landed: bool,
    /// Started by the runner as one of several items held at once.
    #[serde(default)]
    pub batch: bool,
    /// The tool use ids of background Bash, PowerShell, Agent and Monitor
    /// calls whose ending `<task-notification>` has not come back in a
    /// prompt yet, empty for a call whose hook gave none. Kept across
    /// turns: the work outlives the turn that started it. By id, since a
    /// notification about other work (a Monitor's stream ending, an agent
    /// that stopped for its own background work) must end only its own.
    /// Saved under a new name: the old one held a count.
    #[serde(default, rename = "background")]
    pub outstanding: Vec<String>,
    /// A ScheduleWakeup call is waiting, until the next prompt.
    #[serde(default)]
    pub waiting: bool,
}

/// The tool use ids, empty where none is quoted, of the
/// `<task-notification>` blocks in a prompt that say their work ended.
/// Hooks say when background work starts but not when it ends; the
/// notification that wakes the session is the only word of it. An agent
/// that stopped while work of its own still runs is reported as completed
/// with a note that it may resume, so it has not ended.
pub fn ended_tasks(prompt: &str) -> Vec<String> {
    let tag = |chunk: &str, name: &str| -> Option<String> {
        let rest = chunk.split(&format!("<{name}>")).nth(1)?;
        Some(rest.split(&format!("</{name}>")).next()?.trim().to_string())
    };
    prompt
        .split("<task-notification>")
        .skip(1)
        .map(|chunk| chunk.split("</task-notification>").next().unwrap_or(chunk))
        .filter(|chunk| {
            tag(chunk, "status")
                .is_some_and(|s| matches!(s.as_str(), "completed" | "failed" | "killed"))
        })
        .filter(|chunk| {
            !tag(chunk, "summary")
                .is_some_and(|s| s.ends_with("waiting on its own background work"))
        })
        .map(|chunk| tag(chunk, "tool-use-id").unwrap_or_default())
        .collect()
}

/// Whether a tool call that returned left work running behind it: sent to
/// the background, a Monitor, or a command moved there when it ran past
/// its timeout.
fn backgrounded(tool: &str, input: Option<&Value>, response: Option<&Value>) -> bool {
    tool == "Monitor"
        || input
            .and_then(|i| i.get("run_in_background"))
            .and_then(Value::as_bool)
            == Some(true)
        || response.is_some_and(|r| r.get("backgroundTaskId").is_some_and(|v| !v.is_null()))
}

/// The tools that write files. A shell command may too, which the
/// worktree's diff catches where there is one.
pub const WRITERS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

impl Loot {
    /// Takes in a tool that finished, well or not. Subagents count: what
    /// they change is the session's work.
    pub fn hear(&mut self, event: &HookEvent) {
        match event.hook_event_name.as_str() {
            "UserPromptSubmit" if !event.is_subagent() => {
                self.waiting = false;
                for id in ended_tasks(event.user_prompt.as_deref().unwrap_or_default()) {
                    // A notification for work not heard starting ends one
                    // whose hook gave no id, if there is one.
                    let at = self.outstanding.iter().position(|o| *o == id);
                    let at = at.or_else(|| self.outstanding.iter().position(String::is_empty));
                    if let Some(at) = at {
                        self.outstanding.remove(at);
                    }
                }
                return;
            }
            // A new process knows of none of the old one's work. A
            // compaction is the same process, its work still running.
            "SessionStart"
                if !event.is_subagent() && event.source.as_deref() != Some("compact") =>
            {
                self.waiting = false;
                self.outstanding.clear();
                return;
            }
            _ => {}
        }
        let ok = match event.hook_event_name.as_str() {
            "PostToolUse" => true,
            "PostToolUseFailure" => false,
            _ => return,
        };
        let Some(tool) = event.tool_name.as_deref() else {
            return;
        };
        // The wakeup's own prompt is what ends its wait.
        if tool == "ScheduleWakeup" {
            if ok {
                self.waiting = true;
            }
            return;
        }
        if WRITERS.contains(&tool) {
            if ok {
                self.changed = true;
                self.tests = None;
            }
            return;
        }
        // One sent to the background reports back at once, before it ran.
        // A subagent's own is the subagent's to wait on: its notification
        // never reaches the session's prompt, and the agent itself is
        // counted.
        if backgrounded(
            tool,
            event.tool_input.as_ref(),
            event.tool_response.as_ref(),
        ) {
            if ok && !event.is_subagent() {
                self.outstanding
                    .push(event.tool_use_id.clone().unwrap_or_default());
            }
            return;
        }
        let Some(input) = event.tool_input.as_ref() else {
            return;
        };
        let Some(command) = input.get("command").and_then(Value::as_str) else {
            return;
        };
        if runs_tests(command) {
            self.tests = Some(ok);
        }
        if ok && commits(command) {
            self.committed = true;
        }
    }

    /// Whether the session has work of its own still running, from this
    /// turn or an earlier one, a Monitor among it, or sleeps on a wakeup: a stop is
    /// then a wait, which a nudge would only interrupt.
    pub fn waiting_on_own_work(&self) -> bool {
        !self.outstanding.is_empty() || self.waiting
    }

    /// Whether git should be asked if its work landed.
    pub fn may_land(&self) -> bool {
        self.committed
    }
}

/// The rarity of a session with `loot`, whose worktree has something
/// changed or committed when `diff` is true. Landing beats being one of a
/// batch, since landing is what the batch was for.
pub fn rarity(loot: &Loot, diff: bool) -> Rarity {
    let changed = loot.changed || loot.committed || diff;
    if !changed {
        Rarity::Normal
    } else if loot.landed {
        Rarity::Unique
    } else if loot.batch {
        Rarity::Set
    } else if loot.tests == Some(true) {
        Rarity::Rare
    } else {
        Rarity::Magic
    }
}

/// The simple commands of a command line, each as its words with any
/// `NAME=value` in front dropped. Quotes are not understood, which at
/// worst splits a quoted `&&` and finds nothing in the halves.
fn simple_commands(line: &str) -> Vec<Vec<&str>> {
    line.split(['&', '|', ';', '\n', '(', ')', '{', '}'])
        .map(|part| {
            part.split_whitespace()
                .skip_while(|w| w.contains('=') && !w.starts_with('-'))
                .collect::<Vec<_>>()
        })
        .filter(|words| !words.is_empty())
        .collect()
}

/// A program's name without its folder, `.exe` or case.
fn program(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let name = name.to_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

/// Whether a shell command line runs a test suite.
pub fn runs_tests(line: &str) -> bool {
    simple_commands(line).iter().any(|words| {
        let first = program(words[0]);
        let second = words.get(1).map(|w| w.to_lowercase()).unwrap_or_default();
        let third = words.get(2).map(|w| w.to_lowercase()).unwrap_or_default();
        match first.as_str() {
            "pytest" | "jest" | "vitest" | "rspec" | "phpunit" | "ctest" | "tox" | "nox" => true,
            "cargo" => matches!(second.as_str(), "test" | "nextest"),
            "npm" | "pnpm" | "yarn" | "bun" | "deno" => {
                second == "test" || second == "t" || (second == "run" && third.starts_with("test"))
            }
            "npx" | "pnpx" | "bunx" => matches!(second.as_str(), "jest" | "vitest" | "playwright"),
            "go" | "dotnet" | "mvn" | "gradle" | "gradlew" | "make" | "swift" | "mix" => {
                second == "test"
            }
            "python" | "python3" | "py" => {
                second == "-m" && matches!(third.as_str(), "pytest" | "unittest")
            }
            _ => false,
        }
    })
}

/// Whether a shell command line commits or merges with git. Options
/// before the subcommand, `-C <dir>` and `-c <key=value>`, are skipped.
pub fn commits(line: &str) -> bool {
    simple_commands(line).iter().any(|words| {
        if program(words[0]) != "git" {
            return false;
        }
        let mut rest = words[1..].iter();
        while let Some(w) = rest.next() {
            match *w {
                "-C" | "-c" | "--git-dir" | "--work-tree" => {
                    rest.next();
                }
                w if w.starts_with('-') => {}
                w => return matches!(w, "commit" | "merge"),
            }
        }
        false
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(hook: &str, name: &str, input: Value) -> HookEvent {
        HookEvent {
            tool_name: Some(name.into()),
            tool_input: Some(input),
            ..HookEvent::synthetic(hook)
        }
    }

    fn bash(hook: &str, command: &str) -> HookEvent {
        tool(hook, "Bash", json!({ "command": command }))
    }

    #[test]
    fn nothing_done_is_normal() {
        assert_eq!(rarity(&Loot::default(), false), Rarity::Normal);
    }

    #[test]
    fn an_edit_is_magic_and_tests_after_it_are_rare() {
        let mut l = Loot::default();
        l.hear(&tool("PostToolUse", "Edit", json!({ "file_path": "a.rs" })));
        assert_eq!(rarity(&l, false), Rarity::Magic);
        l.hear(&bash(
            "PostToolUse",
            "cargo fmt --all && cargo test --workspace",
        ));
        assert_eq!(rarity(&l, false), Rarity::Rare);
    }

    #[test]
    fn an_edit_after_the_tests_needs_them_again() {
        let mut l = Loot::default();
        l.hear(&tool("PostToolUse", "Write", json!({})));
        l.hear(&bash("PostToolUse", "npm test"));
        l.hear(&tool("PostToolUse", "Edit", json!({})));
        assert_eq!(rarity(&l, false), Rarity::Magic);
    }

    #[test]
    fn failing_tests_are_not_rare() {
        let mut l = Loot::default();
        l.hear(&tool("PostToolUse", "Edit", json!({})));
        l.hear(&bash("PostToolUse", "pytest"));
        l.hear(&bash("PostToolUseFailure", "pytest -x"));
        assert_eq!(rarity(&l, false), Rarity::Magic);
    }

    #[test]
    fn a_failed_edit_changes_nothing() {
        let mut l = Loot::default();
        l.hear(&tool("PostToolUseFailure", "Edit", json!({})));
        l.hear(&tool("PreToolUse", "Write", json!({})));
        assert_eq!(rarity(&l, false), Rarity::Normal);
    }

    fn background_bash() -> HookEvent {
        tool(
            "PostToolUse",
            "Bash",
            json!({ "command": "sleep 60", "run_in_background": true }),
        )
    }

    fn with_id(event: HookEvent, id: &str) -> HookEvent {
        HookEvent {
            tool_use_id: Some(id.into()),
            ..event
        }
    }

    fn ended(id: &str, summary: &str) -> String {
        format!(
            "<task-notification>
<task-id>x</task-id>
<tool-use-id>{id}</tool-use-id>
             <status>completed</status>
<summary>{summary}</summary>
</task-notification>
"
        )
    }

    fn prompt(text: &str) -> HookEvent {
        HookEvent {
            user_prompt: Some(text.into()),
            ..HookEvent::synthetic("UserPromptSubmit")
        }
    }

    fn done(n: usize) -> String {
        "<task-notification><status>completed</status></task-notification>
"
        .repeat(n)
    }

    #[test]
    fn a_stop_with_background_work_running_is_a_wait() {
        let mut l = Loot::default();
        l.hear(&background_bash());
        l.hear(&tool("Stop", "", json!({})));
        assert!(l.waiting_on_own_work());
    }

    #[test]
    fn work_from_an_earlier_turn_still_counts() {
        let mut l = Loot::default();
        l.hear(&background_bash());
        l.hear(&tool(
            "PostToolUse",
            "Agent",
            json!({ "prompt": "review", "run_in_background": true }),
        ));
        l.hear(&tool("Stop", "", json!({})));
        // The checks end and wake it; the review agent still runs.
        l.hear(&prompt(&done(1)));
        l.hear(&tool("Stop", "", json!({})));
        assert!(l.waiting_on_own_work());
        l.hear(&prompt(&done(1)));
        l.hear(&tool("Stop", "", json!({})));
        assert!(!l.waiting_on_own_work());
    }

    #[test]
    fn a_human_prompt_ends_nothing() {
        let mut l = Loot::default();
        l.hear(&background_bash());
        l.hear(&prompt("carry on"));
        assert!(l.waiting_on_own_work());
    }

    #[test]
    fn a_wakeup_waits_until_the_next_prompt() {
        let mut l = Loot::default();
        l.hear(&tool(
            "PostToolUse",
            "ScheduleWakeup",
            json!({ "delaySeconds": 60 }),
        ));
        l.hear(&tool("Stop", "", json!({})));
        assert!(l.waiting_on_own_work());
        l.hear(&prompt("woke"));
        assert!(!l.waiting_on_own_work());
    }

    #[test]
    fn a_monitor_waits_through_its_events_until_its_stream_ends() {
        let mut l = Loot::default();
        l.hear(&with_id(
            tool(
                "PostToolUse",
                "Monitor",
                json!({ "command": "tail -f log" }),
            ),
            "toolu_m",
        ));
        // An event wakes the session; the monitor still watches.
        l.hear(&prompt(
            "<task-notification>
<task-id>b1</task-id>
<summary>Monitor event: \"x\"</summary>
<event>line</event>
</task-notification>",
        ));
        l.hear(&tool("Stop", "", json!({})));
        assert!(l.waiting_on_own_work());
        l.hear(&prompt(&ended("toolu_m", "Monitor \"x\" stream ended")));
        assert!(!l.waiting_on_own_work());
    }

    #[test]
    fn a_monitor_ending_does_not_end_a_check_still_running() {
        let mut l = Loot::default();
        l.hear(&with_id(background_bash(), "toolu_check"));
        l.hear(&prompt(&ended("toolu_other", "Monitor \"x\" stream ended")));
        assert!(l.waiting_on_own_work());
        l.hear(&prompt(&ended("toolu_check", "Background command done")));
        assert!(!l.waiting_on_own_work());
    }

    #[test]
    fn an_agent_waiting_on_its_own_work_has_not_ended() {
        let mut l = Loot::default();
        l.hear(&with_id(
            tool(
                "PostToolUse",
                "Agent",
                json!({ "prompt": "build", "run_in_background": true }),
            ),
            "toolu_a",
        ));
        let waits = "Agent \"Build\" is waiting on its own background work";
        l.hear(&prompt(&ended("toolu_a", waits)));
        assert!(l.waiting_on_own_work());
        l.hear(&prompt(&ended("toolu_a", "Agent \"Build\" completed")));
        assert!(!l.waiting_on_own_work());
    }

    #[test]
    fn a_command_moved_to_the_background_on_its_timeout_counts() {
        let mut l = Loot::default();
        l.hear(&HookEvent {
            tool_response: Some(json!({ "stdout": "", "backgroundTaskId": "b7" })),
            ..with_id(bash("PostToolUse", "horadric heavy cargo test"), "toolu_t")
        });
        l.hear(&tool("Stop", "", json!({})));
        assert!(l.waiting_on_own_work());
        // Its tests have not run yet either.
        assert_eq!(l.tests, None);
    }

    #[test]
    fn a_subagents_background_work_is_not_the_sessions() {
        let mut l = Loot::default();
        l.hear(&HookEvent {
            agent_id: Some("sub".into()),
            ..with_id(background_bash(), "toolu_sub")
        });
        l.hear(&HookEvent {
            tool_response: Some(json!({ "backgroundTaskId": null })),
            ..bash("PostToolUse", "ls")
        });
        assert!(!l.waiting_on_own_work());
    }

    #[test]
    fn a_compaction_keeps_the_work_running() {
        let mut l = Loot::default();
        l.hear(&background_bash());
        l.hear(&HookEvent {
            source: Some("compact".into()),
            ..HookEvent::synthetic("SessionStart")
        });
        assert!(l.waiting_on_own_work());
    }

    #[test]
    fn a_count_saved_by_the_old_build_still_loads() {
        let l: Loot = serde_json::from_str(r#"{ "changed": true, "outstanding": 2 }"#).unwrap();
        assert!(l.changed && l.outstanding.is_empty());
        // A tomb's `finished`, from before the tombs were removed.
        let l: Loot = serde_json::from_str(r#"{ "batch": true, "finished": true }"#).unwrap();
        assert!(l.batch);
    }

    #[test]
    fn more_ended_than_started_stays_at_nothing() {
        let mut l = Loot::default();
        l.hear(&prompt(&done(3)));
        assert!(l.outstanding.is_empty());
    }

    #[test]
    fn only_completed_failed_or_killed_notifications_end_work() {
        let text = "<task-notification><tool-use-id>a</tool-use-id><status>running</status></task-notification>            <task-notification><tool-use-id>b</tool-use-id><status>failed</status></task-notification>            <task-notification><status>completed</status></task-notification>";
        assert_eq!(ended_tasks(text), vec!["b".to_string(), String::new()]);
        assert!(ended_tasks("no notice here").is_empty());
    }

    #[test]
    fn a_new_process_forgets_old_work() {
        let mut l = Loot::default();
        l.hear(&background_bash());
        l.hear(&HookEvent::synthetic("SessionStart"));
        assert!(!l.waiting_on_own_work());
    }

    #[test]
    fn a_stop_without_background_work_is_not_a_wait() {
        let mut l = Loot::default();
        l.hear(&tool("Stop", "", json!({})));
        assert!(!l.waiting_on_own_work());
    }

    #[test]
    fn tests_sent_to_the_background_have_not_run() {
        let mut l = Loot::default();
        l.hear(&tool(
            "PostToolUse",
            "Bash",
            json!({ "command": "cargo test", "run_in_background": true }),
        ));
        assert_eq!(l.tests, None);
    }

    #[test]
    fn a_worktree_diff_counts_as_a_change() {
        let mut l = Loot::default();
        assert_eq!(rarity(&l, true), Rarity::Magic);
        l.tests = Some(true);
        assert_eq!(rarity(&l, true), Rarity::Rare);
    }

    #[test]
    fn landed_is_unique_and_beats_a_batch() {
        let mut l = Loot::default();
        l.hear(&bash("PostToolUse", "git -C ../main merge --ff-only item"));
        assert!(l.may_land());
        l.batch = true;
        assert_eq!(rarity(&l, false), Rarity::Set);
        l.landed = true;
        assert_eq!(rarity(&l, false), Rarity::Unique);
    }

    #[test]
    fn a_batch_that_changed_nothing_stays_normal() {
        let l = Loot {
            batch: true,
            ..Loot::default()
        };
        assert_eq!(rarity(&l, false), Rarity::Normal);
    }

    #[test]
    fn a_failed_commit_is_not_a_commit() {
        let mut l = Loot::default();
        l.hear(&bash("PostToolUseFailure", "git commit -m x"));
        assert!(!l.may_land());
    }

    #[test]
    fn test_commands_are_recognised() {
        for yes in [
            "cargo test",
            "cargo nextest run",
            "RUST_LOG=1 cargo test -p core",
            "cd app && npm run test:unit",
            "pnpm test",
            "yarn t",
            "npx vitest run",
            "python -m pytest tests",
            "C:\\Python\\python.exe -m unittest",
            "go test ./...",
            "dotnet test",
            "./gradlew test",
            "make test",
            "cargo clippy; cargo test",
        ] {
            assert!(runs_tests(yes), "{yes}");
        }
        for no in [
            "cargo build",
            "npm install",
            "echo test",
            "git commit -m 'cargo test'",
            "ls tests",
            "cargo fmt --all",
        ] {
            assert!(!runs_tests(no), "{no}");
        }
    }

    #[test]
    fn commits_and_merges_are_recognised() {
        assert!(commits("git commit -m 'x'"));
        assert!(commits("git add -A && git commit -q -m x"));
        assert!(commits("git -C C:/repo merge --ff-only rarity"));
        assert!(commits("git -c user.name=x commit"));
        assert!(!commits("git status"));
        assert!(!commits("git log --grep commit"));
        assert!(!commits("echo git commit"));
    }

    #[test]
    fn loot_reads_back_from_nothing() {
        let l: Loot = serde_json::from_str("{}").unwrap();
        assert_eq!(l, Loot::default());
    }
}
