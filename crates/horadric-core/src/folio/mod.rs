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
//!
//! A subagent's lines carry the id of the Agent (or Task) step that started
//! it as `parent_tool_use_id`, and never stream: they fold into that step's
//! [`Lane`]. AskUserQuestion, ExitPlanMode and TodoWrite are not steps:
//! the first two become [`Block::Question`] and [`Block::Plan`] when the
//! CLI asks, the last is [`Folio::todos`].

mod edits;
mod send;
mod session;
mod tools;
mod transcript;

pub use edits::*;
pub use send::*;
pub use session::*;
pub use tools::*;

use std::collections::{HashMap, HashSet};

use serde_json::Value;

/// One step of tool work inside a [`Block::Tools`] run.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub id: String,
    pub tool: String,
    /// One line saying what the step did: the command, the file, the pattern.
    pub summary: String,
    pub state: StepState,
    /// Its input as last known; `{}` while it streams in.
    pub input: Value,
    /// What an Edit, Write, MultiEdit or NotebookEdit changes.
    pub edit: Option<FileEdit>,
    /// What the subagent an Agent (or Task) step started did.
    pub lane: Option<Lane>,
    /// The end of its result's text, at most [`OUTPUT_TAIL`] bytes: a
    /// shell command's last lines, an error.
    pub output: String,
}

/// How much of a step's result text is kept.
pub const OUTPUT_TAIL: usize = 4000;

/// A subagent's own run, under the step that started it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Lane {
    /// Such as `general-purpose` or `Explore`.
    pub agent_type: String,
    pub steps: Vec<Step>,
    /// What it said last: its report once it is done.
    pub text: String,
    /// What the CLI says it is doing now, from `task_progress`.
    pub progress: String,
    pub tokens: u64,
    pub tool_uses: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Running,
    Done,
    Failed,
    /// Its turn ended before its result came: stopped, or the CLI went.
    Stopped,
}

/// Where an ask stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskState {
    Waiting,
    Allowed,
    Denied,
    /// Its turn ended or the CLI withdrew it before the human answered.
    /// An answer now would be to a question no one is asking.
    NotNeeded,
}

/// How a turn ended, from its `result`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Done,
    /// The human stopped it (Esc), or a prompt sent with
    /// [`Priority::Now`] cut it short. The CLI reports this with a
    /// `terminal_reason` starting "aborted".
    Stopped,
    Failed,
}

/// How the human answers a permission ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply<'a> {
    Allow,
    /// Allow, and add the ask's suggested rules so it is not asked again.
    Always,
    /// Deny, with what Claude should do instead; empty says only no.
    Deny(&'a str),
}

/// Something the CLI says about the session between replies.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// The conversation was summed up to free the context (`/compact`, or
    /// automatic as the window fills).
    Compacted {
        /// `manual` or `auto`.
        trigger: String,
        pre_tokens: u64,
        post_tokens: Option<u64>,
    },
    /// `/clear` started a new conversation; what is above is not in it.
    Cleared,
    /// An API call failed and is tried again; a later retry of the same
    /// call replaces this one.
    Retrying {
        attempt: u64,
        max_retries: u64,
        delay_ms: u64,
        /// The HTTP status, None when the request never got one.
        status: Option<u64>,
        error: String,
    },
    /// What a slash command printed, such as "Set model to sonnet".
    CommandOutput(String),
    /// A control request of ours the CLI refused.
    Refused(String),
    /// A line the CLI wants the human to read, such as a notification, a
    /// warning or a model falling back.
    Info(String),
}

/// A task running in the background of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundTask {
    pub task_id: String,
    /// Such as `local_bash`, `local_agent` or `monitor`.
    pub task_type: String,
    pub description: String,
    /// A local agent's type, such as `Explore`; empty for other tasks.
    pub agent_type: String,
    /// Housekeeping the CLI asks hosts to leave out of anything that
    /// shows activity, such as a watcher it started itself.
    pub ambient: bool,
}

impl BackgroundTask {
    /// Whether it is an agent at work, rather than a shell or monitor that
    /// may sit there all day.
    pub fn is_agent(&self) -> bool {
        !self.ambient
            && matches!(
                self.task_type.as_str(),
                "local_agent" | "remote_agent" | "in_process_teammate" | "local_workflow"
            )
    }
}

/// An image sent with a prompt, for its thumbnail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptImage {
    pub media_type: String,
    /// The image itself in base64, for drawing the thumbnail.
    pub base64: String,
    /// Read from a PNG's or GIF's header; None for the other formats.
    pub size: Option<(u32, u32)>,
}

impl PromptImage {
    pub fn new(image: &ImageData) -> PromptImage {
        PromptImage {
            media_type: image.media_type.clone(),
            base64: image.base64.clone(),
            size: image_size(&image.base64),
        }
    }
}

/// Width and height from the first bytes of a PNG or GIF.
fn image_size(base64: &str) -> Option<(u32, u32)> {
    let head: String = base64.chars().take(44).collect();
    let bytes = crate::release::base64_decode(&head)?;
    let be = |i: usize| {
        let b: [u8; 4] = bytes.get(i..i + 4)?.try_into().ok()?;
        Some(u32::from_be_bytes(b))
    };
    let le = |i: usize| Some(u16::from_le_bytes(bytes.get(i..i + 2)?.try_into().ok()?) as u32);
    match bytes.as_slice() {
        [0x89, b'P', b'N', b'G', ..] => Some((be(16)?, be(20)?)),
        [b'G', b'I', b'F', ..] => Some((le(6)?, le(8)?)),
        _ => None,
    }
}

/// A prompt sent while Claude works, waiting for its turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedPrompt {
    /// The uuid it was sent with; the CLI names it in `command_lifecycle`.
    pub uuid: String,
    pub text: String,
    pub images: Vec<ImageData>,
    pub priority: Priority,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Prompt(String),
    /// The images sent with the [`Block::Prompt`] just before.
    Images(Vec<PromptImage>),
    /// Grows as token deltas stream in.
    Reply(String),
    Thinking(String),
    /// Consecutive tool steps fold into one run.
    Tools(Vec<Step>),
    /// A `can_use_tool` request, waiting on the human until
    /// [`Folio::answer`] is called.
    Ask {
        request_id: String,
        tool: String,
        summary: String,
        /// What the card shows of what Claude wants to do, made once
        /// rather than every frame.
        shown: String,
        input: serde_json::Value,
        /// The request's `permission_suggestions`: the rules an "always"
        /// answer adds. Null when it has none.
        suggestions: serde_json::Value,
        state: AskState,
    },
    /// AskUserQuestion, waiting until [`Folio::answer_question`].
    Question {
        /// Empty when the CLI never asked (a transcript, an answer that
        /// came another way).
        request_id: String,
        tool_use_id: String,
        questions: Vec<Question>,
        /// Per question, what was answered, several picks joined by ", ".
        /// Empty until answered.
        answers: Vec<String>,
        state: AskState,
    },
    /// ExitPlanMode: a plan to approve with [`Folio::approve_plan`] or
    /// send back with [`Folio::keep_planning`].
    Plan {
        request_id: String,
        tool_use_id: String,
        /// Markdown.
        plan: String,
        /// Allowed is approved, Denied is sent back.
        state: AskState,
        /// What the human said when sending it back.
        feedback: String,
    },
    /// The end of a turn, from the `result` message.
    Footer {
        duration_ms: u64,
        ending: Ending,
        usage: TurnUsage,
    },
    Notice(Notice),
    /// A line we do not understand, kept rather than dropped.
    Raw(String),
}

/// A control request we sent, waiting for its response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sent {
    Initialize,
    Mode,
    Model,
    Context,
}

#[derive(Debug, Default)]
pub struct Folio {
    pub blocks: Vec<Block>,
    pub session_id: Option<String>,
    pub model: Option<String>,
    /// True from a prompt until that turn's `result`.
    pub busy: bool,
    /// What `system/init` and `initialize` said, latest wins.
    pub info: SessionInfo,
    /// The todo list as TodoWrite last wrote it.
    pub todos: Vec<Todo>,
    /// The context window, from the last result or `get_context_usage`.
    pub context: Option<ContextUsage>,
    /// What the CLI says it is doing, such as `requesting` or `compacting`;
    /// None when it says nothing.
    pub status: Option<String>,
    /// Prompts sent while busy that have not started yet, oldest first.
    pub queued: Vec<QueuedPrompt>,
    /// Prompts the CLI dropped unstarted when the turn was stopped, for the
    /// composer to take back so nothing typed is lost.
    pub dropped: Vec<QueuedPrompt>,
    /// How many blocks came from a transcript, before this process's own.
    pub history: usize,
    /// The background tasks alive now (shells, monitors, agents), as the
    /// CLI's last `background_tasks_changed` listed them.
    pub background: Vec<BackgroundTask>,
    /// Whether the CLI has listed its background tasks at all, so an empty
    /// list means none run rather than a CLI that never says.
    pub background_heard: bool,
    /// The blocks of the message streaming now, by content index.
    open: HashMap<u64, Open>,
    /// The id of the message streaming now.
    message: String,
    /// Blocks that streamed, waiting for their `assistant` copy.
    streamed: Vec<Streamed>,
    /// Lines the CLI must be sent back, such as the answer to a request
    /// it made that the human has no say in, by the request id it answers.
    /// Taken by [`Folio::take_outbox`].
    outbox: Vec<(String, String)>,
    /// Whether `initialize` went to this process, sent here or replayed.
    pub initialized: bool,
    /// The input of each AskUserQuestion and ExitPlanMode ask, by request
    /// id, for the answer to echo back whether or not it named its tool use.
    ask_inputs: HashMap<String, Value>,
    /// The tool use each ask was about, by request id.
    asked: HashMap<String, String>,
    /// Tool uses whose ask the CLI withdrew. Their result comes back as a
    /// refusal, which is the turn being stopped, not the step failing.
    withdrawn: HashSet<String>,
    /// Tool uses shown as something other than a step, with their input.
    hidden: HashMap<String, (String, Value)>,
    /// Our control requests waiting for their response, by request id.
    requests: HashMap<String, Sent>,
    /// For request ids and queued prompts' uuids.
    sent: u64,
}

/// A content block between its `content_block_start` and its stop.
#[derive(Debug)]
enum Open {
    /// Index of its Reply in `blocks`.
    Text(usize),
    /// Index of its Thinking, None until a delta with text arrives: the CLI
    /// often streams thinking empty, and an empty block shows nothing.
    Thinking(Option<usize>),
    /// A tool call, its input JSON as it streams.
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
    /// Its content index in that message.
    index: u64,
    kind: Kind,
    /// Index in `blocks`, None for thinking that never showed.
    block: Option<usize>,
}

/// Tools shown as something other than a step.
fn hidden_tool(tool: &str) -> bool {
    matches!(tool, "AskUserQuestion" | "ExitPlanMode" | "TodoWrite")
}

/// The text of a tool result's content, a string or text blocks.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The last `max` bytes of `s`, cut at a char boundary.
fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut at = s.len() - max;
    while !s.is_char_boundary(at) {
        at += 1;
    }
    s[at..].to_string()
}

fn find_step<'a>(steps: &'a mut [Step], id: &str) -> Option<&'a mut Step> {
    for s in steps.iter_mut() {
        if s.id == id {
            return Some(s);
        }
        if let Some(lane) = s.lane.as_mut() {
            if let Some(found) = find_step(&mut lane.steps, id) {
                return Some(found);
            }
        }
    }
    None
}

fn new_step(id: &str, tool: &str, input: &Value) -> Step {
    Step {
        id: id.to_string(),
        tool: tool.to_string(),
        summary: summarize(tool, input),
        state: StepState::Running,
        input: input.clone(),
        edit: FileEdit::from_input(tool, input),
        lane: matches!(tool, "Agent" | "Task").then(|| Lane {
            agent_type: input["subagent_type"].as_str().unwrap_or_default().into(),
            ..Lane::default()
        }),
        output: String::new(),
    }
}

/// Gives a step its whole input. Returns whether what shows changed.
fn set_input(step: &mut Step, input: &Value) -> bool {
    if input.as_object().is_none_or(|o| o.is_empty()) || step.input == *input {
        return false;
    }
    step.input = input.clone();
    step.summary = summarize(&step.tool, input);
    if let Some(edit) = FileEdit::from_input(&step.tool, input) {
        step.edit = Some(edit);
    }
    if let Some(lane) = step.lane.as_mut() {
        if let Some(t) = input["subagent_type"].as_str() {
            lane.agent_type = t.to_string();
        }
    }
    true
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

    /// The human sent a prompt with images. Returns the line to write.
    pub fn push_prompt_with_images(&mut self, text: &str, images: &[ImageData]) -> String {
        self.push_prompt(text);
        if !images.is_empty() {
            self.blocks
                .push(Block::Images(images.iter().map(PromptImage::new).collect()));
        }
        prompt_line_with_images(text, images)
    }

    /// The human sent a prompt, maybe while Claude works. Returns the line
    /// to write. While busy it waits in [`Folio::queued`] until the CLI
    /// takes it up: [`Priority::Next`] folded into the turn running now at
    /// its next step, [`Priority::Now`] cutting that turn short (it ends
    /// Stopped).
    pub fn queue_prompt(&mut self, text: &str, images: &[ImageData], priority: Priority) -> String {
        if !self.busy {
            return self.push_prompt_with_images(text, images);
        }
        let uuid = self.next_uuid();
        let line = queued_prompt_line(text, images, &uuid, priority);
        self.queued.push(QueuedPrompt {
            uuid,
            text: text.to_string(),
            images: images.to_vec(),
            priority,
        });
        line
    }

    /// The permission mode the CLI is in now, such as `default`, `plan`,
    /// `acceptEdits`, `auto`. Empty before init.
    pub fn permission_mode(&self) -> &str {
        &self.info.permission_mode
    }

    /// The `/` list for what the human typed.
    pub fn slash_commands(&self, typed: &str) -> Vec<SlashCommand> {
        self.info.slash_commands_for(typed)
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
        if let Some(parent) = v["parent_tool_use_id"].as_str() {
            return self.subagent(parent, &v);
        }
        match v["type"].as_str().unwrap_or_default() {
            "stream_event" => self.stream_event(&v["event"]),
            "assistant" => self.assistant(&v["message"]),
            "user" => self.user(&v),
            "result" => self.result(&v),
            "control_request" => self.control_request(&v, line),
            "control_cancel_request" => self.cancel(v["request_id"].as_str().unwrap_or_default()),
            "control_response" => self.control_response(&v),
            "command_lifecycle" => self.lifecycle(&v),
            "conversation_reset" => {
                self.todos.clear();
                self.notice(Notice::Cleared)
            }
            "rate_limit_event" => false,
            "system" => self.system(&v, line),
            _ => self.raw(line),
        }
    }

    /// One line that was written to the CLI's stdin, folded back in. A host
    /// replaying a session to a UI that attaches sends these between the
    /// stdout lines, in the order they went, since the CLI echoes neither
    /// prompts nor answers: a prompt becomes its block (queued if it went
    /// with a priority while a turn ran), an answer settles its ask,
    /// question or plan, and a control request of ours waits for its
    /// response as if this Folio had sent it. Returns whether anything
    /// visible changed.
    pub fn feed_sent(&mut self, line: &str) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            return false;
        };
        match v["type"].as_str().unwrap_or_default() {
            "user" => self.sent_prompt(&v),
            "control_response" => {
                // Answered already, so a replay must not answer it again.
                let id = v["response"]["request_id"].as_str().unwrap_or_default();
                self.outbox.retain(|(r, _)| r != id);
                self.sent_answer(&v["response"])
            }
            "control_request" => {
                let id = v["request_id"].as_str().unwrap_or_default();
                // Our ids count up; a later request must not reuse one.
                if let Some(n) = id.strip_prefix("horadric-").and_then(|n| n.parse().ok()) {
                    self.sent = self.sent.max(n);
                }
                let kind = match v["request"]["subtype"].as_str().unwrap_or_default() {
                    "initialize" => {
                        self.initialized = true;
                        Sent::Initialize
                    }
                    "set_permission_mode" => Sent::Mode,
                    "set_model" => Sent::Model,
                    "get_context_usage" => Sent::Context,
                    _ => return false,
                };
                self.requests.insert(id.to_string(), kind);
                false
            }
            _ => false,
        }
    }

    fn sent_prompt(&mut self, v: &Value) -> bool {
        let content = &v["message"]["content"];
        let mut text = content.as_str().unwrap_or_default().to_string();
        let mut images = Vec::new();
        for b in content.as_array().into_iter().flatten() {
            match b["type"].as_str().unwrap_or_default() {
                "text" => text.push_str(b["text"].as_str().unwrap_or_default()),
                "image" => images.push(ImageData {
                    media_type: b["source"]["media_type"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    base64: b["source"]["data"].as_str().unwrap_or_default().to_string(),
                }),
                _ => {}
            }
        }
        let priority = match v["priority"].as_str() {
            Some("now") => Some(Priority::Now),
            Some("next") => Some(Priority::Next),
            Some("later") => Some(Priority::Later),
            _ => None,
        };
        if let (true, Some(priority), Some(uuid)) = (self.busy, priority, v["uuid"].as_str()) {
            self.queued.push(QueuedPrompt {
                uuid: uuid.to_string(),
                text,
                images,
                priority,
            });
            return true;
        }
        self.push_prompt_with_images(&text, &images);
        true
    }

    fn sent_answer(&mut self, r: &Value) -> bool {
        let id = r["request_id"].as_str().unwrap_or_default();
        let body = &r["response"];
        let allow = body["behavior"] == "allow";
        let to = if allow {
            AskState::Allowed
        } else {
            AskState::Denied
        };
        self.blocks.iter_mut().any(|b| match b {
            Block::Ask {
                request_id, state, ..
            } if request_id == id && *state == AskState::Waiting => {
                *state = to;
                true
            }
            Block::Question {
                request_id,
                questions,
                answers,
                state,
                ..
            } if request_id == id && *state == AskState::Waiting => {
                *state = to;
                *answers = questions
                    .iter()
                    .map(|q| {
                        body["updatedInput"]["answers"][&q.question]
                            .as_str()
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect();
                true
            }
            Block::Plan {
                request_id,
                state,
                feedback,
                ..
            } if request_id == id && *state == AskState::Waiting => {
                *state = to;
                if !allow {
                    *feedback = body["message"].as_str().unwrap_or_default().to_string();
                }
                true
            }
            _ => false,
        })
    }

    /// Marks an ask answered and returns the `control_response` line to
    /// write to the CLI's stdin, or None if no such ask is pending.
    pub fn answer(&mut self, request_id: &str, allow: bool) -> Option<String> {
        let reply = if allow { Reply::Allow } else { Reply::Deny("") };
        self.reply(request_id, reply)
    }

    /// Answers an ask as the human chose: once, always (adding the rules
    /// its `permission_suggestions` name), or no with what to do instead.
    /// None if no such ask is pending.
    pub fn reply(&mut self, request_id: &str, reply: Reply) -> Option<String> {
        let allow = !matches!(reply, Reply::Deny(_));
        let (input, suggestions) = self.blocks.iter_mut().find_map(|b| match b {
            Block::Ask {
                request_id: r,
                input,
                suggestions,
                state,
                ..
            } if r == request_id && *state == AskState::Waiting => {
                *state = if allow {
                    AskState::Allowed
                } else {
                    AskState::Denied
                };
                Some((input.clone(), suggestions.clone()))
            }
            _ => None,
        })?;
        Some(match reply {
            Reply::Allow => control_response(request_id, true, input),
            Reply::Always => {
                let mut response = serde_json::json!({"behavior": "allow", "updatedInput": input});
                if suggestions.as_array().is_some_and(|a| !a.is_empty()) {
                    response["updatedPermissions"] = suggestions;
                }
                control_success(request_id, response)
            }
            Reply::Deny(reason) if !reason.trim().is_empty() => control_success(
                request_id,
                serde_json::json!({"behavior": "deny", "message": reason.trim()}),
            ),
            Reply::Deny(_) => control_response(request_id, false, input),
        })
    }

    /// Answers an AskUserQuestion: per question, in order, the labels
    /// picked (or the human's own words). Returns the line to write, or
    /// None if no such question waits.
    pub fn answer_question(&mut self, request_id: &str, picks: &[Vec<String>]) -> Option<String> {
        let (input, questions) = self
            .blocks
            .iter_mut()
            .find_map(|b| match b {
                Block::Question {
                    request_id: r,
                    questions,
                    answers,
                    state,
                    ..
                } if r == request_id && *state == AskState::Waiting => {
                    *state = AskState::Allowed;
                    *answers = picks.iter().map(|p| p.join(", ")).collect();
                    Some(questions.clone())
                }
                _ => None,
            })
            .map(|q| (self.input_of_request(request_id), q))?;
        let mut input = input.unwrap_or_else(|| serde_json::json!({}));
        input["answers"] = answers_object(&questions, picks);
        Some(control_response(request_id, true, input))
    }

    /// Approves a plan. The CLI leaves plan mode by itself, to `default`,
    /// or to `mode` (such as `acceptEdits`) when given. Returns the line
    /// to write, or None if no such plan waits.
    pub fn approve_plan(&mut self, request_id: &str, mode: Option<&str>) -> Option<String> {
        self.settle_plan(request_id, AskState::Allowed, "")?;
        let input = self
            .input_of_request(request_id)
            .unwrap_or_else(|| serde_json::json!({}));
        let mut response = serde_json::json!({"behavior": "allow", "updatedInput": input});
        if let Some(m) = mode {
            response["updatedPermissions"] =
                serde_json::json!([{"type": "setMode", "mode": m, "destination": "session"}]);
        }
        Some(control_success(request_id, response))
    }

    /// Sends a plan back with what to change. Claude stays in plan mode.
    pub fn keep_planning(&mut self, request_id: &str, feedback: &str) -> Option<String> {
        self.settle_plan(request_id, AskState::Denied, feedback)?;
        let message = if feedback.trim().is_empty() {
            "The user wants you to keep planning."
        } else {
            feedback
        };
        Some(control_success(
            request_id,
            serde_json::json!({"behavior": "deny", "message": message}),
        ))
    }

    fn settle_plan(&mut self, request_id: &str, to: AskState, said: &str) -> Option<()> {
        self.blocks.iter_mut().find_map(|b| match b {
            Block::Plan {
                request_id: r,
                state,
                feedback,
                ..
            } if r == request_id && *state == AskState::Waiting => {
                *state = to;
                *feedback = said.to_string();
                Some(())
            }
            _ => None,
        })
    }

    /// The input of the tool use a request asked about.
    fn input_of_request(&mut self, request_id: &str) -> Option<Value> {
        if let Some(input) = self.ask_inputs.remove(request_id) {
            return Some(input);
        }
        let tool_use = self.asked.get(request_id)?;
        self.hidden.get(tool_use).map(|(_, input)| input.clone())
    }

    /// Whether a turn ran here and ended with nothing asked since: the
    /// last prompt has its footer. A transcript's turns do not count, as
    /// they ended before this process.
    pub fn turn_finished(&self) -> bool {
        !self.busy && self.turn_over()
    }

    /// Whether the last prompt of this process has its footer.
    fn turn_over(&self) -> bool {
        let own = self.blocks.get(self.history..).unwrap_or_default();
        let last =
            (own.iter().rev()).find(|b| matches!(b, Block::Prompt(_) | Block::Footer { .. }));
        matches!(last, Some(Block::Footer { .. }))
    }

    /// The asks, questions and plans still waiting on the human, oldest
    /// first, by request id.
    pub fn pending_asks(&self) -> Vec<&str> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Ask {
                    request_id,
                    state: AskState::Waiting,
                    ..
                }
                | Block::Question {
                    request_id,
                    state: AskState::Waiting,
                    ..
                }
                | Block::Plan {
                    request_id,
                    state: AskState::Waiting,
                    ..
                } if !request_id.is_empty() => Some(request_id.as_str()),
                _ => None,
            })
            .collect()
    }

    /// `set_permission_mode`: the line to write. The answer sets
    /// [`Folio::permission_mode`].
    pub fn set_permission_mode(&mut self, mode: &str) -> String {
        let id = self.request(Sent::Mode);
        control_request_line(
            &id,
            "set_permission_mode",
            serde_json::json!({"mode": mode}),
        )
    }

    /// `set_model`, such as `sonnet`, `opus` or `default`: the line to
    /// write. The answer sets [`Folio::model`] to the model id it became.
    pub fn set_model(&mut self, model: &str) -> String {
        let id = self.request(Sent::Model);
        control_request_line(&id, "set_model", serde_json::json!({"model": model}))
    }

    /// `get_context_usage`: the line to write. The answer sets
    /// [`Folio::context`], with what fills it.
    pub fn get_context_usage(&mut self) -> String {
        let id = self.request(Sent::Context);
        control_request_line(&id, "get_context_usage", serde_json::json!({}))
    }

    /// `initialize`: the line to write. The answer fills
    /// [`SessionInfo::commands`] with descriptions for the `/` list, and
    /// [`SessionInfo::models`] with each model's effort levels. Optional:
    /// the CLI works without it.
    pub fn initialize(&mut self) -> String {
        self.initialized = true;
        let id = self.request(Sent::Initialize);
        control_request_line(&id, "initialize", serde_json::json!({}))
    }

    fn request(&mut self, kind: Sent) -> String {
        self.sent += 1;
        let id = format!("horadric-{}", self.sent);
        self.requests.insert(id.clone(), kind);
        id
    }

    /// A version 4 shaped uuid, unique within this process.
    fn next_uuid(&mut self) -> String {
        self.sent += 1;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or_default();
        let mix = |mut z: u64| {
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        let a = mix(nanos ^ self.sent.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let b = mix(a ^ std::process::id() as u64);
        format!(
            "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
            a >> 32,
            (a >> 16) & 0xffff,
            a & 0xfff,
            0x8000 | ((b >> 48) & 0x3fff),
            b & 0xffff_ffff_ffff
        )
    }

    fn raw(&mut self, line: &str) -> bool {
        self.blocks.push(Block::Raw(line.to_string()));
        true
    }

    /// What a slash command printed. The CLI may say it twice, as a
    /// `local_command_output` and in a user message, so it shows once.
    fn command_notice(&mut self, out: String) -> bool {
        let n = Notice::CommandOutput(out);
        if (self.blocks.iter().rev().take(3)).any(|b| matches!(b, Block::Notice(m) if *m == n)) {
            return false;
        }
        self.notice(n)
    }

    fn notice(&mut self, n: Notice) -> bool {
        self.blocks.push(Block::Notice(n));
        true
    }

    fn system(&mut self, v: &Value, line: &str) -> bool {
        match v["subtype"].as_str().unwrap_or_default() {
            // Sent again at the start of every turn, not just the first.
            "init" => {
                let id = v["session_id"].as_str().map(str::to_string);
                let model = v["model"].as_str().map(str::to_string);
                let mode = self.info.permission_mode.clone();
                self.info.read_init(v);
                let changed = id != self.session_id
                    || model != self.model
                    || mode != self.info.permission_mode;
                self.session_id = id.or(self.session_id.take());
                self.model = model.or(self.model.take());
                changed
            }
            "status" => {
                let status = v["status"].as_str().map(str::to_string);
                let mut changed = status != self.status;
                self.status = status;
                if let Some(m) = v["permissionMode"].as_str() {
                    changed |= m != self.info.permission_mode;
                    self.info.permission_mode = m.to_string();
                }
                changed
            }
            "compact_boundary" => {
                let m = &v["compact_metadata"];
                self.notice(Notice::Compacted {
                    trigger: m["trigger"].as_str().unwrap_or_default().to_string(),
                    pre_tokens: m["pre_tokens"].as_u64().unwrap_or_default(),
                    post_tokens: m["post_tokens"].as_u64(),
                })
            }
            "api_retry" => {
                let n = Notice::Retrying {
                    attempt: v["attempt"].as_u64().unwrap_or_default(),
                    max_retries: v["max_retries"].as_u64().unwrap_or_default(),
                    delay_ms: v["retry_delay_ms"].as_u64().unwrap_or_default(),
                    status: v["error_status"].as_u64(),
                    error: v["error"].as_str().unwrap_or_default().to_string(),
                };
                if let Some(Block::Notice(last @ Notice::Retrying { .. })) = self.blocks.last_mut()
                {
                    *last = n;
                    return true;
                }
                self.notice(n)
            }
            "task_started" | "task_progress" => {
                let tool_use = v["tool_use_id"].as_str().unwrap_or_default();
                let Some(lane) = self.lane_mut(tool_use) else {
                    return false;
                };
                if let Some(t) = v["subagent_type"].as_str() {
                    lane.agent_type = t.to_string();
                }
                if let Some(d) = v["description"].as_str() {
                    lane.progress = d.to_string();
                }
                let u = &v["usage"];
                lane.tokens = u["total_tokens"].as_u64().unwrap_or(lane.tokens);
                lane.tool_uses = u["tool_uses"].as_u64().unwrap_or(lane.tool_uses);
                true
            }
            // The whole set each time, so a missed line heals at the next.
            "background_tasks_changed" => {
                let tasks: Vec<BackgroundTask> = (v["tasks"].as_array().into_iter().flatten())
                    .map(|t| BackgroundTask {
                        task_id: t["task_id"].as_str().unwrap_or_default().to_string(),
                        task_type: t["task_type"].as_str().unwrap_or_default().to_string(),
                        description: t["description"].as_str().unwrap_or_default().to_string(),
                        agent_type: t["subagent_type"].as_str().unwrap_or_default().to_string(),
                        ambient: t["ambient"].as_bool().unwrap_or_default(),
                    })
                    .collect();
                let changed = tasks != self.background;
                self.background = tasks;
                self.background_heard = true;
                changed
            }
            // A finished task leaves the set by the next
            // background_tasks_changed; dropping it here as well means one
            // lost line cannot keep it running.
            "task_notification" => {
                let id = v["task_id"].as_str().unwrap_or_default();
                let before = self.background.len();
                self.background.retain(|t| t.task_id != id);
                before != self.background.len()
            }
            "commands_changed" => {
                self.info.read_commands(&v["commands"]);
                false
            }
            "local_command_output" => match said(&v["content"]) {
                Some(t) => self.command_notice(t),
                None => false,
            },
            // Level `info` is for the CLI's verbose transcript view only.
            "informational" if v["level"] == "info" => false,
            "notification" => match said(&v["text"]) {
                Some(t) => self.notice(Notice::Info(t)),
                None => false,
            },
            "informational" | "model_fallback" | "model_refusal_no_fallback" => {
                match said(&v["content"]) {
                    Some(t) => self.notice(Notice::Info(t)),
                    None => false,
                }
            }
            // Bookkeeping with nothing to show: hooks are read from the
            // hook events, as for a terminal session, a subagent's end
            // shows in its step's result, a turn's end in its result, and
            // the rest is for other hosts (IDEs, remote control, the
            // CLI's own screen).
            "hook_started"
            | "hook_progress"
            | "hook_response"
            | "thinking_tokens"
            | "task_updated"
            | "task_summary"
            | "stop_hook_summary"
            | "session_state_changed"
            | "session_title_changed"
            | "session_metadata"
            | "turn_duration"
            | "post_turn_summary"
            | "api_error"
            | "permission_denied"
            | "permission_retry"
            | "permission_check_status"
            | "per_turn_effort_changed"
            | "files_persisted"
            | "file_snapshot"
            | "file_attachments_missing"
            | "elicitation_complete"
            | "control_request_progress"
            | "memory_recall"
            | "memory_saved"
            | "instruction_size_warning"
            | "away_summary"
            | "thinking"
            | "vcs_state_changed"
            | "turn_handoff_available"
            | "turn_preempted"
            | "inherited_asks_settled"
            | "worker_shutting_down"
            | "agents_killed"
            | "cloud_session_delta"
            | "code_change_published"
            | "dev_intent"
            | "feedback_draft_queued"
            | "mirror_error"
            | "model_consent_fallback"
            | "peer_message_hold"
            | "plugin_install"
            | "scheduled_task_fire"
            | "upgrade_relay_marker" => false,
            s if s.starts_with("ui_") => false,
            _ => self.raw(line),
        }
    }

    /// A queued prompt's way through the CLI.
    fn lifecycle(&mut self, v: &Value) -> bool {
        if v["state"] != "started" {
            return false;
        }
        let uuid = v["command_uuid"].as_str().unwrap_or_default();
        let Some(i) = self.queued.iter().position(|q| q.uuid == uuid) else {
            return false;
        };
        let q = self.queued.remove(i);
        self.push_prompt_with_images(&q.text, &q.images);
        true
    }

    fn control_response(&mut self, v: &Value) -> bool {
        let r = &v["response"];
        let id = r["request_id"].as_str().unwrap_or_default();
        // An interrupt's answer names what still waits; anything else was
        // dropped and will never start, so it must not keep the turn busy.
        if let Some(still) = r["response"]["still_queued"].as_array() {
            let still = serde_json::to_string(still).unwrap_or_default();
            let before = self.queued.len();
            let (kept, dropped) = std::mem::take(&mut self.queued)
                .into_iter()
                .partition(|q| still.contains(&q.uuid));
            self.queued = kept;
            self.dropped.extend::<Vec<_>>(dropped);
            if self.queued.len() != before && self.queued.is_empty() && self.turn_over() {
                self.busy = false;
            }
        }
        // The answer to an interrupt, or to a request not ours.
        let Some(kind) = self.requests.remove(id) else {
            return false;
        };
        if r["subtype"] == "error" {
            let error = r["error"].as_str().unwrap_or("refused");
            return self.notice(Notice::Refused(error.to_string()));
        }
        let body = &r["response"];
        match kind {
            Sent::Mode => {
                let Some(m) = body["mode"].as_str() else {
                    return false;
                };
                self.info.permission_mode = m.to_string();
            }
            Sent::Model => {
                let Some(m) = body["model"].as_str() else {
                    return false;
                };
                self.model = Some(m.to_string());
            }
            Sent::Context => self.context = Some(ContextUsage::from_response(body)),
            Sent::Initialize => self.info.read_initialize(body),
        }
        true
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
                        self.mark_streamed(index, Kind::Text, Some(at));
                        true
                    }
                    "thinking" => {
                        let text = text_of(block, "thinking");
                        let at = (!text.is_empty()).then(|| {
                            self.blocks.push(Block::Thinking(text));
                            self.blocks.len() - 1
                        });
                        self.open.insert(index, Open::Thinking(at));
                        self.mark_streamed(index, Kind::Thinking, at);
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

    fn mark_streamed(&mut self, index: u64, kind: Kind, block: Option<usize>) {
        self.streamed.push(Streamed {
            message: self.message.clone(),
            index,
            kind,
            block,
        });
    }

    /// The whole of one or more content blocks, after (or instead of)
    /// their stream.
    fn assistant(&mut self, m: &Value) -> bool {
        let message = m["id"].as_str().unwrap_or_default().to_string();
        // A slash command's own answer comes from "<synthetic>".
        if self.model.is_none() {
            self.model = m["model"]
                .as_str()
                .filter(|s| !s.starts_with('<'))
                .map(str::to_string);
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
            // The copy is the block's final text. A delta arriving after it
            // would add to text already whole, or start a second thinking.
            if s.message == self.message {
                self.open.remove(&s.index);
            }
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
    /// The tools shown otherwise are kept aside.
    fn add_step(&mut self, id: &str, tool: &str, input: &Value) -> bool {
        if hidden_tool(tool) {
            self.hidden
                .insert(id.to_string(), (tool.to_string(), input.clone()));
            return self.hidden_input(id, input);
        }
        if self.step_mut(id).is_some() {
            return self.set_step_input(id, input);
        }
        let step = new_step(id, tool, input);
        match self.blocks.last_mut() {
            Some(Block::Tools(steps)) => steps.push(step),
            _ => self.blocks.push(Block::Tools(vec![step])),
        }
        true
    }

    fn set_step_input(&mut self, id: &str, input: &Value) -> bool {
        if self.hidden.contains_key(id) {
            return self.hidden_input(id, input);
        }
        self.step_mut(id).is_some_and(|step| set_input(step, input))
    }

    /// A hidden tool's input, whole or not yet. TodoWrite's is the list.
    fn hidden_input(&mut self, id: &str, input: &Value) -> bool {
        let Some((tool, kept)) = self.hidden.get_mut(id) else {
            return false;
        };
        if input.as_object().is_some_and(|o| !o.is_empty()) {
            *kept = input.clone();
        }
        if tool != "TodoWrite" {
            return false;
        }
        match todos(kept) {
            Some(list) if list != self.todos => {
                self.todos = list;
                true
            }
            _ => false,
        }
    }

    fn step_mut(&mut self, id: &str) -> Option<&mut Step> {
        self.blocks.iter_mut().rev().find_map(|b| match b {
            Block::Tools(steps) => find_step(steps, id),
            _ => None,
        })
    }

    /// The lane of the Agent step `tool_use`, made if it has none.
    fn lane_mut(&mut self, tool_use: &str) -> Option<&mut Lane> {
        let step = self.step_mut(tool_use)?;
        Some(step.lane.get_or_insert_with(Lane::default))
    }

    /// A line of a subagent's own: its words and steps go to its lane.
    /// Its lines do not stream, so each comes whole.
    fn subagent(&mut self, parent: &str, v: &Value) -> bool {
        match v["type"].as_str().unwrap_or_default() {
            "assistant" => {
                let mut changed = false;
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    let Some(lane) = self.lane_mut(parent) else {
                        return changed;
                    };
                    match block["type"].as_str().unwrap_or_default() {
                        "text" => {
                            let t = text_of(block, "text");
                            if !t.trim().is_empty() {
                                lane.text = t;
                                changed = true;
                            }
                        }
                        "tool_use" => {
                            let id = block["id"].as_str().unwrap_or_default();
                            let tool = block["name"].as_str().unwrap_or_default();
                            let input = &block["input"];
                            // How a subagent hands its report back.
                            if tool == "SubagentHandback" {
                                if let Some(m) = input["message"].as_str() {
                                    lane.text = m.to_string();
                                    changed = true;
                                }
                            } else if let Some(s) = find_step(&mut lane.steps, id) {
                                changed |= set_input(s, input);
                            } else {
                                lane.steps.push(new_step(id, tool, input));
                                changed = true;
                            }
                        }
                        _ => {}
                    }
                }
                changed
            }
            "user" => self.tool_results(v),
            _ => false,
        }
    }

    /// A user message from the CLI carries tool results, or a slash
    /// command's output. Its other text (such as "[Request interrupted by
    /// user]", a compact summary, a prompt echoed back) is the CLI's own,
    /// shown otherwise or already shown.
    fn user(&mut self, v: &Value) -> bool {
        if let Some(s) = v["message"]["content"].as_str() {
            return match command_output(s) {
                Some(out) => self.command_notice(out),
                None => false,
            };
        }
        self.tool_results(v)
    }

    fn tool_results(&mut self, v: &Value) -> bool {
        // `tool_use_result` on stdout, `toolUseResult` in a transcript: the
        // tool's own account of its one result in this message.
        let detail = match &v["tool_use_result"] {
            Value::Null => &v["toolUseResult"],
            d => d,
        };
        let results: Vec<&Value> = v["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "tool_result")
            .collect();
        let detail = if results.len() == 1 {
            detail
        } else {
            &Value::Null
        };
        let mut changed = false;
        for block in results {
            let id = block["tool_use_id"].as_str().unwrap_or_default();
            let failed = block["is_error"].as_bool().unwrap_or(false);
            let withdrawn = self.withdrawn.remove(id);
            self.asked.retain(|_, tool_use| tool_use != id);
            let text = result_text(&block["content"]);
            if self.hidden.contains_key(id) {
                changed |= self.hidden_result(id, failed, withdrawn, &text, detail);
                continue;
            }
            let state = match (failed, withdrawn) {
                (false, _) => StepState::Done,
                (true, true) => StepState::Stopped,
                (true, false) => StepState::Failed,
            };
            let Some(step) = self.step_mut(id) else {
                continue;
            };
            step.state = state;
            step.output = tail(&text, OUTPUT_TAIL);
            if let Some(edit) = step.edit.as_mut() {
                if detail.is_object() {
                    edit.apply_result(detail);
                }
            }
            if let Some(lane) = step.lane.as_mut() {
                if let Some(report) = detail["handbackReport"]["text"].as_str() {
                    lane.text = report.to_string();
                } else if lane.text.is_empty() {
                    lane.text = text;
                }
            }
            changed = true;
        }
        changed
    }

    /// The result of AskUserQuestion or ExitPlanMode: settles its block,
    /// or adds one when it was never asked here (answered another way, or
    /// read from a transcript).
    fn hidden_result(
        &mut self,
        id: &str,
        failed: bool,
        withdrawn: bool,
        text: &str,
        detail: &Value,
    ) -> bool {
        // Its result is the last that is heard of it.
        let Some((tool, input)) = self.hidden.remove(id) else {
            return false;
        };
        let to = match (failed, withdrawn) {
            (false, _) => AskState::Allowed,
            (true, true) => AskState::NotNeeded,
            (true, false) => AskState::Denied,
        };
        let said: Vec<String> = questions(&input)
            .iter()
            .map(|q| {
                detail["answers"][&q.question]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        let found = self.blocks.iter_mut().rev().find_map(|b| match b {
            Block::Question {
                tool_use_id,
                state,
                answers,
                ..
            } if tool_use_id == id => {
                if *state == AskState::Waiting {
                    *state = to;
                }
                if answers.is_empty() && said.iter().any(|a| !a.is_empty()) {
                    *answers = said.clone();
                }
                Some(())
            }
            Block::Plan {
                tool_use_id,
                state,
                feedback,
                ..
            } if tool_use_id == id => {
                if *state == AskState::Waiting {
                    *state = to;
                    if to == AskState::Denied {
                        *feedback = text.to_string();
                    }
                }
                Some(())
            }
            _ => None,
        });
        if found.is_some() {
            return true;
        }
        match tool.as_str() {
            "AskUserQuestion" => self.blocks.push(Block::Question {
                request_id: String::new(),
                tool_use_id: id.to_string(),
                questions: questions(&input),
                answers: said,
                state: to,
            }),
            "ExitPlanMode" => self.blocks.push(Block::Plan {
                request_id: String::new(),
                tool_use_id: id.to_string(),
                plan: input["plan"].as_str().unwrap_or_default().to_string(),
                state: to,
                feedback: if to == AskState::Denied {
                    text.to_string()
                } else {
                    String::new()
                },
            }),
            _ => return false,
        }
        true
    }

    fn result(&mut self, v: &Value) -> bool {
        let aborted = v["terminal_reason"]
            .as_str()
            .is_some_and(|r| r.starts_with("aborted"));
        let ending = match (v["is_error"].as_bool().unwrap_or(false), aborted) {
            (_, true) => Ending::Stopped,
            (true, false) => Ending::Failed,
            (false, false) => Ending::Done,
        };
        let usage = TurnUsage::from_result(v);
        if let (Some(used), Some(limit)) = (usage.context_used, usage.context_limit) {
            let c = self.context.get_or_insert_with(ContextUsage::default);
            c.used = used;
            c.limit = limit;
        }
        self.settle_turn();
        self.status = None;
        self.blocks.push(Block::Footer {
            duration_ms: v["duration_ms"].as_u64().unwrap_or_default(),
            ending,
            usage,
        });
        if let Some(id) = v["session_id"].as_str() {
            self.session_id = Some(id.to_string());
        }
        // A CLI without lifecycle messages starts the next queued prompt
        // when the turn ends.
        if !self.queued.is_empty() && !self.info.has_capability("msg_lifecycle_v1") {
            let q = self.queued.remove(0);
            self.push_prompt_with_images(&q.text, &q.images);
        }
        // The CLI sends the result before it starts what waits, so idle
        // here would let a local queue or a hand over slip in ahead of it.
        if !self.queued.is_empty() {
            self.busy = true;
        }
        true
    }

    /// The stream ended: claude.exe exited or its pipe broke. Whatever the
    /// turn left open is over. Returns whether anything changed.
    pub fn end(&mut self) -> bool {
        let was = self.busy || !self.queued.is_empty() || !self.background.is_empty();
        self.dropped.append(&mut self.queued);
        // The CLI's background tasks die with it, and a new process says
        // nothing of them until its set first changes.
        self.background.clear();
        self.settle_turn() || was
    }

    /// How many agents work in the background, such as subagents started
    /// with run_in_background: they go on after the turn's result, until
    /// the CLI wakes Claude with what they found.
    pub fn agents_at_work(&self) -> usize {
        self.background.iter().filter(|t| t.is_agent()).count()
    }

    /// Ends what a turn left open: asks no one can answer now, steps whose
    /// result will never come. Returns whether a block changed.
    fn settle_turn(&mut self) -> bool {
        fn stop(steps: &mut [Step]) -> bool {
            let mut changed = false;
            for s in steps {
                if s.state == StepState::Running {
                    s.state = StepState::Stopped;
                    changed = true;
                }
                if let Some(lane) = s.lane.as_mut() {
                    changed |= stop(&mut lane.steps);
                }
            }
            changed
        }
        let mut changed = false;
        for b in &mut self.blocks {
            match b {
                Block::Ask { state, .. }
                | Block::Question { state, .. }
                | Block::Plan { state, .. }
                    if *state == AskState::Waiting =>
                {
                    *state = AskState::NotNeeded;
                    changed = true;
                }
                Block::Tools(steps) => changed |= stop(steps),
                _ => {}
            }
        }
        self.busy = false;
        self.open.clear();
        self.streamed.clear();
        // What a turn's asks and hidden tools kept is of no use past it.
        self.asked.clear();
        self.withdrawn.clear();
        self.hidden.clear();
        self.ask_inputs.clear();
        changed
    }

    /// The lines to write to the CLI's stdin that the model made on its
    /// own, oldest first.
    pub fn take_outbox(&mut self) -> Vec<String> {
        let out = std::mem::take(&mut self.outbox);
        out.into_iter().map(|(_, line)| line).collect()
    }

    fn control_request(&mut self, v: &Value, line: &str) -> bool {
        let request = &v["request"];
        let request_id = v["request_id"].as_str().unwrap_or_default().to_string();
        if request["subtype"] != "can_use_tool" {
            // Unanswered, the CLI may wait on it for good. An error is what
            // it gets from any client that does not know a subtype.
            let subtype = request["subtype"].as_str().unwrap_or_default();
            let error = control_error(
                &request_id,
                &format!("Unsupported control request subtype: {subtype}"),
            );
            self.outbox.push((request_id, error));
            return self.raw(line);
        }
        let tool = request["tool_name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let input = request["input"].clone();
        let tool_use_id = request["tool_use_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if !tool_use_id.is_empty() {
            self.asked.insert(request_id.clone(), tool_use_id.clone());
        }
        if hidden_tool(&tool) {
            self.ask_inputs.insert(request_id.clone(), input.clone());
            if !tool_use_id.is_empty() {
                self.hidden
                    .insert(tool_use_id.clone(), (tool.clone(), input.clone()));
            }
        }
        match tool.as_str() {
            "AskUserQuestion" => self.blocks.push(Block::Question {
                request_id,
                tool_use_id,
                questions: questions(&input),
                answers: Vec::new(),
                state: AskState::Waiting,
            }),
            "ExitPlanMode" => self.blocks.push(Block::Plan {
                request_id,
                tool_use_id,
                plan: input["plan"].as_str().unwrap_or_default().to_string(),
                state: AskState::Waiting,
                feedback: String::new(),
            }),
            _ => {
                let summary = summarize(&tool, &input);
                self.blocks.push(Block::Ask {
                    request_id,
                    shown: ask_shown(&tool, &input),
                    summary,
                    tool,
                    input,
                    suggestions: request["permission_suggestions"].clone(),
                    state: AskState::Waiting,
                });
            }
        }
        true
    }

    /// The CLI gave up a request of its own, such as an ask it no longer
    /// needs answered.
    fn cancel(&mut self, request_id: &str) -> bool {
        if let Some(tool_use) = self.asked.get(request_id) {
            self.withdrawn.insert(tool_use.clone());
        }
        self.blocks.iter_mut().any(|b| match b {
            Block::Ask {
                request_id: r,
                state,
                ..
            }
            | Block::Question {
                request_id: r,
                state,
                ..
            }
            | Block::Plan {
                request_id: r,
                state,
                ..
            } if r == request_id && *state == AskState::Waiting => {
                *state = AskState::NotNeeded;
                true
            }
            _ => false,
        })
    }
}

/// A text field's trimmed contents, None when there are none.
fn said(v: &Value) -> Option<String> {
    let t = v.as_str()?.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// What a slash command printed, from the user message the CLI wraps it
/// in, or None for any other text.
fn command_output(s: &str) -> Option<String> {
    let inner = ["local-command-stdout", "local-command-stderr"]
        .iter()
        .find_map(|tag| {
            let open = format!("<{tag}>");
            let close = format!("</{tag}>");
            let start = s.find(&open)? + open.len();
            let end = s[start..].find(&close)? + start;
            Some(s[start..end].trim().to_string())
        })?;
    (!inner.is_empty()).then_some(inner)
}

/// The line that sets the effort level. The CLI has no control request for
/// it (2.1.312 refuses `set_effort`), so it goes as the `/effort` command;
/// the levels each model takes are in [`ModelInfo::effort_levels`].
pub fn effort_line(level: &str) -> String {
    prompt_line(&format!("/effort {level}"))
}

/// What an ask's card shows: all of what the tool acts on (every line of
/// a command, a file's whole path), or its input as JSON when the tool
/// names nothing, cut short only past what anyone would read.
fn ask_shown(tool: &str, input: &Value) -> String {
    const MAX: usize = 20_000;
    let subject = subject(tool, input).trim_end();
    if !subject.trim().is_empty() {
        return subject.to_string();
    }
    let mut s = serde_json::to_string_pretty(input).unwrap_or_default();
    if s.len() > MAX {
        let cut = (0..=MAX)
            .rev()
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(0);
        s.truncate(cut);
        s.push('\u{2026}');
    }
    s
}

/// The answer to a control request the Folio cannot serve, shaped as the
/// SDK shapes its errors.
pub fn control_error(request_id: &str, error: &str) -> String {
    serde_json::json!({
        "type": "control_response",
        "response": {"subtype": "error", "request_id": request_id, "error": error},
    })
    .to_string()
}

/// The `control_response` to a `can_use_tool`. An allow echoes the input
/// back as `updatedInput`, which the CLI requires.
pub fn control_response(request_id: &str, allow: bool, input: Value) -> String {
    let response = if allow {
        serde_json::json!({"behavior": "allow", "updatedInput": input})
    } else {
        serde_json::json!({"behavior": "deny", "message": "The user denied this in Horadric."})
    };
    control_success(request_id, response)
}

/// What a tool call acts on, whole: a command, a file's path, a pattern.
/// Empty for a tool not known here.
fn subject<'a>(tool: &str, input: &'a Value) -> &'a str {
    let field = |name: &str| input[name].as_str().unwrap_or_default();
    match tool {
        "Bash" | "PowerShell" => field("command"),
        "Read" | "Edit" | "Write" | "MultiEdit" => field("file_path"),
        "NotebookEdit" => field("notebook_path"),
        "Grep" | "Glob" => field("pattern"),
        "WebFetch" => field("url"),
        "WebSearch" => field("query"),
        "Task" | "Agent" => field("description"),
        "Skill" => field("skill"),
        "ToolSearch" => field("query"),
        _ => "",
    }
}

/// One line saying what a tool call does: a command, a file name, a pattern.
pub fn summarize(tool: &str, input: &Value) -> String {
    let text = subject(tool, input);
    match tool {
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => first_line(file_name(text)),
        _ => first_line(text),
    }
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

    const PLAIN: &str = include_str!("../../tests/fixtures/headless/plain.jsonl");
    const TOOLS: &str = include_str!("../../tests/fixtures/headless/tools.jsonl");
    const BASH_ALLOW: &str = include_str!("../../tests/fixtures/headless/bash_allow.jsonl");
    const BASH_DENY: &str = include_str!("../../tests/fixtures/headless/bash_deny.jsonl");
    const INTERRUPT: &str = include_str!("../../tests/fixtures/headless/interrupt.jsonl");
    const ASK_INTERRUPT: &str = include_str!("../../tests/fixtures/headless/ask_interrupt.jsonl");

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
                ending,
                ..
            } => {
                assert_eq!(duration_ms, 2691);
                assert_eq!(ending, Ending::Done);
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
        assert_eq!(steps[0].state, StepState::Done);
        assert_eq!(steps[1].tool, "Grep");
        assert_eq!(steps[1].summary, "println");
        assert_eq!(steps[1].state, StepState::Done);
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
        assert_eq!(runs(&f)[0][0].state, StepState::Running);
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
            state,
            ..
        }) = f.blocks.last().cloned()
        else {
            panic!("{:#?}", f.blocks);
        };
        assert_eq!(tool, "Bash");
        assert_eq!(summary, "echo horadric-test > out.txt");
        assert_eq!(state, AskState::Waiting);
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
        assert_eq!(runs(&f)[0][0].state, StepState::Done);
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
        assert_eq!(runs(&f)[0][0].state, StepState::Failed);
        assert!(matches!(
            f.blocks.iter().find(|b| matches!(b, Block::Ask { .. })),
            Some(Block::Ask {
                state: AskState::Denied,
                ..
            })
        ));
        assert_eq!(
            replies(&f),
            ["The command was refused, so out2.txt was not written."]
        );
    }

    #[test]
    fn always_adds_the_suggested_rules_and_a_reason_goes_with_a_deny() {
        let rules = serde_json::json!([{"type": "addRules", "behavior": "allow",
            "rules": [{"toolName": "Bash", "ruleContent": "echo:*"}],
            "destination": "localSettings"}]);
        let ask = |f: &mut Folio, id: &str| {
            f.feed(
                &serde_json::json!({"type": "control_request", "request_id": id,
                    "request": {"subtype": "can_use_tool", "tool_name": "Bash",
                        "input": {"command": "echo hi"}, "tool_use_id": "t",
                        "permission_suggestions": rules}})
                .to_string(),
            );
        };
        let mut f = Folio::new();
        ask(&mut f, "a");
        let v: Value = serde_json::from_str(&f.reply("a", Reply::Always).unwrap()).unwrap();
        let r = &v["response"]["response"];
        assert_eq!(r["behavior"], "allow");
        assert_eq!(r["updatedInput"]["command"], "echo hi");
        assert_eq!(r["updatedPermissions"], rules);

        ask(&mut f, "b");
        let v: Value =
            serde_json::from_str(&f.reply("b", Reply::Deny(" use ls ")).unwrap()).unwrap();
        assert_eq!(v["response"]["response"]["behavior"], "deny");
        assert_eq!(v["response"]["response"]["message"], "use ls");
        assert!(f.reply("b", Reply::Allow).is_none(), "answered once only");
    }

    #[test]
    fn an_unknown_request_is_not_answered() {
        let mut f = Folio::new();
        assert!(f.answer("nope", true).is_none());
    }

    #[test]
    fn an_interrupt_ends_the_turn_stopped_and_the_next_turn_goes_on() {
        let mut f = Folio::new();
        f.push_prompt("Write the numbers");
        let mut lines = INTERRUPT.lines();
        feed_until(&mut f, &mut lines, |v| v["type"] == "result");
        assert!(!f.busy);
        assert!(matches!(
            f.blocks.last(),
            Some(Block::Footer {
                ending: Ending::Stopped,
                ..
            })
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
                ending: Ending::Done,
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

    fn footers(f: &Folio) -> Vec<Ending> {
        f.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Footer { ending, .. } => Some(*ending),
                _ => None,
            })
            .collect()
    }

    /// Plays two turns, the second prompt sent after the first result.
    fn play_two(stream: &str, edit: impl Fn(Value) -> Value) -> Folio {
        let mut f = Folio::new();
        let mut prompts = ["two", "one"].to_vec();
        f.push_prompt(prompts.pop().unwrap());
        for line in stream.lines() {
            let v = edit(serde_json::from_str(line).unwrap());
            f.feed(&v.to_string());
            if v["type"] == "result" {
                if let Some(next) = prompts.pop() {
                    f.push_prompt(next);
                }
            }
        }
        f
    }

    #[test]
    fn esc_during_an_ask_withdraws_it_and_stops_the_turn() {
        let mut f = Folio::new();
        f.push_prompt("Run echo");
        let mut lines = ASK_INTERRUPT.lines();
        feed_until(&mut f, &mut lines, |v| v["type"] == "control_request");
        let id = f.pending_asks()[0].to_string();
        feed_until(&mut f, &mut lines, |v| {
            v["type"] == "control_cancel_request"
        });
        assert!(f.pending_asks().is_empty());
        assert!(
            f.answer(&id, true).is_none(),
            "no answer to a withdrawn ask"
        );
        feed_until(&mut f, &mut lines, |v| v["type"] == "result");
        assert!(!f.busy);
        assert!(f.blocks.iter().any(|b| matches!(
            b,
            Block::Ask {
                state: AskState::NotNeeded,
                ..
            }
        )));
        assert_eq!(runs(&f)[0][0].state, StepState::Stopped);
        assert_eq!(footers(&f)[0], Ending::Stopped);

        f.push_prompt("Reply with exactly: still here");
        for line in lines {
            f.feed(line);
        }
        assert_eq!(replies(&f).last(), Some(&"still here"));
        assert_eq!(footers(&f)[1], Ending::Done);
    }

    #[test]
    fn a_turn_s_end_lapses_its_asks_and_stops_its_steps() {
        let mut f = Folio::new();
        f.push_prompt("p");
        let mut lines = BASH_ALLOW.lines();
        feed_until(&mut f, &mut lines, |v| v["type"] == "control_request");
        let id = f.pending_asks()[0].to_string();
        assert!(f.end(), "the stream ending mid turn changes what shows");
        assert!(!f.busy);
        assert!(f.pending_asks().is_empty());
        assert!(f.answer(&id, true).is_none());
        assert_eq!(runs(&f)[0][0].state, StepState::Stopped);
        assert!(!f.end(), "and once only");

        // A result with an ask still waiting lapses it the same way.
        let mut f = Folio::new();
        f.push_prompt("p");
        let mut lines = BASH_ALLOW.lines();
        feed_until(&mut f, &mut lines, |v| v["type"] == "control_request");
        f.feed(r#"{"type":"result","subtype":"error_during_execution","is_error":true,"duration_ms":10}"#);
        assert!(f.pending_asks().is_empty());
        assert_eq!(footers(&f)[0], Ending::Failed);
    }

    #[test]
    fn a_request_the_folio_cannot_serve_is_refused_and_kept_raw() {
        let mut f = Folio::new();
        let line =
            r#"{"type":"control_request","request_id":"r9","request":{"subtype":"something_new"}}"#;
        assert!(f.feed(line));
        assert!(matches!(f.blocks.last(), Some(Block::Raw(_))));
        let out = f.take_outbox();
        assert_eq!(out.len(), 1);
        let v: Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(v["type"], "control_response");
        assert_eq!(v["response"]["subtype"], "error");
        assert_eq!(v["response"]["request_id"], "r9");
        assert!(v["response"]["error"]
            .as_str()
            .unwrap()
            .contains("something_new"));
        assert!(f.take_outbox().is_empty(), "sent once");
        assert!(f.pending_asks().is_empty());
    }

    #[test]
    fn a_replay_does_not_answer_a_request_again() {
        let ask = |id: &str| {
            format!(
                r#"{{"type":"control_request","request_id":"{id}","request":{{"subtype":"x"}}}}"#
            )
        };
        let mut f = Folio::new();
        f.feed(&ask("r1"));
        f.feed_sent(&control_error(
            "r1",
            "Unsupported control request subtype: x",
        ));
        f.feed(&ask("r2"));
        let out = f.take_outbox();
        assert_eq!(out.len(), 1, "only the one never answered");
        assert!(out[0].contains("\"r2\""));
    }

    #[test]
    fn initialize_is_known_sent_once_replayed() {
        let mut f = Folio::new();
        assert!(!f.initialized);
        let line = Folio::new().initialize();
        f.feed_sent(&line);
        assert!(f.initialized);
        let mut g = Folio::new();
        g.initialize();
        assert!(g.initialized);
    }

    #[test]
    fn a_question_without_its_tool_use_still_echoes_its_input() {
        let mut f = Folio::new();
        f.push_prompt("p");
        let input =
            serde_json::json!({"questions": [{"question": "Q?", "options": [{"label": "A"}]}]});
        let line = serde_json::json!({"type": "control_request", "request_id": "q1", "request": {
            "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": input}});
        f.feed(&line.to_string());
        let out = f.answer_question("q1", &[vec!["A".into()]]).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let echoed = &v["response"]["response"]["updatedInput"];
        assert_eq!(echoed["questions"], input["questions"]);
        assert_eq!(echoed["answers"]["Q?"], "A");

        let plan = serde_json::json!({"type": "control_request", "request_id": "p1", "request": {
            "subtype": "can_use_tool", "tool_name": "ExitPlanMode", "input": {"plan": "Do it."}}});
        f.feed(&plan.to_string());
        let out = f.approve_plan("p1", None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["response"]["response"]["updatedInput"]["plan"], "Do it.");
    }

    #[test]
    fn a_turn_keeps_nothing_of_its_asks_once_over() {
        for stream in [ASK_QUESTION, PLAN_KEEP, ASK_INTERRUPT, BASH_ALLOW] {
            let (f, _) = live("p", stream, |f, id| {
                let picks = [vec!["Blue".into()], vec!["Large".into()]];
                f.answer_question(id, &picks)
                    .or_else(|| f.keep_planning(id, KEEP))
                    .or_else(|| f.answer(id, true))
                    .unwrap()
            });
            assert!(f.hidden.is_empty() && f.asked.is_empty());
            assert!(f.withdrawn.is_empty() && f.ask_inputs.is_empty());
        }
    }

    #[test]
    fn a_delta_after_the_copy_adds_nothing() {
        let mut f = Folio::new();
        f.push_prompt("p");
        let lines = [
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"thinking","thinking":"Hmm."}]}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Hmm."}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hel"}}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Hello"}]}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"lo"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#,
        ];
        for l in lines {
            f.feed(l);
        }
        assert_eq!(
            f.blocks[1..],
            [Block::Thinking("Hmm.".into()), Block::Reply("Hello".into())],
            "{:#?}",
            f.blocks
        );
    }

    #[test]
    fn an_ask_without_a_summary_shows_its_input() {
        let mut f = Folio::new();
        f.feed(r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"mcp__x__y","input":{"a":1}}}"#);
        let Some(Block::Ask { shown, .. }) = f.blocks.last() else {
            panic!("{:#?}", f.blocks);
        };
        assert_eq!(shown, "{\n  \"a\": 1\n}");
        let long = ask_shown("x", &serde_json::json!({"a": "\u{e9}".repeat(20_000)}));
        assert!(long.len() <= 20_000 + 3 && long.ends_with('\u{2026}'));
    }

    #[test]
    fn an_ask_shows_all_it_acts_on() {
        let bash = serde_json::json!({"command": "echo hi\nrm -rf ~\n"});
        assert_eq!(ask_shown("Bash", &bash), "echo hi\nrm -rf ~");
        let edit = serde_json::json!({"file_path": "C:\\a\\b\\pane.rs"});
        assert_eq!(ask_shown("Edit", &edit), "C:\\a\\b\\pane.rs");
        assert_eq!(summarize("Edit", &edit), "pane.rs");
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
    fn a_turn_is_finished_once_its_result_came() {
        let mut f = Folio::new();
        assert!(!f.turn_finished());
        f.push_prompt("p");
        assert!(!f.turn_finished());
        for line in PLAIN.lines() {
            f.feed(line);
        }
        assert!(f.turn_finished());
        f.push_prompt("again");
        assert!(!f.turn_finished());
    }

    #[test]
    fn background_tasks_are_state_not_blocks() {
        let mut f = Folio::new();
        let two = r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b1","task_type":"local_bash","description":"npm run dev"},{"task_id":"a2","task_type":"local_agent","description":"Review"}],"uuid":"u","session_id":"s"}"#;
        assert!(f.feed(two));
        assert!(!f.feed(two));
        assert_eq!(f.background.len(), 2);
        assert_eq!(f.background[0].description, "npm run dev");
        assert!(f.feed(r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#));
        assert!(f.background.is_empty());
        assert!(f.blocks.is_empty());
    }

    #[test]
    fn background_agents_work_past_the_turn_s_result() {
        let mut f = play("p", PLAIN);
        assert!(!f.busy);
        assert_eq!(f.agents_at_work(), 0);
        let set = |tasks: &str| {
            format!(r#"{{"type":"system","subtype":"background_tasks_changed","tasks":[{tasks}]}}"#)
        };
        let agent = r#"{"task_id":"a1","task_type":"local_agent","description":"Review"}"#;
        let shell = r#"{"task_id":"b1","task_type":"local_bash","description":"npm run dev"}"#;
        let ambient =
            r#"{"task_id":"w1","task_type":"local_agent","description":"w","ambient":true}"#;
        f.feed(&set(&[agent, shell, ambient].join(",")));
        assert_eq!(
            f.agents_at_work(),
            1,
            "a shell or ambient task is not an agent at work"
        );
        f.feed(&set(shell));
        assert_eq!(f.agents_at_work(), 0, "the set replaces, it does not add");
        assert_eq!(f.background.len(), 1);
    }

    #[test]
    fn a_missed_set_cannot_keep_an_agent_at_work() {
        let set = r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a1","task_type":"local_agent","description":"Review"}]}"#;
        let done = r#"{"type":"system","subtype":"task_notification","task_id":"a1","status":"completed","summary":"s"}"#;
        let mut f = Folio::new();
        f.feed(set);
        assert!(f.feed(done), "its notification ends it");
        assert_eq!(f.agents_at_work(), 0);
        assert!(f.blocks.is_empty());

        let mut f = Folio::new();
        f.feed(set);
        assert!(f.end(), "the CLI ending ends its tasks");
        assert_eq!(f.agents_at_work(), 0);
        assert!(!f.end());
    }

    #[test]
    fn system_lines_for_other_hosts_stay_quiet() {
        let mut f = Folio::new();
        for s in [
            "session_state_changed",
            "turn_duration",
            "ui_toast",
            "task_summary",
            "api_error",
        ] {
            f.feed(&format!(r#"{{"type":"system","subtype":"{s}"}}"#));
        }
        f.feed(r#"{"type":"system","subtype":"informational","level":"info","content":"verbose"}"#);
        assert!(f.blocks.is_empty());
    }

    #[test]
    fn system_lines_meant_for_the_human_are_notices() {
        let mut f = Folio::new();
        f.feed(r#"{"type":"system","subtype":"notification","key":"k","text":"Heads up","priority":"low"}"#);
        f.feed(
            r#"{"type":"system","subtype":"informational","level":"warning","content":"Careful"}"#,
        );
        f.feed(
            r#"{"type":"system","subtype":"local_command_output","content":"Set model to sonnet"}"#,
        );
        // The same output again as the user message the CLI wraps it in.
        f.feed(r#"{"type":"user","message":{"content":"<local-command-stdout>Set model to sonnet</local-command-stdout>"}}"#);
        let n: Vec<_> = (f.blocks.iter())
            .filter_map(|b| match b {
                Block::Notice(n) => Some(n.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            n,
            [
                Notice::Info("Heads up".into()),
                Notice::Info("Careful".into()),
                Notice::CommandOutput("Set model to sonnet".into()),
            ]
        );
    }

    #[test]
    fn commands_changed_replaces_the_command_list() {
        let mut f = Folio::new();
        f.feed(r#"{"type":"system","subtype":"commands_changed","commands":[{"name":"deploy","description":"Ship it","argumentHint":""}]}"#);
        assert_eq!(f.info.commands.len(), 1);
        assert_eq!(f.slash_commands("dep")[0].description, "Ship it");
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
        assert!(f.feed(r#"{"type":"system","subtype":"something_new"}"#));
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

    const EDITS: &str = include_str!("../../tests/fixtures/headless/edits.jsonl");
    const ASK_QUESTION: &str = include_str!("../../tests/fixtures/headless/ask_question.jsonl");
    const PLAN_APPROVE: &str = include_str!("../../tests/fixtures/headless/plan_approve.jsonl");
    const PLAN_ACCEPT_EDITS: &str =
        include_str!("../../tests/fixtures/headless/plan_accept_edits.jsonl");
    const PLAN_KEEP: &str = include_str!("../../tests/fixtures/headless/plan_keep.jsonl");
    const SUBAGENT: &str = include_str!("../../tests/fixtures/headless/subagent.jsonl");
    const IMAGE: &str = include_str!("../../tests/fixtures/headless/image.jsonl");
    const QUEUED_NEXT: &str = include_str!("../../tests/fixtures/headless/queued_next.jsonl");
    const QUEUED_NOW: &str = include_str!("../../tests/fixtures/headless/queued_now.jsonl");
    const COMPACT: &str = include_str!("../../tests/fixtures/headless/compact.jsonl");
    const CLEAR: &str = include_str!("../../tests/fixtures/headless/clear.jsonl");
    const API_RETRY: &str = include_str!("../../tests/fixtures/headless/api_retry.jsonl");
    const CONTROLS: &str = include_str!("../../tests/fixtures/headless/controls.jsonl");
    const EFFORT: &str = include_str!("../../tests/fixtures/headless/effort.jsonl");
    const TRANSCRIPT_EDITS: &str =
        include_str!("../../tests/fixtures/headless/transcript_edits.jsonl");
    const TRANSCRIPT_IMAGE: &str =
        include_str!("../../tests/fixtures/headless/transcript_image.jsonl");
    const TRANSCRIPT_SUBAGENT: &str =
        include_str!("../../tests/fixtures/headless/transcript_subagent.jsonl");

    /// The red 16 by 16 PNG of image.jsonl.
    const RED_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAGElEQVR4nGP4z8BAEiJN9aiGUQ0MQ0kDAJD5/wGaM2eTAAAAAElFTkSuQmCC";

    fn steps(f: &Folio) -> Vec<&Step> {
        runs(f).into_iter().flatten().collect()
    }

    fn notices(f: &Folio) -> Vec<&Notice> {
        f.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Notice(n) => Some(n),
                _ => None,
            })
            .collect()
    }

    fn request_id(line: &str) -> Value {
        serde_json::from_str::<Value>(line).unwrap()["request_id"].clone()
    }

    #[test]
    fn init_describes_the_session() {
        let f = play("p", PLAIN);
        assert_eq!(f.info.claude_version, "2.1.312");
        assert_eq!(f.info.api_key_source, "none");
        assert_eq!(f.permission_mode(), "auto");
        assert!(f.info.tools.iter().any(|t| t == "Bash"));
        let f = play("p", CONTROLS);
        assert!(f.info.has_capability("msg_lifecycle_v1"));
        assert_eq!(f.info.mcp_servers[0].name, "horadric");
        assert!(f
            .slash_commands("/comp")
            .iter()
            .any(|c| c.name == "compact"));
    }

    /// Plays the controls recording with its request ids made ours, and
    /// returns the lines sent.
    fn controls() -> (Folio, Vec<String>) {
        let mut f = Folio::new();
        let sent = vec![
            f.initialize(),
            f.get_context_usage(),
            f.set_permission_mode("plan"),
            f.set_model("sonnet"),
        ];
        let ids: Vec<(&str, Value)> = ["init_1", "c1", "c2", "c3"]
            .into_iter()
            .zip(sent.iter().map(|l| request_id(l)))
            .collect();
        f.push_prompt("Reply with exactly: pong");
        for line in CONTROLS.lines() {
            f.feed(&ours(line, &ids));
        }
        (f, sent)
    }

    /// A recorded line with the request ids it answers made ours.
    fn ours(line: &str, ids: &[(&str, Value)]) -> String {
        let mut v: Value = serde_json::from_str(line).unwrap();
        if let Some((_, id)) = ids
            .iter()
            .find(|(theirs, _)| v["response"]["request_id"] == *theirs)
        {
            v["response"]["request_id"] = id.clone();
        }
        v.to_string()
    }

    #[test]
    fn control_requests_carry_their_fields() {
        let mut f = Folio::new();
        let v: Value = serde_json::from_str(&f.set_permission_mode("acceptEdits")).unwrap();
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request"]["subtype"], "set_permission_mode");
        assert_eq!(v["request"]["mode"], "acceptEdits");
        let w: Value = serde_json::from_str(&f.set_model("opus")).unwrap();
        assert_eq!(w["request"]["model"], "opus");
        assert_ne!(v["request_id"], w["request_id"]);
        let c: Value = serde_json::from_str(&f.get_context_usage()).unwrap();
        assert_eq!(c["request"]["subtype"], "get_context_usage");
        let e: Value = serde_json::from_str(&effort_line("high")).unwrap();
        assert_eq!(e["message"]["content"], "/effort high");
    }

    #[test]
    fn control_responses_set_mode_model_context_and_commands() {
        let (f, _) = controls();
        assert_eq!(f.permission_mode(), "plan");
        assert_eq!(f.model.as_deref(), Some("claude-sonnet-5-5"));
        let c = f.context.as_ref().unwrap();
        assert_eq!(c.used, 36674);
        assert_eq!(c.limit, 1_000_000);
        assert_eq!(c.auto_compact_at, Some(967_000));
        assert!(c
            .categories
            .iter()
            .any(|k| k.name == "System prompt" && !k.deferred));
        assert!(c.categories.iter().any(|k| k.deferred));
        let compact = &f.slash_commands("compact")[0];
        assert!(!compact.description.is_empty());
        let m = f.info.models.iter().find(|m| m.value == "default").unwrap();
        assert!(m.effort_levels.iter().any(|l| l == "high"));
        // set_model's own words come as a slash command's output.
        assert!(notices(&f)
            .iter()
            .any(|n| matches!(n, Notice::CommandOutput(t) if t.contains("sonnet"))));
        // The responses to requests not ours (c4, c5) show nothing.
        assert!(!notices(&f).iter().any(|n| matches!(n, Notice::Refused(_))));
    }

    #[test]
    fn a_refused_request_of_ours_is_a_notice() {
        let mut f = Folio::new();
        let id = request_id(&f.set_model("nope"));
        let id = id.as_str().unwrap();
        assert!(f.feed(&control_error(id, "Unknown model")));
        assert_eq!(notices(&f), [&Notice::Refused("Unknown model".into())]);
        assert!(!f.feed(&control_error(id, "again")), "answered once only");
    }

    #[test]
    fn edits_carry_their_diff_and_counts() {
        let f = play("p", EDITS);
        let edits: Vec<&FileEdit> = steps(&f).iter().filter_map(|s| s.edit.as_ref()).collect();
        assert_eq!(edits.len(), 3, "{:#?}", f.blocks);
        assert_eq!(edits[0].file_name(), "hello.txt");
        assert!(edits[0].created);
        assert_eq!(edits[0].counts(), "+2 -0");
        assert_eq!(edits[1].file_name(), "main.rs");
        assert_eq!(edits[1].changes[0].new, "println!(\"bye\");");
        assert_eq!(edits[1].hunks[0].lines[1], "-    println!(\"hello\");");
        assert_eq!(edits[1].counts(), "+1 -1");
        assert_eq!(edits[2].counts(), "+1 -1");
        let read = steps(&f).into_iter().find(|s| s.tool == "Read").unwrap();
        assert!(read.edit.is_none());
        assert!(read.output.contains("fn main()"));
    }

    #[test]
    fn a_footer_has_the_turn_s_tokens_and_the_context_left() {
        let f = play("p", EDITS);
        let Some(Block::Footer { usage, .. }) = f.blocks.last() else {
            panic!("{:#?}", f.blocks.last());
        };
        assert_eq!(usage.output_tokens, 850);
        assert_eq!(usage.cache_read_tokens, 90054);
        assert_eq!(usage.context_limit, Some(1_000_000));
        let used = usage.context_used.unwrap();
        assert!(used > 50_000 && used < 60_000, "{used}");
        assert_eq!(f.context.as_ref().unwrap().used, used);
    }

    #[test]
    fn ask_user_question_is_a_question_answered_with_its_answers() {
        let mut f = Folio::new();
        f.push_prompt("p");
        let mut lines = ASK_QUESTION.lines();
        feed_until(&mut f, &mut lines, |v| v["type"] == "control_request");
        assert!(
            !steps(&f).iter().any(|s| s.tool == "AskUserQuestion"),
            "not a step"
        );
        let Some(Block::Question {
            request_id,
            questions,
            state: AskState::Waiting,
            ..
        }) = f.blocks.last().cloned()
        else {
            panic!("{:#?}", f.blocks.last());
        };
        assert_eq!(f.pending_asks(), [request_id.as_str()]);
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0].question, "Which color do you prefer?");
        assert_eq!(questions[0].options[1].label, "Blue");
        assert_eq!(questions[0].options[1].description, "Blue color");
        assert!(questions[1].multi_select);

        let line = f
            .answer_question(&request_id, &[vec!["Blue".into()], vec!["Large".into()]])
            .unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        let r = &v["response"]["response"];
        assert_eq!(r["behavior"], "allow");
        assert_eq!(
            r["updatedInput"]["answers"],
            serde_json::json!({"Which color do you prefer?": "Blue", "Which sizes do you want?": "Large"})
        );
        assert_eq!(r["updatedInput"]["questions"][1]["multiSelect"], true);
        assert!(f.answer_question(&request_id, &[]).is_none(), "once only");

        for line in lines {
            f.feed(line);
        }
        let Some(Block::Question { answers, state, .. }) = f
            .blocks
            .iter()
            .find(|b| matches!(b, Block::Question { .. }))
        else {
            unreachable!()
        };
        assert_eq!(*state, AskState::Allowed);
        assert_eq!(answers, &["Blue", "Large"]);
        assert_eq!(replies(&f).last(), Some(&"Blue, Large"));
    }

    fn plan_until_ask(stream: &str) -> (Folio, std::str::Lines<'_>, String) {
        let mut f = Folio::new();
        f.push_prompt("p");
        let mut lines = stream.lines();
        feed_until(&mut f, &mut lines, |v| v["type"] == "control_request");
        assert_eq!(f.permission_mode(), "plan");
        let Some(Block::Plan {
            request_id, plan, ..
        }) = f.blocks.last()
        else {
            panic!("{:#?}", f.blocks.last());
        };
        assert!(plan.contains("goodbye"));
        let id = request_id.clone();
        (f, lines, id)
    }

    #[test]
    fn an_approved_plan_leaves_plan_mode_by_itself() {
        let (mut f, lines, id) = plan_until_ask(PLAN_APPROVE);
        assert!(!steps(&f).iter().any(|s| s.tool == "ExitPlanMode"));
        let v: Value = serde_json::from_str(&f.approve_plan(&id, None).unwrap()).unwrap();
        let r = &v["response"]["response"];
        assert_eq!(r["behavior"], "allow");
        assert!(r["updatedInput"]["plan"]
            .as_str()
            .unwrap()
            .contains("goodbye"));
        assert!(r.get("updatedPermissions").is_none());
        for line in lines {
            f.feed(line);
        }
        assert_eq!(f.permission_mode(), "default");
        assert!(f.blocks.iter().any(|b| matches!(
            b,
            Block::Plan {
                state: AskState::Allowed,
                ..
            }
        )));
    }

    #[test]
    fn a_plan_approved_into_a_mode_switches_to_it() {
        let (mut f, lines, id) = plan_until_ask(PLAN_ACCEPT_EDITS);
        let v: Value =
            serde_json::from_str(&f.approve_plan(&id, Some("acceptEdits")).unwrap()).unwrap();
        assert_eq!(
            v["response"]["response"]["updatedPermissions"],
            serde_json::json!([{"type": "setMode", "mode": "acceptEdits", "destination": "session"}])
        );
        for line in lines {
            f.feed(line);
        }
        assert_eq!(f.permission_mode(), "acceptEdits");
    }

    const KEEP: &str = "Keep planning: also print a blank line first.";

    #[test]
    fn a_plan_sent_back_keeps_planning_with_the_feedback() {
        let (mut f, lines, id) = plan_until_ask(PLAN_KEEP);
        let v: Value = serde_json::from_str(&f.keep_planning(&id, KEEP).unwrap()).unwrap();
        assert_eq!(v["response"]["response"]["behavior"], "deny");
        assert_eq!(v["response"]["response"]["message"], KEEP);
        for line in lines {
            f.feed(line);
        }
        assert_eq!(f.permission_mode(), "plan");
        let Some(Block::Plan {
            state, feedback, ..
        }) = f.blocks.iter().find(|b| matches!(b, Block::Plan { .. }))
        else {
            unreachable!()
        };
        assert_eq!((*state, feedback.as_str()), (AskState::Denied, KEEP));
        assert_eq!(replies(&f).last(), Some(&"revising"));
    }

    #[test]
    fn a_subagent_s_work_folds_into_its_step() {
        let f = play("p", SUBAGENT);
        let agent = steps(&f).into_iter().find(|s| s.tool == "Agent").unwrap();
        assert_eq!(agent.state, StepState::Done);
        assert_eq!(agent.summary, "Read notes.txt content");
        let lane = agent.lane.as_ref().unwrap();
        assert_eq!(lane.agent_type, "general-purpose");
        assert_eq!(lane.steps.len(), 1, "{lane:#?}");
        assert_eq!(lane.steps[0].tool, "Read");
        assert_eq!(lane.steps[0].state, StepState::Done);
        assert!(
            lane.text.contains("A tiny scratch folder."),
            "{}",
            lane.text
        );
        assert!(lane.tokens > 0);
        // Its words are not the main reply's.
        assert!(replies(&f).iter().all(|r| !r.contains("Exact content")));
        assert!(replies(&f)
            .last()
            .unwrap()
            .starts_with("The subagent found"));
        assert!(!f.blocks.iter().any(|b| matches!(b, Block::Raw(_))));
    }

    fn red() -> ImageData {
        ImageData::from_bytes(&crate::release::base64_decode(RED_PNG).unwrap()).unwrap()
    }

    #[test]
    fn a_prompt_with_an_image_shows_its_thumbnail() {
        let mut f = Folio::new();
        let line = f.push_prompt_with_images("What color is this image?", &[red()]);
        assert!(line.contains("\"type\":\"image\""));
        for l in IMAGE.lines() {
            f.feed(l);
        }
        let Block::Images(images) = &f.blocks[1] else {
            panic!("{:#?}", f.blocks);
        };
        assert_eq!(images[0].media_type, "image/png");
        assert_eq!(images[0].size, Some((16, 16)));
        assert_eq!(replies(&f), ["Red"]);
    }

    /// Plays a recording where a prompt was queued at the first text, its
    /// uuid made the one the Folio gave it. Returns the Folio, how many
    /// waited after each line, and every line in order with whether it
    /// was sent (true) or came from the CLI.
    fn play_queued(stream: &str, priority: Priority) -> (Folio, Vec<usize>, Vec<(bool, String)>) {
        let mut f = Folio::new();
        f.push_prompt("Count");
        let mut all = vec![(true, prompt_line("Count"))];
        let mut uuid = None;
        let mut queued_seen = Vec::new();
        for line in stream.lines() {
            let mut v: Value = serde_json::from_str(line).unwrap();
            if uuid.is_none() && v["event"]["delta"]["type"] == "text_delta" {
                let sent = f.queue_prompt("Reply with exactly: queued", &[], priority);
                let l: Value = serde_json::from_str(&sent).unwrap();
                let want = if priority == Priority::Now {
                    "now"
                } else {
                    "next"
                };
                assert_eq!(l["priority"], want);
                uuid = l["uuid"].as_str().map(str::to_string);
                all.push((true, sent));
            }
            if v["type"] == "command_lifecycle" {
                v["command_uuid"] = uuid.clone().into();
            }
            f.feed(&v.to_string());
            all.push((false, v.to_string()));
            queued_seen.push(f.queued.len());
        }
        (f, queued_seen, all)
    }

    #[test]
    fn a_prompt_sent_while_busy_waits_until_its_turn() {
        let (f, seen, _) = play_queued(QUEUED_NEXT, Priority::Next);
        assert!(seen.contains(&1) && *seen.last().unwrap() == 0);
        let order: Vec<&str> = f
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Prompt(_) => Some("prompt"),
                Block::Footer { .. } => Some("footer"),
                _ => None,
            })
            .collect();
        assert_eq!(order, ["prompt", "footer", "prompt", "footer"]);
        assert_eq!(replies(&f).last(), Some(&"queued"));
        assert_eq!(footers(&f)[0], Ending::Done);
        assert!(!f.busy);
    }

    /// Whether the Folio was busy after each CLI line that is a result.
    fn busy_after_results(stream: &str, priority: Priority) -> Vec<bool> {
        let (_, _, all) = play_queued(stream, priority);
        let mut f = Folio::new();
        let mut busy = Vec::new();
        for (sent, line) in &all {
            if *sent {
                f.feed_sent(line);
                continue;
            }
            f.feed(line);
            if line.contains("\"type\":\"result\"") {
                busy.push(f.busy);
            }
        }
        busy
    }

    #[test]
    fn a_turn_with_a_prompt_waiting_stays_busy_past_its_result() {
        for (stream, priority) in [(QUEUED_NEXT, Priority::Next), (QUEUED_NOW, Priority::Now)] {
            // The first result comes before the queued prompt starts.
            assert_eq!(busy_after_results(stream, priority), [true, false]);
        }
    }

    #[test]
    fn an_interrupt_that_drops_the_queue_ends_the_wait() {
        let (_, _, all) = play_queued(QUEUED_NEXT, Priority::Next);
        let mut f = Folio::new();
        for (sent, line) in &all {
            if line.contains("\"state\":\"started\"") {
                break;
            }
            if *sent {
                f.feed_sent(line);
            } else {
                f.feed(line);
            }
        }
        assert!(f.busy && f.queued.len() == 1);
        let uuid = f.queued[0].uuid.clone();
        let answer = |still: Value| {
            serde_json::json!({"type": "control_response", "response": {
                "subtype": "success", "request_id": "x", "response": {"still_queued": still}}})
            .to_string()
        };
        f.feed(&answer(serde_json::json!([{ "uuid": uuid }])));
        assert!(f.busy && f.queued.len() == 1);
        f.feed(&answer(serde_json::json!([])));
        assert!(!f.busy && f.queued.is_empty());
        assert_eq!(f.dropped[0].text, "Reply with exactly: queued");
    }

    #[test]
    fn a_prompt_sent_next_is_folded_into_the_turn_running() {
        let mut f = Folio::new();
        f.push_prompt("Look around");
        f.feed(r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#);
        let sent: Value =
            serde_json::from_str(&f.queue_prompt("Also check docs", &[], Priority::Next)).unwrap();
        assert_eq!(sent["priority"], "next");
        let uuid = sent["uuid"].as_str().unwrap().to_string();
        assert_eq!(f.queued.len(), 1);
        f.feed(r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"a"}]}}"#);
        f.feed(&serde_json::json!({"type": "command_lifecycle", "command_uuid": uuid, "state": "started"}).to_string());
        assert!(f.queued.is_empty() && f.busy);
        f.feed(r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"Docs too"}]}}"#);
        f.feed(r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":5,"total_cost_usd":0.1}"#);
        let order: Vec<&str> = (f.blocks.iter())
            .filter_map(|b| match b {
                Block::Prompt(p) => Some(p.as_str()),
                Block::Footer { .. } => Some("footer"),
                _ => None,
            })
            .collect();
        assert_eq!(order, ["Look around", "Also check docs", "footer"]);
        assert_eq!(replies(&f).last(), Some(&"Docs too"));
        assert!(!f.busy);
    }

    #[test]
    fn a_prompt_sent_now_cuts_the_turn_short() {
        let (f, _, _) = play_queued(QUEUED_NOW, Priority::Now);
        let endings: Vec<Ending> = footers(&f);
        assert_eq!(endings, [Ending::Stopped, Ending::Done]);
        assert_eq!(replies(&f).last(), Some(&"queued"));
        assert!(f.queued.is_empty());
    }

    #[test]
    fn a_prompt_sent_while_idle_is_a_plain_prompt() {
        let mut f = Folio::new();
        let v: Value = serde_json::from_str(&f.queue_prompt("hi", &[], Priority::Now)).unwrap();
        assert!(v.get("priority").is_none());
        assert!(f.busy && f.queued.is_empty());
    }

    #[test]
    fn compact_and_clear_are_notices() {
        let f = play_two(COMPACT, |v| v);
        let n = notices(&f);
        assert!(
            matches!(
                n[0],
                Notice::Compacted { trigger, pre_tokens: 36335, post_tokens: Some(14515) }
                    if trigger == "manual"
            ),
            "{n:?}"
        );
        assert_eq!(n[1], &Notice::CommandOutput("Compacted".into()));
        assert!(!f.blocks.iter().any(|b| matches!(b, Block::Raw(_))));

        let f = play_two(CLEAR, |v| v);
        assert_eq!(notices(&f), [&Notice::Cleared]);
        assert_eq!(
            f.session_id.as_deref(),
            Some("1fa7444a-1827-43b3-9f5c-c648973cfe3f"),
            "a new conversation after /clear"
        );
    }

    #[test]
    fn retries_are_one_notice_that_counts_up() {
        let f = play("p", API_RETRY);
        let n = notices(&f);
        assert_eq!(n.len(), 1, "{n:?}");
        assert!(matches!(
            n[0],
            Notice::Retrying {
                attempt: 7,
                max_retries: 10,
                status: None,
                ..
            }
        ));
        assert!(f.busy, "still trying");
    }

    #[test]
    fn effort_s_answer_is_a_reply_and_not_the_model() {
        let f = play_two(EFFORT, |v| v);
        assert!(replies(&f)
            .last()
            .unwrap()
            .starts_with("Set effort level to high"));
        assert_eq!(f.model.as_deref(), Some("claude-fable-5-1"));
    }

    #[test]
    fn todo_write_is_the_todo_list_not_a_step() {
        // TodoWrite is off in 2.1.312 as installed here, so this is the
        // documented input; the model has been seen sending it as a string.
        let mut f = Folio::new();
        f.push_prompt("p");
        let list = r#"[{"content":"say hi","status":"in_progress","activeForm":"Saying hi"},{"content":"say bye","status":"pending","activeForm":"Saying bye"}]"#;
        let line = serde_json::json!({"type": "assistant", "message": {"id": "m1", "content": [
            {"type": "tool_use", "id": "t1", "name": "TodoWrite", "input": {"todos": list}}]}});
        assert!(f.feed(&line.to_string()));
        assert_eq!(f.todos.len(), 2);
        assert_eq!(f.todos[0].status, TodoStatus::InProgress);
        assert!(runs(&f).is_empty());
        let done: Value = serde_json::from_str(&list.replace("in_progress", "completed")).unwrap();
        let line = serde_json::json!({"type": "assistant", "message": {"id": "m2", "content": [
            {"type": "tool_use", "id": "t2", "name": "TodoWrite", "input": {"todos": done}}]}});
        assert!(f.feed(&line.to_string()));
        assert_eq!(f.todos[0].status, TodoStatus::Completed, "latest wins");
        f.feed(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"}]}}"#);
        assert!(runs(&f).is_empty());
        f.feed(r#"{"type":"conversation_reset","trigger":"clear"}"#);
        assert!(f.todos.is_empty());
    }

    #[test]
    fn a_transcript_replays_as_history() {
        let f = Folio::from_transcript(TRANSCRIPT_EDITS);
        let prompts: Vec<&str> = f
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Prompt(p) => Some(p.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(prompts.len(), 2, "{:#?}", f.blocks);
        assert!(prompts[0].starts_with("Do these three tool calls"));
        assert_eq!(prompts[1], "Reply with exactly: resumed");
        assert_eq!(replies(&f), ["done", "resumed"]);
        let counts: Vec<String> = steps(&f)
            .iter()
            .filter_map(|s| s.edit.as_ref().map(FileEdit::counts))
            .collect();
        assert_eq!(counts, ["+2 -0", "+1 -1", "+1 -1"]);
        assert!(steps(&f).iter().all(|s| s.state == StepState::Done));
        assert_eq!(f.history, f.blocks.len());
        assert!(!f.busy);
        assert_eq!(
            f.session_id.as_deref(),
            Some("4792f003-15a6-47f5-8cdd-efd07abf2ea4")
        );
    }

    #[test]
    fn a_transcript_keeps_images_and_subagents() {
        let f = Folio::from_transcript(TRANSCRIPT_IMAGE);
        assert!(matches!(&f.blocks[0], Block::Prompt(p) if p.starts_with("What color")));
        assert!(matches!(&f.blocks[1], Block::Images(i) if i[0].size == Some((16, 16))));
        assert_eq!(replies(&f), ["Red"]);

        let f = Folio::from_transcript(TRANSCRIPT_SUBAGENT);
        let agent = steps(&f).into_iter().find(|s| s.tool == "Agent").unwrap();
        assert!(agent
            .lane
            .as_ref()
            .unwrap()
            .text
            .contains("A tiny scratch folder."));
    }

    /// Feeds a host's replay: each line sent (true) or from the CLI.
    fn replay(lines: &[(bool, String)]) -> Folio {
        let mut f = Folio::new();
        for (sent, line) in lines {
            if *sent {
                f.feed_sent(line);
            } else {
                f.feed(line);
            }
        }
        f
    }

    /// Plays `stream` live, answering its one ask with `answer`, and
    /// returns the Folio and every line in order, sent or received.
    fn live(
        prompt: &str,
        stream: &str,
        answer: impl Fn(&mut Folio, &str) -> String,
    ) -> (Folio, Vec<(bool, String)>) {
        let mut f = Folio::new();
        f.push_prompt(prompt);
        let mut all = vec![(true, prompt_line(prompt))];
        for line in stream.lines() {
            f.feed(line);
            all.push((false, line.to_string()));
            if let Some(id) = f.pending_asks().first().map(|s| s.to_string()) {
                all.push((true, answer(&mut f, &id)));
            }
        }
        (f, all)
    }

    #[test]
    fn a_replay_with_what_was_sent_rebuilds_the_folio() {
        let (f, all) = live("p", ASK_QUESTION, |f, id| {
            f.answer_question(id, &[vec!["Blue".into()], vec!["Large".into()]])
                .unwrap()
        });
        assert_eq!(replay(&all).blocks, f.blocks);

        let (f, all) = live("p", PLAN_KEEP, |f, id| f.keep_planning(id, KEEP).unwrap());
        let r = replay(&all);
        assert_eq!(r.blocks, f.blocks);
        assert_eq!(r.permission_mode(), "plan");

        let (f, all) = live("p", BASH_DENY, |f, id| f.answer(id, false).unwrap());
        let r = replay(&all);
        assert_eq!(r.blocks, f.blocks);
        assert!(r.blocks.iter().any(|b| matches!(
            b,
            Block::Ask {
                state: AskState::Denied,
                ..
            }
        )));

        let (f, _, all) = play_queued(QUEUED_NEXT, Priority::Next);
        assert_eq!(replay(&all).blocks, f.blocks);

        let mut f = Folio::new();
        let mut all = vec![(
            true,
            f.push_prompt_with_images("What color is this image?", &[red()]),
        )];
        all.extend(IMAGE.lines().map(|l| (false, l.to_string())));
        for l in IMAGE.lines() {
            f.feed(l);
        }
        assert_eq!(replay(&all).blocks, f.blocks);
    }

    #[test]
    fn a_replayed_control_request_takes_its_response() {
        let (f, sent) = controls();
        let ids: Vec<(&str, Value)> = ["init_1", "c1", "c2", "c3"]
            .into_iter()
            .zip(sent.iter().map(|l| request_id(l)))
            .collect();
        let mut r = Folio::new();
        for l in &sent {
            assert!(!r.feed_sent(l));
        }
        r.feed_sent(&prompt_line("Reply with exactly: pong"));
        for line in CONTROLS.lines() {
            r.feed(&ours(line, &ids));
        }
        assert_eq!(r.blocks, f.blocks);
        assert_eq!(r.permission_mode(), "plan");
        assert_eq!(r.model, f.model);
        assert_eq!(r.context, f.context);
        // A request made after the replay does not reuse an id.
        let next = request_id(&r.set_model("opus"));
        assert!(sent.iter().all(|l| request_id(l) != next));
    }
}
