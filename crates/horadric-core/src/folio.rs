//! The Folio's model: a headless Claude session's stream-json lines in, the
//! conversation as blocks out. No I/O, so every folding rule is tested
//! against recorded streams. See docs/plan/headless.md.

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
}

impl Folio {
    pub fn new() -> Self {
        Self::default()
    }

    /// The human sent a prompt.
    pub fn push_prompt(&mut self, text: &str) {
        let _ = text;
        todo!()
    }

    /// One stdout line from the CLI. Returns whether anything visible changed.
    pub fn feed(&mut self, line: &str) -> bool {
        let _ = line;
        todo!()
    }

    /// Marks an ask answered and returns the `control_response` line to
    /// write to the CLI's stdin, or None if no such ask is pending.
    pub fn answer(&mut self, request_id: &str, allow: bool) -> Option<String> {
        let _ = (request_id, allow);
        todo!()
    }
}
