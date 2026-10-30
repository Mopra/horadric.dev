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

use serde_json::Value;

use super::{command_output, Block, Folio, ImageData, Notice, PromptImage};

impl Folio {
    /// A Folio holding a transcript's conversation, as if it had streamed.
    pub fn from_transcript(jsonl: &str) -> Folio {
        let mut f = Folio::new();
        f.replay_transcript(jsonl);
        f
    }

    /// A subagent's own transcript as a Folio, its task the first
    /// prompt. Every line of it is marked as a sidechain, which a
    /// session's replay leaves out.
    pub fn from_sidechain(jsonl: &str) -> Folio {
        let mut f = Folio::new();
        f.replay(jsonl, true);
        f
    }

    /// Adds a transcript's conversation to the blocks and marks it as
    /// history ([`Folio::history`]). Its turns have no footers: the
    /// transcript keeps no per turn cost.
    pub fn replay_transcript(&mut self, jsonl: &str) {
        self.replay(jsonl, false);
    }

    fn replay(&mut self, jsonl: &str, sidechain: bool) {
        for line in jsonl.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if v["isSidechain"] == true && !sidechain {
                continue;
            }
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
            if self.session_id.is_none() {
                self.session_id = v["sessionId"].as_str().map(str::to_string);
            }
        }
        self.settle_turn();
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
}
