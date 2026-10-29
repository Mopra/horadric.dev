//! The Folio's model: a headless Claude session's stream-json lines in, the
//! conversation as blocks out. No I/O, so every folding rule is tested
//! against recorded streams. See docs/plan/headless.md.
//!
//! What the CLI sends (2.1.312, recorded in tests/fixtures/headless): with
//! `--include-partial-messages`, each content block streams as
//! `stream_event`s (`content_block_start`, deltas, `content_block_stop`),
//! and the whole block also comes once as an `assistant` message, one block
//! per message, sharing the streamed message's id. That copy usually lands
//! just before the block's `content_block_stop`, and after an interrupt it
//! carries the text cut short. So a streamed block is claimed by its copy,
//! which sets the final text, and a block never streamed (no partial
//! messages, a replay) is added from the copy alone.

use std::collections::HashMap;

use serde_json::Value;

/// One step of tool work inside a [`Block::Tools`] run.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub id: String,
    pub tool: String,
    /// One line saying what the step did: the command, the file, the pattern.
    pub summary: String,
    /// None while it runs.
    pub ok: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Prompt(String),
    /// Grows as token deltas stream in.
    Reply(String),
    Thinking(String),
    /// Consecutive tool steps fold into one run.
    Tools(Vec<Step>),
    /// A `can_use_tool` request waiting on the human. `answered` is None
    /// until [`Folio::answer`] is called.
    Ask {
        request_id: String,
        tool: String,
        summary: String,
        input: serde_json::Value,
        answered: Option<bool>,
    },
    /// The end of a turn, from the `result` message.
    Footer {
        duration_ms: u64,
        cost_usd: f64,
        is_error: bool,
    },
    /// A line we do not understand, kept rather than dropped.
    Raw(String),
}

#[derive(Debug, Default)]
pub struct Folio {
    pub blocks: Vec<Block>,
    pub session_id: Option<String>,
    pub model: Option<String>,
    /// True from a prompt until that turn's `result`.
    pub busy: bool,
    /// The blocks of the message streaming now, by content index.
    open: HashMap<u64, Open>,
    /// The id of the message streaming now.
    message: String,
    /// Blocks that streamed, waiting for their `assistant` copy.
    streamed: Vec<Streamed>,
}

/// A content block between its `content_block_start` and its stop.
#[derive(Debug)]
enum Open {
    /// Index of its Reply in `blocks`.
    Text(usize),
    /// Index of its Thinking, None until a delta with text arrives: the CLI
    /// often streams thinking empty, and an empty block shows nothing.
    Thinking(Option<usize>),
    /// A tool step, its input JSON as it streams.
    Tool { id: String, json: String },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Thinking,
}

#[derive(Debug)]
struct Streamed {
    message: String,
    kind: Kind,
    /// Index in `blocks`, None for thinking that never showed.
    block: Option<usize>,
}

impl Folio {
    pub fn new() -> Self {
        Self::default()
    }

    /// The human sent a prompt.
    pub fn push_prompt(&mut self, text: &str) {
        self.blocks.push(Block::Prompt(text.to_string()));
        self.busy = true;
    }

    /// One stdout line from the CLI. Returns whether anything visible changed.
    pub fn feed(&mut self, line: &str) -> bool {
        let line = line.trim();
        if line.is_empty() {
            return false;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return self.raw(line);
        };
        // A subagent's own messages. Its lane is a later step; until then
        // its stream must not run into the main reply.
        if !v["parent_tool_use_id"].is_null() {
            return false;
        }
        match v["type"].as_str().unwrap_or_default() {
            "stream_event" => self.stream_event(&v["event"]),
            "assistant" => self.assistant(&v["message"]),
            "user" => self.user(&v["message"]),
            "result" => self.result(&v),
            "control_request" => self.control_request(&v, line),
            // The answer to our own interrupt or initialize.
            "control_response" | "rate_limit_event" => false,
            "system" => self.system(&v, line),
            _ => self.raw(line),
        }
    }

    /// Marks an ask answered and returns the `control_response` line to
    /// write to the CLI's stdin, or None if no such ask is pending.
    pub fn answer(&mut self, request_id: &str, allow: bool) -> Option<String> {
        let input = self.blocks.iter_mut().find_map(|b| match b {
            Block::Ask {
                request_id: r,
                input,
                answered: answered @ None,
                ..
            } if r == request_id => {
                *answered = Some(allow);
                Some(input.clone())
            }
            _ => None,
        })?;
        Some(control_response(request_id, allow, input))
    }

    /// The asks still waiting on the human, oldest first, by request id.
    pub fn pending_asks(&self) -> Vec<&str> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Ask {
                    request_id,
                    answered: None,
                    ..
                } => Some(request_id.as_str()),
                _ => None,
            })
            .collect()
    }

    fn raw(&mut self, line: &str) -> bool {
        self.blocks.push(Block::Raw(line.to_string()));
        true
    }

    fn system(&mut self, v: &Value, line: &str) -> bool {
        match v["subtype"].as_str().unwrap_or_default() {
            // Sent again at the start of every turn, not just the first.
            "init" => {
                let id = v["session_id"].as_str().map(str::to_string);
                let model = v["model"].as_str().map(str::to_string);
                let changed = id != self.session_id || model != self.model;
                self.session_id = id.or(self.session_id.take());
                self.model = model.or(self.model.take());
                changed
            }
            // Bookkeeping with nothing to show: hooks are read from the
            // hook events, as for a terminal session.
            "status" | "hook_started" | "hook_progress" | "hook_response" | "thinking_tokens" => {
                false
            }
            _ => self.raw(line),
        }
    }

    fn stream_event(&mut self, e: &Value) -> bool {
        let index = e["index"].as_u64().unwrap_or_default();
        match e["type"].as_str().unwrap_or_default() {
            "message_start" => {
                self.open.clear();
                self.message = e["message"]["id"].as_str().unwrap_or_default().into();
                false
            }
            "content_block_start" => {
                let block = &e["content_block"];
                match block["type"].as_str().unwrap_or_default() {
                    "text" => {
                        self.blocks.push(Block::Reply(text_of(block, "text")));
                        let at = self.blocks.len() - 1;
                        self.open.insert(index, Open::Text(at));
                        self.mark_streamed(Kind::Text, Some(at));
                        true
                    }
                    "thinking" => {
                        let text = text_of(block, "thinking");
                        let at = (!text.is_empty()).then(|| {
                            self.blocks.push(Block::Thinking(text));
                            self.blocks.len() - 1
                        });
                        self.open.insert(index, Open::Thinking(at));
                        self.mark_streamed(Kind::Thinking, at);
                        at.is_some()
                    }
                    "tool_use" => {
                        let id = block["id"].as_str().unwrap_or_default().to_string();
                        let tool = block["name"].as_str().unwrap_or_default();
                        let changed = self.add_step(&id, tool, &block["input"]);
                        self.open.insert(
                            index,
                            Open::Tool {
                                id,
                                json: String::new(),
                            },
                        );
                        changed
                    }
                    _ => false,
                }
            }
            "content_block_delta" => self.delta(index, &e["delta"]),
            "content_block_stop" => match self.open.remove(&index) {
                Some(Open::Tool { id, json }) => match serde_json::from_str::<Value>(&json) {
                    Ok(input) => self.set_step_input(&id, &input),
                    Err(_) => false,
                },
                _ => false,
            },
            _ => false,
        }
    }

    fn delta(&mut self, index: u64, d: &Value) -> bool {
        let Some(open) = self.open.get_mut(&index) else {
            return false;
        };
        match (open, d["type"].as_str().unwrap_or_default()) {
            (Open::Text(at), "text_delta") => {
                let at = *at;
                let piece = d["text"].as_str().unwrap_or_default();
                if let Some(Block::Reply(t)) = self.blocks.get_mut(at) {
                    t.push_str(piece);
                }
                !piece.is_empty()
            }
            (Open::Thinking(at), "thinking_delta") => {
                let piece = d["thinking"].as_str().unwrap_or_default();
                if piece.is_empty() {
                    return false;
                }
                match *at {
                    Some(i) => {
                        if let Some(Block::Thinking(t)) = self.blocks.get_mut(i) {
                            t.push_str(piece);
                        }
                    }
                    None => {
                        self.blocks.push(Block::Thinking(piece.to_string()));
                        let i = self.blocks.len() - 1;
                        *at = Some(i);
                        let message = self.message.clone();
                        if let Some(s) = self.streamed.iter_mut().rev().find(|s| {
                            s.message == message && s.kind == Kind::Thinking && s.block.is_none()
                        }) {
                            s.block = Some(i);
                        }
                    }
                }
                true
            }
            (Open::Tool { json, .. }, "input_json_delta") => {
                json.push_str(d["partial_json"].as_str().unwrap_or_default());
                false
            }
            _ => false,
        }
    }

    fn mark_streamed(&mut self, kind: Kind, block: Option<usize>) {
        self.streamed.push(Streamed {
            message: self.message.clone(),
            kind,
            block,
        });
    }

    /// The whole of one or more content blocks, after (or instead of)
    /// their stream.
    fn assistant(&mut self, m: &Value) -> bool {
        let message = m["id"].as_str().unwrap_or_default().to_string();
        if self.model.is_none() {
            self.model = m["model"].as_str().map(str::to_string);
        }
        let mut changed = false;
        for block in m["content"].as_array().into_iter().flatten() {
            changed |= match block["type"].as_str().unwrap_or_default() {
                "text" => self.settle(&message, Kind::Text, text_of(block, "text")),
                "thinking" => self.settle(&message, Kind::Thinking, text_of(block, "thinking")),
                "tool_use" => {
                    let id = block["id"].as_str().unwrap_or_default();
                    let tool = block["name"].as_str().unwrap_or_default();
                    self.add_step(id, tool, &block["input"])
                }
                _ => false,
            };
        }
        changed
    }

    /// Gives a streamed block its final text, or adds it when it never
    /// streamed.
    fn settle(&mut self, message: &str, kind: Kind, text: String) -> bool {
        let claimed = self
            .streamed
            .iter()
            .position(|s| s.message == message && s.kind == kind);
        if let Some(i) = claimed {
            let s = self.streamed.remove(i);
            return match (s.block.and_then(|b| self.blocks.get_mut(b)), kind) {
                (Some(Block::Reply(t)), Kind::Text)
                | (Some(Block::Thinking(t)), Kind::Thinking)
                    if *t != text =>
                {
                    *t = text;
                    true
                }
                (None, Kind::Thinking) if !text.is_empty() => {
                    self.blocks.push(Block::Thinking(text));
                    true
                }
                _ => false,
            };
        }
        if text.is_empty() {
            return false;
        }
        self.blocks.push(match kind {
            Kind::Text => Block::Reply(text),
            Kind::Thinking => Block::Thinking(text),
        });
        true
    }

    /// Adds a step to the run at the end, or a new run. A step already
    /// there (streamed, then its `assistant` copy) only gets its input.
    fn add_step(&mut self, id: &str, tool: &str, input: &Value) -> bool {
        if self.step_mut(id).is_some() {
            return self.set_step_input(id, input);
        }
        let step = Step {
            id: id.to_string(),
            tool: tool.to_string(),
            summary: summarize(tool, input),
            ok: None,
        };
        match self.blocks.last_mut() {
            Some(Block::Tools(steps)) => steps.push(step),
            _ => self.blocks.push(Block::Tools(vec![step])),
        }
        true
    }

    fn set_step_input(&mut self, id: &str, input: &Value) -> bool {
        let Some(step) = self.step_mut(id) else {
            return false;
        };
        let summary = summarize(&step.tool, input);
        if summary.is_empty() || summary == step.summary {
            return false;
        }
        step.summary = summary;
        true
    }

    fn step_mut(&mut self, id: &str) -> Option<&mut Step> {
        self.blocks.iter_mut().rev().find_map(|b| match b {
            Block::Tools(steps) => steps.iter_mut().find(|s| s.id == id),
            _ => None,
        })
    }

    /// A user message from the CLI carries tool results. Its plain text
    /// ones (such as "[Request interrupted by user]") are the CLI's own
    /// notes, shown by the turn's footer instead.
    fn user(&mut self, m: &Value) -> bool {
        let mut changed = false;
        for block in m["content"].as_array().into_iter().flatten() {
            if block["type"] != "tool_result" {
                continue;
            }
            let id = block["tool_use_id"].as_str().unwrap_or_default();
            let ok = !block["is_error"].as_bool().unwrap_or(false);
            if let Some(step) = self.step_mut(id) {
                step.ok = Some(ok);
                changed = true;
            }
        }
        changed
    }

    fn result(&mut self, v: &Value) -> bool {
        self.blocks.push(Block::Footer {
            duration_ms: v["duration_ms"].as_u64().unwrap_or_default(),
            cost_usd: v["total_cost_usd"].as_f64().unwrap_or_default(),
            is_error: v["is_error"].as_bool().unwrap_or(false),
        });
        if let Some(id) = v["session_id"].as_str() {
            self.session_id = Some(id.to_string());
        }
        self.busy = false;
        self.open.clear();
        self.streamed.clear();
        true
    }

    fn control_request(&mut self, v: &Value, line: &str) -> bool {
        let request = &v["request"];
        if request["subtype"] != "can_use_tool" {
            return self.raw(line);
        }
        let tool = request["tool_name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let input = request["input"].clone();
        self.blocks.push(Block::Ask {
            request_id: v["request_id"].as_str().unwrap_or_default().to_string(),
            summary: summarize(&tool, &input),
            tool,
            input,
            answered: None,
        });
        true
    }
}

/// The `control_response` to a `can_use_tool`. An allow echoes the input
/// back as `updatedInput`, which the CLI requires.
pub fn control_response(request_id: &str, allow: bool, input: Value) -> String {
    let response = if allow {
        serde_json::json!({"behavior": "allow", "updatedInput": input})
    } else {
        serde_json::json!({"behavior": "deny", "message": "The user denied this in Horadric."})
    };
    serde_json::json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id, "response": response},
    })
    .to_string()
}

/// One line saying what a tool call does: a command, a file name, a pattern.
pub fn summarize(tool: &str, input: &Value) -> String {
    let field = |name: &str| input[name].as_str().unwrap_or_default();
    let text = match tool {
        "Bash" | "PowerShell" => field("command"),
        "Read" | "Edit" | "Write" | "MultiEdit" => file_name(field("file_path")),
        "NotebookEdit" => file_name(field("notebook_path")),
        "Grep" | "Glob" => field("pattern"),
        "WebFetch" => field("url"),
        "WebSearch" => field("query"),
        "Task" | "Agent" => field("description"),
        "Skill" => field("skill"),
        _ => "",
    };
    first_line(text)
}

/// The last part of a path, either slash.
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn first_line(text: &str) -> String {
    let mut lines = text.trim().lines();
    let first = lines.next().unwrap_or_default().trim_end();
    if lines.next().is_some() {
        format!("{first} \u{2026}")
    } else {
        first.to_string()
    }
}

fn text_of(block: &Value, field: &str) -> String {
    block[field].as_str().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: &str = include_str!("../tests/fixtures/headless/plain.jsonl");
    const TOOLS: &str = include_str!("../tests/fixtures/headless/tools.jsonl");
    const BASH_ALLOW: &str = include_str!("../tests/fixtures/headless/bash_allow.jsonl");
    const BASH_DENY: &str = include_str!("../tests/fixtures/headless/bash_deny.jsonl");
    const INTERRUPT: &str = include_str!("../tests/fixtures/headless/interrupt.jsonl");

    /// Feeds lines until `stop` says so, returning the rest.
    fn feed_until<'a>(
        f: &mut Folio,
        lines: &mut impl Iterator<Item = &'a str>,
        stop: impl Fn(&Value) -> bool,
    ) {
        for line in lines.by_ref() {
            f.feed(line);
            if serde_json::from_str::<Value>(line).is_ok_and(|v| stop(&v)) {
                return;
            }
        }
    }

    fn play(prompt: &str, stream: &str) -> Folio {
        let mut f = Folio::new();
        f.push_prompt(prompt);
        for line in stream.lines() {
            f.feed(line);
        }
        f
    }

    fn replies(f: &Folio) -> Vec<&str> {
        f.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Reply(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    fn runs(f: &Folio) -> Vec<&Vec<Step>> {
        f.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Tools(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_plain_reply_is_a_prompt_a_reply_and_a_footer() {
        let f = play("Reply with exactly: pong", PLAIN);
        assert_eq!(
            f.session_id.as_deref(),
            Some("66ae460d-6bf3-497a-b900-ca35ea75f5e3")
        );
        assert_eq!(f.model.as_deref(), Some("claude-fable-5-1"));
        assert!(!f.busy);
        assert_eq!(f.blocks.len(), 3, "{:#?}", f.blocks);
        assert_eq!(
            f.blocks[0],
            Block::Prompt("Reply with exactly: pong".into())
        );
        assert_eq!(f.blocks[1], Block::Reply("pong".into()));
        match f.blocks[2] {
            Block::Footer {
                duration_ms,
                cost_usd,
                is_error,
            } => {
                assert_eq!(duration_ms, 2691);
                assert!(cost_usd > 0.0);
                assert!(!is_error);
            }
            ref b => panic!("{b:?}"),
        }
    }

    #[test]
    fn the_reply_grows_as_deltas_stream_in() {
        let mut f = Folio::new();
        f.push_prompt("p");
        let mut lines = TOOLS.lines();
        feed_until(&mut f, &mut lines, |v| {
            v["event"]["delta"]["text"] == " says \"A tiny scrat"
        });
        assert_eq!(replies(&f), ["notes.txt just says \"A tiny scrat"]);
        assert!(f.busy);
    }

    #[test]
    fn tool_steps_fold_into_one_run_with_their_summaries() {
        let f = play("p", TOOLS);
        let runs = runs(&f);
        assert_eq!(runs.len(), 1, "{:#?}", f.blocks);
        let steps = runs[0];
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].tool, "Read");
        assert_eq!(steps[0].summary, "notes.txt");
        assert_eq!(steps[0].ok, Some(true));
        assert_eq!(steps[1].tool, "Grep");
        assert_eq!(steps[1].summary, "println");
        assert_eq!(steps[1].ok, Some(true));
        assert_eq!(
            replies(&f),
            ["notes.txt just says \"A tiny scratch folder.\" and the only println is in `main.rs:2`, printing \"hello\"."]
        );
        // Redacted thinking streams empty and shows nothing.
        assert!(!f.blocks.iter().any(|b| matches!(b, Block::Thinking(_))));
    }

    #[test]
    fn a_step_runs_until_its_result() {
        let mut f = Folio::new();
        f.push_prompt("p");
        let mut lines = TOOLS.lines();
        feed_until(&mut f, &mut lines, |v| {
            v["type"] == "assistant" && v["message"]["content"][0]["name"] == "Read"
        });
        assert_eq!(runs(&f)[0][0].ok, None);
        assert_eq!(runs(&f)[0][0].summary, "notes.txt");
    }

    #[test]
    fn a_bash_ask_is_answered_with_the_input_echoed() {
        let mut f = Folio::new();
        f.push_prompt("p");
        let mut lines = BASH_ALLOW.lines();
        feed_until(&mut f, &mut lines, |v| v["type"] == "control_request");
        let Some(Block::Ask {
            request_id,
            tool,
            summary,
            input,
            answered,
        }) = f.blocks.last().cloned()
        else {
            panic!("{:#?}", f.blocks);
        };
        assert_eq!(tool, "Bash");
        assert_eq!(summary, "echo horadric-test > out.txt");
        assert_eq!(answered, None);
        assert_eq!(f.pending_asks(), [request_id.as_str()]);

        let line = f.answer(&request_id, true).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "control_response");
        assert_eq!(v["response"]["request_id"], request_id.as_str());
        assert_eq!(v["response"]["response"]["behavior"], "allow");
        assert_eq!(v["response"]["response"]["updatedInput"], input);
        assert!(f.answer(&request_id, true).is_none(), "answered once only");
        assert!(f.pending_asks().is_empty());

        for line in lines {
            f.feed(line);
        }
        assert_eq!(runs(&f)[0][0].ok, Some(true));
        assert!(!f.busy);
    }

    #[test]
    fn a_denied_ask_gives_a_message_and_the_step_fails() {
        let mut f = Folio::new();
        f.push_prompt("p");
        let mut lines = BASH_DENY.lines();
        feed_until(&mut f, &mut lines, |v| v["type"] == "control_request");
        let id = f.pending_asks()[0].to_string();
        let v: Value = serde_json::from_str(&f.answer(&id, false).unwrap()).unwrap();
        assert_eq!(v["response"]["response"]["behavior"], "deny");
        assert!(v["response"]["response"]["message"].is_string());
        assert!(v["response"]["response"].get("updatedInput").is_none());
        for line in lines {
            f.feed(line);
        }
        assert_eq!(runs(&f)[0][0].ok, Some(false));
        assert!(matches!(
            f.blocks.iter().find(|b| matches!(b, Block::Ask { .. })),
            Some(Block::Ask {
                answered: Some(false),
                ..
            })
        ));
        assert_eq!(
            replies(&f),
            ["The command was refused, so out2.txt was not written."]
        );
    }

    #[test]
    fn an_unknown_request_is_not_answered() {
        let mut f = Folio::new();
        assert!(f.answer("nope", true).is_none());
    }

    #[test]
    fn an_interrupt_ends_the_turn_in_error_and_the_next_turn_goes_on() {
        let mut f = Folio::new();
        f.push_prompt("Write the numbers");
        let mut lines = INTERRUPT.lines();
        feed_until(&mut f, &mut lines, |v| v["type"] == "result");
        assert!(!f.busy);
        assert!(matches!(
            f.blocks.last(),
            Some(Block::Footer { is_error: true, .. })
        ));
        // Streamed once, then copied whole by the cut-short assistant
        // message: one reply, not two.
        let first = replies(&f);
        assert_eq!(first.len(), 1);
        assert!(first[0].starts_with("one\ntwo\nthree"));
        assert_eq!(first[0].len(), 227);

        f.push_prompt("Reply with exactly: still here");
        for line in lines {
            f.feed(line);
        }
        assert_eq!(replies(&f)[1], "still here");
        assert!(matches!(
            f.blocks.last(),
            Some(Block::Footer {
                is_error: false,
                ..
            })
        ));
        let prompts = f
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Prompt(_)))
            .count();
        assert_eq!(prompts, 2);
    }

    #[test]
    fn without_partial_messages_the_assistant_copy_is_the_reply() {
        let mut f = Folio::new();
        f.push_prompt("p");
        for line in PLAIN.lines().filter(|l| !l.contains("\"stream_event\"")) {
            f.feed(line);
        }
        assert_eq!(replies(&f), ["pong"]);
    }

    #[test]
    fn thinking_with_text_gets_a_block_of_its_own() {
        let mut f = Folio::new();
        f.push_prompt("p");
        let lines = [
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Hmm, "}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"yes."}}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"thinking","thinking":"Hmm, yes."}]}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
        ];
        for l in lines {
            f.feed(l);
        }
        assert_eq!(f.blocks[1], Block::Thinking("Hmm, yes.".into()));
        assert_eq!(f.blocks.len(), 2);
    }

    #[test]
    fn unknown_lines_are_kept_raw() {
        let mut f = Folio::new();
        assert!(f.feed("not json"));
        assert!(f.feed(r#"{"type":"something_new","x":1}"#));
        assert!(f.feed(r#"{"type":"system","subtype":"compact_boundary"}"#));
        assert_eq!(f.blocks.len(), 3);
        assert!(f.blocks.iter().all(|b| matches!(b, Block::Raw(_))));
        assert!(!f.feed(r#"{"type":"system","subtype":"hook_started"}"#));
        assert!(!f.feed(""));
    }

    #[test]
    fn summaries_are_one_line() {
        let s = |tool, input: Value| summarize(tool, &input);
        assert_eq!(
            s(
                "Bash",
                serde_json::json!({"command": "cargo test\ncargo fmt"})
            ),
            "cargo test \u{2026}"
        );
        assert_eq!(
            s(
                "Edit",
                serde_json::json!({"file_path": "C:\\a\\b\\pane.rs"})
            ),
            "pane.rs"
        );
        assert_eq!(
            s("Write", serde_json::json!({"file_path": "/a/b/c.md"})),
            "c.md"
        );
        assert_eq!(
            s("Glob", serde_json::json!({"pattern": "**/*.rs"})),
            "**/*.rs"
        );
        assert_eq!(s("TodoWrite", serde_json::json!({"todos": []})), "");
    }
}
