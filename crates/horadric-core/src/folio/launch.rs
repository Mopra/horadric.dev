//! A Folio's command line from the one a terminal session gets. Horadric
//! builds a Claude session's flags once for both, so the two cannot drift;
//! this takes out what the headless start sets itself or cannot run with,
//! and joins the system prompts, since Claude Code takes only one.

use crate::agent::{CLAUDE_LIST_FLAGS, CLAUDE_VALUE_FLAGS};

/// Set by the headless start itself (its stream-json flags and
/// `--resume`), or meaning a session that ends after one turn, so one
/// among a session's arguments would fight the start.
const SET_BY_START: &[&str] = &[
    "-p",
    "--print",
    "--input-format",
    "--output-format",
    "--verbose",
    "--include-partial-messages",
    "--permission-prompt-tool",
    "--replay-user-messages",
    "--resume",
    "-r",
    "--continue",
    "-c",
];

const SYSTEM: &str = "--append-system-prompt";

/// `args` (Horadric's flags first, then the session's own) as a headless
/// start takes them: without what [`SET_BY_START`] lists, without any
/// prompt, since a Folio sends it as its first message, and with every
/// `--append-system-prompt`, and `add` after them, joined into one.
pub fn headless_args(args: &[String], add: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let mut system: Vec<String> = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if a == "--" {
            break;
        }
        if !a.starts_with('-') {
            // A prompt, sent as the first message instead.
            continue;
        }
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut group = vec![a.clone()];
        if inline.is_none() {
            if name == SYSTEM {
                // Horadric's own prompts may start with anything.
                group.extend(it.next().cloned());
            } else if CLAUDE_LIST_FLAGS.contains(&name) {
                while let Some(v) = it.next_if(|v| !v.starts_with('-')) {
                    group.push(v.clone());
                }
            } else if CLAUDE_VALUE_FLAGS.contains(&name) || name == "--resume" || name == "-r" {
                group.extend(it.next_if(|v| !v.starts_with('-')).cloned());
            }
        }
        if SET_BY_START.contains(&name) {
            continue;
        }
        if name == SYSTEM {
            system.extend(inline.or_else(|| group.get(1).cloned()));
            continue;
        }
        out.extend(group);
    }
    system.extend(add.map(str::to_string));
    system.retain(|s| !s.is_empty());
    if !system.is_empty() {
        out.push(format!("{SYSTEM}={}", system.join("\n\n")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn system_prompts_join_into_one_with_the_added_last() {
        let args = s(&[
            "--append-system-prompt",
            "quest log",
            "--model",
            "opus",
            "--append-system-prompt=own",
        ]);
        assert_eq!(
            headless_args(&args, Some("subagents")),
            s(&[
                "--model",
                "opus",
                "--append-system-prompt=quest log\n\nown\n\nsubagents"
            ])
        );
    }

    #[test]
    fn the_prompt_and_what_the_start_sets_are_left_out() {
        let args = s(&[
            "--dangerously-skip-permissions",
            "-p",
            "--output-format",
            "text",
            "--resume",
            "abc",
            "--allowedTools",
            "Bash(git:*)",
            "Read",
            "--mcp-config=c.json",
            "do the quest",
        ]);
        assert_eq!(
            headless_args(&args, None),
            s(&[
                "--dangerously-skip-permissions",
                "--allowedTools",
                "Bash(git:*)",
                "Read",
                "--mcp-config=c.json",
            ])
        );
    }

    #[test]
    fn a_system_prompt_starting_with_a_dash_stays_its_value() {
        let args = s(&["--append-system-prompt", "- a list", "--verbose"]);
        assert_eq!(
            headless_args(&args, None),
            s(&["--append-system-prompt=- a list"])
        );
    }
}
