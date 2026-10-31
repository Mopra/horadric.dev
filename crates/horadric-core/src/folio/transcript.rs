//! A resumed session's history. `--resume` does not replay earlier
//! messages on stdout (checked on 2.1.312: only the new turn streams), so
//! a Folio that resumes reads Claude Code's own transcript,
//! `~/.claude/projects/<slug>/<id>.jsonl`, into the same blocks.
//!
//! A transcript holds a line per message, like the stream's whole
//! `assistant` and `user` lines but with `toolUseResult` in camel case,
//! among many lines that are not messages (attachments, API request
//! records, cost and queue bookkeeping), which are skipped. Lines marked
//! `isSidechain` are a subagent's, `isMeta` the CLI's own notes, and
//! `isCompactSummary` the summary a compact left: none is a prompt.
//!
//! Every message line names the one before it (`parentUuid`). A rewind
//! leaves the turns it undid in the file and carries on from an earlier
//! line, so a prompt whose parent is not the line before it starts a new
//! branch, and the one it left is not shown.

use std::collections::HashMap;

use serde_json::Value;

use super::{command_output, Block, Folio, ImageData, Notice, PromptImage};

impl Folio {
    /// A Folio holding a transcript's conversation, as if it had streamed.
    pub fn from_transcript(jsonl: &str) -> Folio {
        Folio::from_transcript_at(jsonl, None)
    }

    /// [`Folio::from_transcript`] up to the line `at` and no further: a
    /// conversation resumed there with `--resume-session-at`, whose undone
    /// turns are still last in the file until it writes a branch of its own.
    pub fn from_transcript_at(jsonl: &str, at: Option<&str>) -> Folio {
        let mut f = Folio::new();
        f.replay(jsonl, false, at);
        f
    }

    /// A subagent's own transcript as a Folio, its task the first
    /// prompt. Every line of it is marked as a sidechain, which a
    /// session's replay leaves out.
    pub fn from_sidechain(jsonl: &str) -> Folio {
        let mut f = Folio::new();
        f.replay(jsonl, true, None);
        f
    }

    /// Adds a transcript's conversation to the blocks and marks it as
    /// history ([`Folio::history`]). Its turns have no footers: the
    /// transcript keeps no per turn cost.
    pub fn replay_transcript(&mut self, jsonl: &str) {
        self.replay(jsonl, false, None);
    }

    fn replay(&mut self, jsonl: &str, sidechain: bool, at: Option<&str>) {
        self.replay_open(jsonl, sidechain, at);
        self.settle_turn();
    }

    /// [`Folio::replay`] with its last turn left as it stands, for a
    /// backlog that may say the turn still runs.
    pub(super) fn replay_open(&mut self, jsonl: &str, sidechain: bool, at: Option<&str>) {
        // How many blocks there were after each line, by its uuid, and the
        // last line's, to see a branch start.
        let mut after: HashMap<String, usize> = HashMap::new();
        let mut last: Option<String> = None;
        for line in jsonl.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if v["isSidechain"] == true && !sidechain {
                continue;
            }
            let before = self.blocks.len();
            match v["type"].as_str().unwrap_or_default() {
                "user" => {
                    self.transcript_user(&v);
                }
                "assistant" => {
                    self.assistant(&v["message"]);
                }
                "system" if v["subtype"] == "compact_boundary" => {
                    self.system(&v, line);
                }
                _ => {}
            }
            let parent = v["parentUuid"].as_str();
            let prompted = matches!(self.blocks.get(before), Some(Block::Prompt(_)));
            if let (true, Some(p)) = (prompted, parent.filter(|p| last.as_deref() != Some(*p))) {
                // Lines other than a rewind's point a few lines back too,
                // with no turn between: only a prompt left behind marks one.
                let from = after.get(p).copied().filter(|&n| {
                    n < before
                        && self.blocks[n..before]
                            .iter()
                            .any(|b| matches!(b, Block::Prompt(_)))
                });
                if let Some(n) = from {
                    self.blocks.drain(n..before);
                }
            }
            if let Some(u) = v["uuid"].as_str() {
                after.insert(u.to_string(), self.blocks.len());
                last = Some(u.to_string());
            }
            if self.session_id.is_none() {
                self.session_id = v["sessionId"].as_str().map(str::to_string);
            }
        }
        if let Some(&n) = at.and_then(|a| after.get(a)) {
            self.blocks.truncate(n);
        }
        self.history = self.blocks.len();
    }

    fn transcript_user(&mut self, v: &Value) {
        if v["isMeta"] == true || v["isCompactSummary"] == true {
            return;
        }
        let content = &v["message"]["content"];
        if let Some(s) = content.as_str() {
            if let Some(out) = command_output(s) {
                self.blocks.push(Block::Notice(Notice::CommandOutput(out)));
            } else if let Some(p) = typed_prompt(s) {
                self.blocks.push(Block::Prompt(p));
            }
            return;
        }
        let Some(parts) = content.as_array() else {
            return;
        };
        if parts.iter().any(|b| b["type"] == "tool_result") {
            self.tool_results(v);
            return;
        }
        let text: Vec<&str> = parts
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect();
        let images: Vec<PromptImage> = parts
            .iter()
            .filter(|b| b["type"] == "image" && b["source"]["type"] == "base64")
            .map(|b| {
                PromptImage::new(&ImageData {
                    media_type: b["source"]["media_type"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    base64: b["source"]["data"].as_str().unwrap_or_default().to_string(),
                })
            })
            .collect();
        let text = text.join("\n");
        // The CLI's own notes in a prompt's place, such as
        // "[Request interrupted by user]".
        if images.is_empty() && (text.starts_with('[') && text.ends_with(']')) {
            return;
        }
        let Some(prompt) = typed_prompt(&text).or_else(|| (!images.is_empty()).then(String::new))
        else {
            return;
        };
        self.blocks.push(Block::Prompt(prompt));
        if !images.is_empty() {
            self.blocks.push(Block::Images(images));
        }
    }
}

/// Where a rewind of a conversation's last turn resumes: the line to
/// carry on from, None when that turn was the first, and the prompt that
/// started it, which `--resume-drops-turn` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewind {
    pub keep: Option<String>,
    pub drop: String,
}

/// The rewind of the last turn of the transcript `jsonl`, counting back
/// from the line `head` when the conversation was resumed there (see
/// [`Folio::from_transcript_at`]), or from its last line. None when no
/// prompt is found, or `head` is not in `jsonl`.
pub fn rewind_point(jsonl: &str, head: Option<&str>) -> Option<Rewind> {
    let mut lines: HashMap<String, Value> = HashMap::new();
    let mut last = None;
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(u) = v["uuid"].as_str().map(str::to_string) else {
            continue;
        };
        if v["isSidechain"] == true {
            continue;
        }
        last = Some(u.clone());
        lines.insert(u, v);
    }
    let mut at = match head {
        Some(h) => h.to_string(),
        None => last?,
    };
    // Bounded, should a chain ever loop.
    for _ in 0..lines.len() {
        let v = lines.get(&at)?;
        let parent = v["parentUuid"].as_str().map(str::to_string);
        if v["type"] == "user" {
            let mut f = Folio::new();
            f.transcript_user(v);
            if matches!(f.blocks.first(), Some(Block::Prompt(_))) {
                return Some(Rewind {
                    keep: parent,
                    drop: at,
                });
            }
        }
        // A compact starts its chain afresh, naming the one before as its
        // logical parent.
        at = parent.or_else(|| v["logicalParentUuid"].as_str().map(str::to_string))?;
    }
    None
}

/// What the human typed, from a transcript's prompt text: a slash command
/// is kept as `/name args` rather than its tags, and the CLI's own wrapped
/// notes are None.
fn typed_prompt(s: &str) -> Option<String> {
    let tag = |name: &str| {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let start = s.find(&open)? + open.len();
        let end = s[start..].find(&close)? + start;
        Some(s[start..end].trim().to_string())
    };
    if let Some(name) = tag("command-name") {
        let args = tag("command-args").unwrap_or_default();
        let name = if name.starts_with('/') {
            name
        } else {
            format!("/{name}")
        };
        return Some(if args.is_empty() {
            name
        } else {
            format!("{name} {args}")
        });
    }
    let t = s.trim();
    if t.is_empty() || t.starts_with("<local-command-caveat>") || t.starts_with("<system-reminder>")
    {
        return None;
    }
    Some(t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slash_command_shows_as_typed() {
        assert_eq!(
            typed_prompt("<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>sonnet</command-args>"),
            Some("/model sonnet".into())
        );
        assert_eq!(
            typed_prompt("<command-name>/clear</command-name><command-args></command-args>"),
            Some("/clear".into())
        );
        assert_eq!(
            typed_prompt("<local-command-caveat>x</local-command-caveat>"),
            None
        );
        assert_eq!(typed_prompt(" hi "), Some("hi".into()));
    }

    fn line(t: &str, uuid: &str, parent: Option<&str>, content: &str) -> String {
        let message = match t {
            "user" => serde_json::json!({"role": "user", "content": content}),
            _ => serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": content}],
            }),
        };
        serde_json::json!({"type": t, "uuid": uuid, "parentUuid": parent, "message": message})
            .to_string()
    }

    fn prompts(f: &Folio) -> Vec<String> {
        (f.blocks.iter())
            .filter_map(|b| match b {
                Block::Prompt(p) => Some(p.clone()),
                _ => None,
            })
            .collect()
    }

    /// Turns A and B, each ending on an attachment as the CLI's do.
    fn two_turns() -> Vec<String> {
        vec![
            line("user", "a", None, "A"),
            line("assistant", "a1", Some("a"), "ok A"),
            serde_json::json!({"type": "attachment", "uuid": "a2", "parentUuid": "a1"})
                .to_string(),
            line("user", "b", Some("a2"), "B"),
            line("assistant", "b1", Some("b"), "ok B"),
        ]
    }

    #[test]
    fn the_last_turn_rewinds_to_the_line_before_its_prompt() {
        let jsonl = two_turns().join("\n");
        let r = rewind_point(&jsonl, None).unwrap();
        assert_eq!((r.keep.as_deref(), r.drop.as_str()), (Some("a2"), "b"));
        // Rewound and not carried on yet: A's turn is the one before.
        let r = rewind_point(&jsonl, Some("a2")).unwrap();
        assert_eq!((r.keep, r.drop.as_str()), (None, "a"));
        let at = Folio::from_transcript_at(&jsonl, Some("a2"));
        assert_eq!(prompts(&at), ["A"]);
    }

    #[test]
    fn a_branch_left_by_a_rewind_is_not_shown() {
        let mut lines = two_turns();
        lines.push(line("user", "c", Some("a2"), "C"));
        lines.push(line("assistant", "c1", Some("c"), "ok C"));
        let jsonl = lines.join("\n");
        assert_eq!(prompts(&Folio::from_transcript(&jsonl)), ["A", "C"]);
        let r = rewind_point(&jsonl, None).unwrap();
        assert_eq!((r.keep.as_deref(), r.drop.as_str()), (Some("a2"), "c"));
    }

    #[test]
    fn a_prompt_skipping_lines_with_no_turn_between_keeps_them() {
        let lines = [
            line("user", "a", None, "A"),
            line("assistant", "a1", Some("a"), "ok A"),
            serde_json::json!({"type": "system", "uuid": "s", "parentUuid": "a1"}).to_string(),
            line("user", "b", Some("a1"), "B"),
        ];
        let f = Folio::from_transcript(&lines.join("\n"));
        assert_eq!(prompts(&f), ["A", "B"]);
    }
}
