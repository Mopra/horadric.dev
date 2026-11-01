//! Compacting a quest's conversation once, between its exploration and its
//! edits.
//!
//! A quest's spend is its window times its calls, and a quest reads first
//! and edits after: the window it carries through the edits is mostly the
//! files it read to decide them. Summed up there, the edits go on from a
//! summary instead. The moment is told from the stream by the first edit
//! the main conversation starts: by then the agent has decided what to
//! change, and the reading that filled the window is behind it.
//!
//! The decision is [`Watch::step`], fed the stream's lines by
//! [`Watch::feed`] and told where the session stands by a [`Moment`]. It
//! stops the turn, sends `/compact` once the turn has ended, then a prompt
//! to go on once the compaction's turn has ended.

use std::collections::HashSet;

use serde_json::Value;

use crate::rarity::WRITERS;

/// The window, in tokens, at which compacting is worth a call. A quest
/// starts at about 55k (system prompt, tools, CLAUDE.md, the quest and its
/// pack) and its median call carries 100k to 110k (measured 2026-10-17 over
/// 103 quests, again 2026-11-01 over 125). A compaction goes on from about
/// 40k, so below this it would buy back little and cost a call that reads
/// the whole window and writes a summary.
pub const COMPACT_AT: u64 = 100_000;

/// What the compaction is told to keep: what the edits need, not what was
/// read on the way to them.
pub const COMPACT_PROMPT: &str = "/compact You have explored and are about to edit. Keep the \
     quest, its plan, every file and function you mean to change and how, the facts the edits \
     depend on, and what is left to do. Leave out the contents of files you only read.";

/// The prompt after the compaction, since stopping the turn ended the work.
pub const GO_ON_PROMPT: &str = "The conversation was compacted between your exploration and \
     your edits, so you stopped. Go on with the quest where you left off: make the edits (read \
     a file again where you need its exact text), and report the quest done as before.";

/// Where the compaction of one quest's conversation stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Stage {
    /// Waiting for the moment.
    #[default]
    Watching,
    /// The turn was told to stop, `/compact` goes once it has.
    Stopping,
    /// `/compact` went, the prompt to go on goes once its turn has ended.
    Compacting,
    /// Compacted, given up, or never to be: nothing more is done.
    Done,
}

/// What the session is to be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Stop the turn, as Esc does.
    Interrupt,
    /// Send this prompt.
    Send(&'static str),
}

/// Where the session stands, from outside the stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Moment {
    /// It holds a quest. Nothing else is ever compacted: a human's own
    /// session is theirs to compact.
    pub quest: bool,
    /// Its stream is live and it can take a prompt, not a replay or a
    /// history still loading.
    pub live: bool,
    /// A turn runs.
    pub busy: bool,
    /// An ask waits on the human.
    pub asking: bool,
    /// Background agents at work, whose results a stopped turn would
    /// leave hanging.
    pub agents: usize,
}

/// What the stream said that the decision needs.
#[derive(Debug, Clone, Default)]
pub struct Watch {
    pub stage: Stage,
    /// The main conversation's window at its latest call, in tokens.
    pub window: u64,
    /// The main conversation has started an edit.
    pub editing: bool,
    /// The conversation was compacted already, by anyone: once is enough.
    pub compacted: bool,
    /// The human typed into the session: it is theirs now.
    pub human: bool,
    /// The main conversation's tool uses whose input is whole and whose
    /// result has not come: they may be running, and are not cut off.
    running: HashSet<String>,
}

impl Watch {
    /// Takes in one line of the CLI's stream. A subagent's lines are its
    /// own window and its own edits, and say nothing of the main one.
    pub fn feed(&mut self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            return;
        };
        if v["parent_tool_use_id"].is_string() {
            return;
        }
        match v["type"].as_str().unwrap_or_default() {
            "stream_event" => {
                let e = &v["event"];
                match e["type"].as_str().unwrap_or_default() {
                    "message_start" => self.usage(&e["message"]["usage"]),
                    // The edit starts as its input streams, before it can
                    // run: the earliest moment there is.
                    "content_block_start" => self.tool(&e["content_block"], false),
                    _ => {}
                }
            }
            "assistant" => {
                self.usage(&v["message"]["usage"]);
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    self.tool(block, true);
                }
            }
            "user" => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        if let Some(id) = block["tool_use_id"].as_str() {
                            self.running.remove(id);
                        }
                    }
                }
            }
            "result" => self.running.clear(),
            "system" if v["subtype"] == "compact_boundary" => self.compacted = true,
            _ => {}
        }
    }

    fn usage(&mut self, u: &Value) {
        let n = |k: &str| u[k].as_u64().unwrap_or_default();
        let window =
            n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
        if window > 0 {
            self.window = window;
        }
    }

    /// A tool use seen streaming, or whole (`whole`), when it may run.
    fn tool(&mut self, block: &Value, whole: bool) {
        if block["type"] != "tool_use" {
            return;
        }
        let name = block["name"].as_str().unwrap_or_default();
        if WRITERS.contains(&name) {
            self.editing = true;
        }
        if let (true, Some(id)) = (whole, block["id"].as_str()) {
            self.running.insert(id.to_string());
        }
    }

    /// Whether this is the moment: a quest's live turn, its first edit
    /// started, no tool of it that may be running, nothing waiting on the
    /// human or on an agent, a window worth compacting, and never before.
    pub fn due(&self, m: &Moment) -> bool {
        self.stage == Stage::Watching
            && m.quest
            && m.live
            && m.busy
            && !m.asking
            && m.agents == 0
            && !self.human
            && !self.compacted
            && self.editing
            && self.running.is_empty()
            && self.window >= COMPACT_AT
    }

    /// What to do now, if anything, moving on a stage when it is done.
    pub fn step(&mut self, m: &Moment) -> Option<Act> {
        match self.stage {
            Stage::Watching if self.due(m) => {
                self.stage = Stage::Stopping;
                Some(Act::Interrupt)
            }
            Stage::Stopping | Stage::Compacting if self.human || !m.live => {
                self.stage = Stage::Done;
                None
            }
            Stage::Stopping if !m.busy => {
                self.stage = Stage::Compacting;
                Some(Act::Send(COMPACT_PROMPT))
            }
            Stage::Compacting if !m.busy => {
                self.stage = Stage::Done;
                Some(Act::Send(GO_ON_PROMPT))
            }
            _ => None,
        }
    }

    /// What [`Watch::step`] asked for could not be done: no second try.
    pub fn give_up(&mut self) {
        self.stage = Stage::Done;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn quest(busy: bool) -> Moment {
        Moment {
            quest: true,
            live: true,
            busy,
            ..Default::default()
        }
    }

    fn call(w: &mut Watch, window: u64) {
        w.feed(
            &json!({"type": "stream_event", "event": {"type": "message_start",
                "message": {"usage": {"input_tokens": 2, "cache_creation_input_tokens": 1000,
                "cache_read_input_tokens": window - 1002}}}})
            .to_string(),
        );
    }

    fn start(w: &mut Watch, id: &str, tool: &str) {
        w.feed(
            &json!({"type": "stream_event", "event": {"type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": id, "name": tool, "input": {}}}})
            .to_string(),
        );
    }

    fn whole(w: &mut Watch, id: &str, tool: &str) {
        w.feed(
            &json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": id, "name": tool, "input": {}}]}})
            .to_string(),
        );
    }

    fn result(w: &mut Watch, id: &str) {
        w.feed(
            &json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": id, "content": "ok"}]}})
            .to_string(),
        );
    }

    /// A quest that read two files at a large window.
    fn explored(window: u64) -> Watch {
        let mut w = Watch::default();
        for id in ["r1", "r2"] {
            call(&mut w, window);
            start(&mut w, id, "Read");
            whole(&mut w, id, "Read");
            result(&mut w, id);
        }
        w
    }

    #[test]
    fn reading_alone_is_never_the_moment() {
        let mut w = explored(150_000);
        assert_eq!(w.step(&quest(true)), None);
    }

    #[test]
    fn the_first_edit_streaming_at_a_large_window_stops_the_turn() {
        let mut w = explored(150_000);
        start(&mut w, "e1", "Edit");
        assert_eq!(w.step(&quest(true)), Some(Act::Interrupt));
        assert_eq!(w.stage, Stage::Stopping);
    }

    #[test]
    fn a_small_window_is_left_alone() {
        let mut w = explored(COMPACT_AT - 1);
        start(&mut w, "e1", "Edit");
        assert_eq!(w.step(&quest(true)), None);
    }

    #[test]
    fn a_tool_that_may_be_running_is_waited_out() {
        let mut w = explored(150_000);
        call(&mut w, 150_000);
        whole(&mut w, "b1", "Bash");
        start(&mut w, "e1", "Write");
        assert_eq!(w.step(&quest(true)), None, "the Bash may run");
        whole(&mut w, "e1", "Write");
        result(&mut w, "b1");
        assert_eq!(w.step(&quest(true)), None, "the Write may run");
        result(&mut w, "e1");
        assert_eq!(w.step(&quest(true)), Some(Act::Interrupt));
    }

    #[test]
    fn a_subagent_s_edits_and_window_are_not_the_main_one_s() {
        let mut w = explored(50_000);
        w.feed(
            &json!({"type": "assistant", "parent_tool_use_id": "a1", "message": {
                "usage": {"input_tokens": 200_000},
                "content": [{"type": "tool_use", "id": "x", "name": "Edit", "input": {}}]}})
            .to_string(),
        );
        assert!(!w.editing);
        assert_eq!(w.window, 50_000);
    }

    #[test]
    fn only_a_live_unattended_quest_with_nothing_waiting_is_compacted() {
        let mut ready = explored(150_000);
        start(&mut ready, "e1", "Edit");
        let not = [
            Moment {
                quest: false,
                ..quest(true)
            },
            Moment {
                live: false,
                ..quest(true)
            },
            Moment {
                asking: true,
                ..quest(true)
            },
            Moment {
                agents: 1,
                ..quest(true)
            },
            quest(false),
        ];
        for m in not {
            assert_eq!(ready.clone().step(&m), None, "{m:?}");
        }
        let mut theirs = ready.clone();
        theirs.human = true;
        assert_eq!(theirs.step(&quest(true)), None);
    }

    #[test]
    fn it_stops_compacts_and_goes_on_once() {
        let mut w = explored(150_000);
        start(&mut w, "e1", "Edit");
        assert_eq!(w.step(&quest(true)), Some(Act::Interrupt));
        assert_eq!(w.step(&quest(true)), None, "the turn has not ended yet");
        w.feed(r#"{"type":"result","subtype":"success"}"#);
        assert_eq!(w.step(&quest(false)), Some(Act::Send(COMPACT_PROMPT)));
        assert_eq!(w.step(&quest(true)), None, "compacting");
        w.feed(r#"{"type":"system","subtype":"compact_boundary"}"#);
        assert_eq!(w.step(&quest(false)), Some(Act::Send(GO_ON_PROMPT)));
        assert_eq!(w.stage, Stage::Done);
        start(&mut w, "e2", "Edit");
        call(&mut w, 200_000);
        assert_eq!(w.step(&quest(true)), None, "once per quest");
    }

    #[test]
    fn a_conversation_compacted_before_is_not_compacted_again() {
        let mut w = explored(150_000);
        w.feed(r#"{"type":"system","subtype":"compact_boundary"}"#);
        call(&mut w, 150_000);
        start(&mut w, "e1", "Edit");
        assert_eq!(w.step(&quest(true)), None);
    }

    #[test]
    fn the_human_stepping_in_ends_it() {
        let mut w = explored(150_000);
        start(&mut w, "e1", "Edit");
        assert_eq!(w.step(&quest(true)), Some(Act::Interrupt));
        w.human = true;
        assert_eq!(w.step(&quest(false)), None);
        assert_eq!(w.stage, Stage::Done);
    }
}
