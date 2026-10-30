//! The hirelings: the subagents a session sends out, each kept whole for
//! the tile's runes, the stage's retinue and the war table. See
//! docs/plan/hirelings.md.
//!
//! Three sources say what one is doing, and none says everything. The
//! mod's spawn names its type, task and parent, and its roster its status.
//! The hooks fired inside it carry its `agent_id`, so its tools are its
//! own. `SubagentStop` ends it with its transcript and its last words, mod
//! or not. Its transcript, read as it grows, gives what nothing else does:
//! how many tools, how many tokens, and when each tool ran.

use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::event::{HookEvent, ModAgent};

/// Where a hireling is on its errand. The order is the retinue's, most
/// in need of the human first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lot {
    /// Stopped on a permission prompt.
    Waiting,
    Failed,
    Working,
    Done,
}

impl Lot {
    pub fn at_work(self) -> bool {
        matches!(self, Lot::Working | Lot::Waiting)
    }

    fn from_status(status: &str) -> Option<Lot> {
        Some(match status {
            "pending" | "running" => Lot::Working,
            "waiting" => Lot::Waiting,
            "completed" | "idle" => Lot::Done,
            "failed" | "killed" => Lot::Failed,
            _ => return None,
        })
    }
}

/// One tool call, for the war table's ticks.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub tool: String,
    pub start: SystemTime,
    /// None while it runs.
    pub end: Option<SystemTime>,
    pub failed: bool,
}

/// One subagent of a session.
#[derive(Debug, Clone, PartialEq)]
pub struct Hireling {
    pub id: String,
    /// `Explore`, `general-purpose`, a plugin's agent, or "agent" when
    /// nothing said.
    pub kind: String,
    /// What it was asked to do, the spawn's description.
    pub task: String,
    pub model: Option<String>,
    /// The hireling that sent it; none when the main loop did.
    pub parent: Option<String>,
    pub lot: Lot,
    pub started: SystemTime,
    pub ended: Option<SystemTime>,
    /// When anything last came from it.
    pub heard: SystemTime,
    /// The tool it is in, between its PreToolUse and PostToolUse.
    pub tool: Option<String>,
    /// Whose word on its lot to believe: a roster's or a stop's beats the
    /// quiet guess.
    told: bool,
    pub tools: u32,
    pub tokens: u64,
    /// Its own transcript, once known.
    pub transcript: Option<String>,
    /// What it said last, from `SubagentStop`.
    pub report: Option<String>,
    pub marks: Vec<Mark>,
    /// How much of the transcript [`Band::tally`] has read, in bytes.
    pub read: u64,
}

impl Hireling {
    fn new(id: &str, now: SystemTime) -> Hireling {
        Hireling {
            id: id.to_string(),
            kind: "agent".to_string(),
            task: String::new(),
            model: None,
            parent: None,
            lot: Lot::Working,
            started: now,
            ended: None,
            heard: now,
            tool: None,
            told: false,
            tools: 0,
            tokens: 0,
            transcript: None,
            report: None,
            marks: Vec::new(),
            read: 0,
        }
    }

    /// Its lot as it stands at `now`: one that nothing has ever said the
    /// end of is taken as done once quiet for [`QUIET`].
    pub fn lot_at(&self, now: SystemTime) -> Lot {
        let quiet = now
            .duration_since(self.heard)
            .is_ok_and(|d| d >= QUIET);
        if self.lot == Lot::Working && !self.told && quiet && self.tool.is_none() {
            Lot::Done
        } else {
            self.lot
        }
    }

    /// How long it has run, or ran.
    pub fn ran(&self, now: SystemTime) -> Duration {
        let end = self.ended.unwrap_or(now);
        end.duration_since(self.started).unwrap_or_default()
    }

    /// What it is doing in a word: its tool, "waiting", "done" or
    /// "failed".
    pub fn doing(&self, now: SystemTime) -> String {
        match self.lot_at(now) {
            Lot::Waiting => match &self.tool {
                Some(t) => format!("waits on {t}"),
                None => "waiting".to_string(),
            },
            Lot::Working => self.tool.clone().unwrap_or_else(|| "thinking".to_string()),
            Lot::Done => format!("done, {} tools", self.tools),
            Lot::Failed => "failed".to_string(),
        }
    }

    /// One line on it: "Explore · find hooks · Grep · 42 s · 18k".
    pub fn summary(&self, now: SystemTime) -> String {
        let mut parts = vec![self.kind.clone()];
        if !self.task.is_empty() {
            parts.push(self.task.clone());
        }
        parts.push(self.doing(now));
        parts.push(crate::heavy::duration(self.ran(now).as_secs()));
        if self.tokens > 0 {
            parts.push(tokens(self.tokens));
        }
        parts.join(" \u{b7} ")
    }

    fn end(&mut self, lot: Lot, now: SystemTime) {
        self.lot = lot;
        self.told = true;
        self.tool = None;
        if self.ended.is_none() {
            self.ended = Some(now);
        }
        for m in self.marks.iter_mut().filter(|m| m.end.is_none()) {
            m.end = Some(now);
        }
    }
}

/// With no roster and no stop, a hireling this quiet has finished.
pub const QUIET: Duration = Duration::from_secs(30);
/// How long a finished hireling keeps its own row and rune.
pub const LINGER: Duration = Duration::from_secs(30);
/// Marks kept per track, so a turn of a thousand tools stays small.
const MAX_MARKS: usize = 400;

/// A session's hirelings and the main loop's own tools, since its last
/// prompt: what the war table draws. Not saved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Band {
    pub hirelings: Vec<Hireling>,
    /// The main loop's tool calls this turn.
    pub main: Vec<Mark>,
    /// When the turn began, the war table's left edge.
    pub turn: Option<SystemTime>,
    /// When the main loop's turn ended, if it has.
    pub turn_end: Option<SystemTime>,
    /// The session's transcript, whose folder holds the hirelings' own.
    pub transcript: Option<String>,
}

impl Band {
    /// Takes in whatever a hook or the mod said.
    pub fn hear(&mut self, e: &HookEvent, now: SystemTime) {
        if !e.transcript_path.is_empty() && !e.is_subagent() {
            self.transcript = Some(e.transcript_path.clone());
        }
        let name = e.hook_event_name.as_str();
        match name {
            HookEvent::AGENTS => {
                for a in e.agents.iter().flatten() {
                    self.roster(a, now);
                }
                return;
            }
            HookEvent::SPAWN | "SubagentStart" => {
                let Some(id) = e.agent_id.as_deref().filter(|i| !i.is_empty()) else {
                    return;
                };
                let h = self.get(id, now);
                if let Some(t) = e.agent_type.as_deref().filter(|t| !t.is_empty()) {
                    h.kind = t.to_string();
                }
                if let Some(d) = e.description.as_deref().filter(|d| !d.is_empty()) {
                    h.task = d.trim().to_string();
                }
                if e.model.is_some() {
                    h.model = e.model.clone();
                }
                if let Some(p) = e.parent_id.as_deref().filter(|p| !p.is_empty()) {
                    h.parent = Some(p.to_string());
                }
                return;
            }
            _ => {}
        }
        if let Some(id) = e.agent_id.as_deref().filter(|i| !i.is_empty()) {
            let h = self.get(id, now);
            h.heard = now;
            if let Some(t) = e.agent_type.as_deref().filter(|t| !t.is_empty()) {
                if h.kind == "agent" {
                    h.kind = t.to_string();
                }
            }
            match name {
                "PreToolUse" => {
                    h.tool = e.tool_name.clone();
                    if h.lot == Lot::Waiting {
                        h.lot = Lot::Working;
                    }
                }
                "PostToolUse" | "PostToolUseFailure" => {
                    h.tool = None;
                    if h.lot == Lot::Waiting {
                        h.lot = Lot::Working;
                    }
                }
                "PermissionRequest" => {
                    h.lot = Lot::Waiting;
                    if e.tool_name.is_some() {
                        h.tool = e.tool_name.clone();
                    }
                }
                "SubagentStop" => {
                    if let Some(p) = e.agent_transcript_path.as_deref().filter(|p| !p.is_empty()) {
                        h.transcript = Some(p.to_string());
                    }
                    if let Some(m) = &e.last_assistant_message {
                        h.report = Some(m.clone());
                    }
                    let lot = if h.lot == Lot::Failed {
                        Lot::Failed
                    } else {
                        Lot::Done
                    };
                    h.end(lot, now);
                }
                _ => {}
            }
            return;
        }
        match name {
            "UserPromptSubmit" => {
                // A new turn: what finished is history, what still works
                // stays on the table.
                self.hirelings.retain(|h| h.lot_at(now).at_work());
                self.main.clear();
                self.turn = Some(now);
                self.turn_end = None;
            }
            "PreToolUse" => {
                self.turn.get_or_insert(now);
                if let Some(t) = &e.tool_name {
                    push(
                        &mut self.main,
                        Mark {
                            tool: t.clone(),
                            start: now,
                            end: None,
                            failed: false,
                        },
                    );
                }
            }
            "PostToolUse" | "PostToolUseFailure" => {
                let failed = name == "PostToolUseFailure";
                if let Some(m) = self
                    .main
                    .iter_mut()
                    .rev()
                    .find(|m| m.end.is_none() && Some(&m.tool) == e.tool_name.as_ref())
                {
                    m.end = Some(now);
                    m.failed = failed;
                }
            }
            "Stop" | "StopFailure" | "Interrupt" | "StopCancelled" => {
                self.turn_end = Some(now);
                for m in self.main.iter_mut().filter(|m| m.end.is_none()) {
                    m.end = Some(now);
                }
            }
            "SessionStart" | "SessionEnd" | HookEvent::REGISTER | HookEvent::PAUSE => {
                // The process is gone, and its hirelings with it.
                for h in self.hirelings.iter_mut().filter(|h| h.lot.at_work()) {
                    h.end(Lot::Failed, now);
                }
                if name != "SessionEnd" {
                    *self = Band {
                        transcript: self.transcript.take(),
                        ..Band::default()
                    };
                }
            }
            _ => {}
        }
    }

    fn roster(&mut self, a: &ModAgent, now: SystemTime) {
        let h = self.get(&a.agent_id, now);
        if !a.agent_type.is_empty() {
            h.kind = a.agent_type.clone();
        }
        if !a.description.is_empty() {
            h.task = a.description.trim().to_string();
        }
        if a.parent_id.is_some() {
            h.parent = a.parent_id.clone();
        }
        if let Some(ms) = a.started_ms {
            h.started = SystemTime::UNIX_EPOCH + Duration::from_millis(ms);
        }
        match Lot::from_status(&a.status) {
            Some(l @ (Lot::Done | Lot::Failed)) if h.lot.at_work() || !h.told => h.end(l, now),
            Some(l @ (Lot::Working | Lot::Waiting)) => {
                h.lot = l;
                h.told = true;
                h.ended = None;
            }
            _ => {}
        }
    }

    fn get(&mut self, id: &str, now: SystemTime) -> &mut Hireling {
        let i = match self.hirelings.iter().position(|h| h.id == id) {
            Some(i) => i,
            None => {
                self.turn.get_or_insert(now);
                self.hirelings.push(Hireling::new(id, now));
                self.hirelings.len() - 1
            }
        };
        &mut self.hirelings[i]
    }

    /// The hirelings to show at `now`: every one at work, failed ones and
    /// those that finished within [`LINGER`], most in need first, then by
    /// when they started. With `all`, every one this turn.
    pub fn shown(&self, now: SystemTime, all: bool) -> Vec<&Hireling> {
        let mut out: Vec<&Hireling> = self
            .hirelings
            .iter()
            .filter(|h| {
                all || match h.lot_at(now) {
                    Lot::Working | Lot::Waiting => true,
                    Lot::Failed | Lot::Done => h
                        .ended
                        .unwrap_or(h.heard)
                        .checked_add(LINGER)
                        .is_some_and(|t| t > now),
                }
            })
            .collect();
        out.sort_by(|a, b| {
            a.lot_at(now)
                .cmp(&b.lot_at(now))
                .then(a.started.cmp(&b.started))
        });
        out
    }

    /// How many are at work, and how many there are this turn.
    pub fn count(&self, now: SystemTime) -> (usize, usize) {
        let working = self
            .hirelings
            .iter()
            .filter(|h| h.lot_at(now).at_work())
            .count();
        (working, self.hirelings.len())
    }

    /// Whether one failed this turn, so the tile does not look all well
    /// when its main turn says so.
    pub fn any_failed(&self) -> bool {
        self.hirelings.iter().any(|h| h.lot == Lot::Failed)
    }

    /// The one waiting on a permission, if any, for the parent's prompt
    /// to name.
    pub fn asking(&self) -> Option<&Hireling> {
        self.hirelings.iter().find(|h| h.lot == Lot::Waiting)
    }

    /// The tile's line about them, if there is anything to say: the kinds
    /// at work, with how many of the turn's are left when some are done,
    /// "2/3 · Explore x2", or the one that failed.
    pub fn line(&self, now: SystemTime) -> Option<String> {
        let shown = self.shown(now, false);
        let working: Vec<String> = shown
            .iter()
            .filter(|h| h.lot_at(now).at_work())
            .map(|h| h.kind.clone())
            .collect();
        if working.is_empty() {
            return shown
                .iter()
                .find(|h| h.lot_at(now) == Lot::Failed)
                .map(|h| match h.task.as_str() {
                    "" => format!("{} failed", h.kind),
                    task => format!("{} failed: {task}", h.kind),
                });
        }
        let kinds = crate::session::subagent_line(&working);
        let total = self.hirelings.len();
        Some(if total > working.len() {
            format!("{}/{total} \u{b7} {kinds}", working.len())
        } else {
            kinds
        })
    }

    /// A line per hireling shown, for hovering the tile: its kind, task,
    /// what it is doing, how long it ran and its tokens.
    pub fn list(&self, now: SystemTime) -> String {
        self.shown(now, false)
            .iter()
            .map(|h| h.summary(now))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Where a hireling's transcript is: what `SubagentStop` said, or
    /// beside the session's own, `<id>/subagents/agent-<agent>.jsonl`.
    pub fn transcript_of(&self, h: &Hireling) -> Option<String> {
        if let Some(t) = &h.transcript {
            return Some(t.clone());
        }
        let main = self.transcript.as_deref()?;
        let stem = main.strip_suffix(".jsonl")?;
        Some(format!("{stem}/subagents/agent-{}.jsonl", h.id))
    }

    /// Reads what was added to a hireling's transcript, `more` being the
    /// file from byte `h.read` on, for its tokens, tool count and marks.
    /// Only whole lines are taken; a half written last one waits.
    pub fn tally(&mut self, id: &str, more: &str) {
        let Some(h) = self.hirelings.iter_mut().find(|h| h.id == id) else {
            return;
        };
        let whole = match more.rfind('\n') {
            Some(i) => &more[..=i],
            None => return,
        };
        h.read += whole.len() as u64;
        for line in whole.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let at = v["timestamp"].as_str().and_then(parse_time);
            if let Some(at) = at {
                if at < h.started {
                    h.started = at;
                }
            }
            let content = &v["message"]["content"];
            match v["type"].as_str() {
                Some("assistant") => {
                    let u = &v["message"]["usage"];
                    let n = |k: &str| u[k].as_u64().unwrap_or(0);
                    // The context it holds now, as Claude Code counts a
                    // subagent's tokens: its latest call, not a sum.
                    let held = n("input_tokens")
                        + n("cache_creation_input_tokens")
                        + n("cache_read_input_tokens")
                        + n("output_tokens");
                    if held > 0 {
                        h.tokens = held;
                    }
                    for b in content.as_array().into_iter().flatten() {
                        if b["type"] == "tool_use" {
                            h.tools += 1;
                            let tool = b["name"].as_str().unwrap_or("tool").to_string();
                            if let Some(at) = at {
                                push(
                                    &mut h.marks,
                                    Mark {
                                        tool,
                                        start: at,
                                        end: None,
                                        failed: false,
                                    },
                                );
                            }
                        }
                    }
                }
                Some("user") => {
                    for b in content.as_array().into_iter().flatten() {
                        if b["type"] != "tool_result" {
                            continue;
                        }
                        let failed = b["is_error"] == true;
                        if let (Some(at), Some(m)) =
                            (at, h.marks.iter_mut().find(|m| m.end.is_none()))
                        {
                            m.end = Some(at);
                            m.failed = failed;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn push(marks: &mut Vec<Mark>, m: Mark) {
    if marks.len() >= MAX_MARKS {
        marks.remove(0);
    }
    marks.push(m);
}

/// What kind of work a tool does, for the war table's colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Craft {
    Read,
    Edit,
    Shell,
    Web,
    /// Sending a hireling, or waiting on one.
    Agent,
    Other,
}

/// The kind of work tool `name` does. An MCP tool is judged by its own
/// name, past `mcp__server__`.
pub fn craft(name: &str) -> Craft {
    let own = name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .map_or(name, |(_, t)| t);
    match own {
        "Read" | "Glob" | "Grep" | "LS" | "NotebookRead" | "LSP" => Craft::Read,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => Craft::Edit,
        "Bash" | "PowerShell" | "BashOutput" | "KillShell" | "Monitor" => Craft::Shell,
        "WebFetch" | "WebSearch" => Craft::Web,
        "Agent" | "Task" | "TaskStop" | "SendMessage" => Craft::Agent,
        t if t.starts_with("browser_") || t.contains("fetch") || t.contains("search") => {
            Craft::Web
        }
        _ => Craft::Other,
    }
}

/// Which of the twelve runes stands for a kind of hireling: the common
/// ones chosen, the rest by a hash of the name, so a kind keeps its rune.
pub fn rune_of(kind: &str) -> usize {
    match kind {
        // Raidho, the journey.
        "Explore" => 4,
        // Ansuz, the word.
        "Plan" => 3,
        // Fehu, the hired hand.
        "general-purpose" | "agent" => 0,
        _ => {
            let h = kind.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
            });
            (h % 12) as usize
        }
    }
}

/// `2026-10-30T22:02:05.761Z` as a time.
pub fn parse_time(s: &str) -> Option<SystemTime> {
    let (head, frac) = match s.split_once('.') {
        Some((h, f)) => (h.to_string() + "Z", f.trim_end_matches('Z')),
        None => (s.to_string(), ""),
    };
    let secs = crate::tasks::parse_utc(&head)?;
    let digits: String = frac.chars().take(3).collect();
    let ms = match digits.len() {
        0 => 0,
        n => digits.parse::<u64>().ok()? * 10u64.pow(3 - n as u32),
    };
    Some(SystemTime::UNIX_EPOCH + Duration::from_millis(secs * 1000 + ms))
}

/// A token count as the retinue shows it: `850`, `18k`, `1.2M`.
pub fn tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{}k", (n + 500) / 1000),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + s)
    }

    fn ev(name: &str) -> HookEvent {
        HookEvent::synthetic(name)
    }

    fn sub(name: &str, id: &str) -> HookEvent {
        HookEvent {
            agent_id: Some(id.into()),
            ..ev(name)
        }
    }

    #[test]
    fn a_spawn_names_it_and_its_tools_are_its_own() {
        let mut b = Band::default();
        b.hear(&ev("UserPromptSubmit"), t(0));
        let mut s = sub(HookEvent::SPAWN, "a1");
        s.agent_type = Some("Explore".into());
        s.description = Some("find hooks".into());
        b.hear(&s, t(1));
        let mut pre = sub("PreToolUse", "a1");
        pre.tool_name = Some("Grep".into());
        b.hear(&pre, t(2));
        let mut main = ev("PreToolUse");
        main.tool_name = Some("Read".into());
        b.hear(&main, t(2));
        let h = &b.hirelings[0];
        assert_eq!((h.kind.as_str(), h.task.as_str()), ("Explore", "find hooks"));
        assert_eq!(h.tool.as_deref(), Some("Grep"));
        assert_eq!(b.main.len(), 1);
        assert_eq!(b.main[0].tool, "Read");
    }

    #[test]
    fn a_quiet_one_is_done_but_not_while_in_a_tool_or_told_otherwise() {
        let mut b = Band::default();
        b.hear(&sub("PostToolUse", "a1"), t(0));
        assert_eq!(b.hirelings[0].lot_at(t(40)), Lot::Done);
        let mut pre = sub("PreToolUse", "a1");
        pre.tool_name = Some("Bash".into());
        b.hear(&pre, t(0));
        assert_eq!(b.hirelings[0].lot_at(t(40)), Lot::Working);
    }

    #[test]
    fn the_roster_ends_one_and_a_failure_shows_until_the_next_prompt() {
        let mut b = Band::default();
        let mut r = ev(HookEvent::AGENTS);
        r.agents = Some(vec![ModAgent {
            agent_id: "a1".into(),
            agent_type: "Plan".into(),
            status: "running".into(),
            ..Default::default()
        }]);
        b.hear(&r, t(0));
        assert_eq!(b.count(t(100)), (1, 1));
        r.agents.as_mut().unwrap()[0].status = "failed".into();
        b.hear(&r, t(5));
        assert!(b.any_failed());
        assert_eq!(b.shown(t(10), false).len(), 1);
        assert!(b.shown(t(60), false).is_empty());
        b.hear(&ev("UserPromptSubmit"), t(70));
        assert!(b.hirelings.is_empty());
    }

    #[test]
    fn waiting_ones_come_first() {
        let mut b = Band::default();
        b.hear(&sub("PostToolUse", "a1"), t(0));
        let mut ask = sub("PermissionRequest", "a2");
        ask.tool_name = Some("Bash".into());
        b.hear(&ask, t(1));
        let ids: Vec<&str> = b.shown(t(2), false).iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["a2", "a1"]);
        assert_eq!(b.asking().map(|h| h.id.as_str()), Some("a2"));
    }

    #[test]
    fn a_transcript_gives_tokens_tools_and_marks() {
        let mut b = Band::default();
        b.hear(&sub("PreToolUse", "a1"), t(0));
        let jsonl = concat!(
            r#"{"type":"assistant","timestamp":"2027-01-15T08:00:01.500Z","message":{"usage":{"input_tokens":2,"cache_read_input_tokens":1000,"output_tokens":50},"content":[{"type":"tool_use","name":"Grep"}]}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2027-01-15T08:00:03Z","message":{"content":[{"type":"tool_result","is_error":true}]}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2027-01-15T08:00:04Z","message":{"cont"#,
        );
        b.tally("a1", jsonl);
        let h = &b.hirelings[0];
        assert_eq!((h.tools, h.tokens), (1, 1052));
        let m = &h.marks[0];
        assert!(m.failed);
        let ran = m.end.unwrap().duration_since(m.start).unwrap();
        assert_eq!(ran, Duration::from_millis(1500));
        assert!(h.read < jsonl.len() as u64);
    }

    #[test]
    fn its_transcript_sits_beside_the_sessions() {
        let mut b = Band::default();
        let mut e = ev("UserPromptSubmit");
        e.transcript_path = "C:/p/abc.jsonl".into();
        b.hear(&e, t(0));
        b.hear(&sub("PostToolUse", "x9"), t(1));
        assert_eq!(
            b.transcript_of(&b.hirelings[0]).as_deref(),
            Some("C:/p/abc/subagents/agent-x9.jsonl")
        );
    }

    #[test]
    fn the_tiles_line_counts_whats_left_and_names_a_failure() {
        let mut b = Band::default();
        for id in ["a1", "a2", "a3"] {
            let mut s = sub(HookEvent::SPAWN, id);
            s.agent_type = Some("Explore".into());
            b.hear(&s, t(0));
        }
        let mut stop = sub("SubagentStop", "a3");
        stop.agent_type = Some("Explore".into());
        b.hear(&stop, t(1));
        assert_eq!(b.line(t(2)).as_deref(), Some("2/3 \u{b7} Explore x2"));
        let mut r = ev(HookEvent::AGENTS);
        r.agents = Some(
            ["a1", "a2"]
                .map(|id| ModAgent {
                    agent_id: id.into(),
                    status: "failed".into(),
                    description: "look".into(),
                    ..Default::default()
                })
                .to_vec(),
        );
        b.hear(&r, t(3));
        assert_eq!(b.line(t(4)).as_deref(), Some("Explore failed: look"));
    }

    #[test]
    fn tools_are_sorted_by_their_craft() {
        assert_eq!(craft("Grep"), Craft::Read);
        assert_eq!(craft("Write"), Craft::Edit);
        assert_eq!(craft("PowerShell"), Craft::Shell);
        assert_eq!(craft("mcp__claude_ai_Slack__slack_search_public"), Craft::Web);
        assert_eq!(craft("Agent"), Craft::Agent);
        assert_eq!(craft("TodoWrite"), Craft::Other);
    }

    #[test]
    fn token_counts_read_short() {
        assert_eq!(tokens(850), "850");
        assert_eq!(tokens(18_400), "18k");
        assert_eq!(tokens(1_240_000), "1.2M");
    }
}
