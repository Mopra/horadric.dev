//! Claude Code's background sessions: the ones `claude --bg` starts, and
//! the ones the left arrow sends to its agent view. A shared daemon runs
//! them, so no Horadric terminal holds them. They get a tile of their own,
//! and a click attaches to them.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::session::{Phase, Session, WaitReason};

/// One background session, as `claude agents --json` lists it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Background {
    /// The short id `claude attach` and `claude stop` take.
    pub id: String,
    /// Claude's own session id, the one its hooks carry.
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub cwd: String,
    #[serde(default)]
    pub name: Option<String>,
    /// What it is doing: `working`, `done` and so on.
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    kind: String,
}

impl Background {
    /// The tile for it. Its id stays the same for the same session, so a
    /// second sighting finds the first tile.
    pub fn session(&self) -> Session {
        let mut s = Session::new(tile_id(&self.id), self.shown_name(), self.cwd.clone());
        s.background = Some(self.id.clone());
        s.claude_session_id = Some(self.session_id.clone());
        s.phase = self.phase();
        s
    }

    fn shown_name(&self) -> String {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(&self.id)
            .to_string()
    }

    /// Its phase before any hook said, from the list. States the list may
    /// grow later count as idle.
    pub fn phase(&self) -> Phase {
        match self.state.as_deref() {
            Some("working") => Phase::Working,
            Some("done") => Phase::Done,
            Some("waiting" | "blocked" | "needs_input") => Phase::Waiting(WaitReason::Input),
            _ => Phase::Idle,
        }
    }
}

/// The Horadric id of the tile for the background session with this short
/// id.
pub fn tile_id(short: &str) -> String {
    format!("bg-{short}")
}

/// Whether a Horadric id is a background session's tile, for what knows
/// only the id, such as a line of the journal.
pub fn is_tile(id: &str) -> bool {
    id.strip_prefix("bg-")
        .is_some_and(|short| !short.is_empty())
}

/// When each conversation no tile holds was last asked about. Every plain
/// `claude` on the machine posts its hooks to Horadric, untagged, and
/// asking `claude agents` on each would start a process a hook.
#[derive(Debug, Default)]
pub struct Asked {
    at: HashMap<String, Instant>,
    last: Option<Instant>,
}

impl Asked {
    /// Long enough that a busy plain session costs a process a minute,
    /// short enough that one sent to the background shows soon after.
    pub const AGAIN: Duration = Duration::from_secs(60);

    /// Between any two asks, whatever conversation they are for. A web
    /// page can post hooks with a new id each time, and without this each
    /// would start a node process.
    pub const GAP: Duration = Duration::from_secs(10);

    /// True when `id` is due to be asked about, which marks it asked. One
    /// turned away by [`Self::GAP`] is not marked, so its next hook asks.
    pub fn due(&mut self, id: &str, now: Instant) -> bool {
        self.at
            .retain(|_, t| now.saturating_duration_since(*t) < Self::AGAIN);
        if self.at.contains_key(id) {
            return false;
        }
        if self
            .last
            .is_some_and(|t| now.saturating_duration_since(t) < Self::GAP)
        {
            return false;
        }
        self.at.insert(id.to_string(), now);
        self.last = Some(now);
        true
    }
}

/// The hooks of conversations no tile held when they came, kept until
/// `claude agents` has been asked whether they are background sessions.
/// The first hook of a new one is often the one that matters, a
/// permission prompt or a turn's end, and the tile it would have lit only
/// exists once the answer is in.
#[derive(Debug)]
pub struct Held<T> {
    by_id: Vec<(String, Vec<(Instant, T)>)>,
}

impl<T> Default for Held<T> {
    fn default() -> Self {
        Self { by_id: Vec::new() }
    }
}

impl<T> Held<T> {
    /// Longer than an ask of `claude agents` takes, short enough that a
    /// plain session's hooks do not pile up.
    pub const KEEP: Duration = Duration::from_secs(30);
    /// The newest this many for a conversation; older ones are moot once
    /// a newer one says where it stands.
    pub const EACH: usize = 4;
    /// Conversations held at once. Every plain `claude` posts here, and a
    /// flood of new ids must not grow this without end.
    pub const IDS: usize = 32;

    /// Keeps `item` for conversation `id`, dropping the oldest when full.
    pub fn hold(&mut self, id: &str, item: T, now: Instant) {
        self.age(now);
        let at = match self.by_id.iter().position(|(i, _)| i == id) {
            Some(at) => at,
            None => {
                if self.by_id.len() >= Self::IDS {
                    self.by_id.remove(0);
                }
                self.by_id.push((id.to_string(), Vec::new()));
                self.by_id.len() - 1
            }
        };
        let items = &mut self.by_id[at].1;
        if items.len() >= Self::EACH {
            items.remove(0);
        }
        items.push((now, item));
    }

    /// What is held for `id`, oldest first, and forgotten here.
    pub fn take(&mut self, id: &str, now: Instant) -> Vec<T> {
        self.age(now);
        match self.by_id.iter().position(|(i, _)| i == id) {
            Some(at) => self
                .by_id
                .remove(at)
                .1
                .into_iter()
                .map(|(_, t)| t)
                .collect(),
            None => Vec::new(),
        }
    }

    fn age(&mut self, now: Instant) {
        for (_, items) in &mut self.by_id {
            items.retain(|(t, _)| now.saturating_duration_since(*t) < Self::KEEP);
        }
        self.by_id.retain(|(_, items)| !items.is_empty());
    }
}

/// The background sessions in what `claude agents --json` printed. It also
/// lists interactive sessions, which are someone's terminal and not ours.
pub fn parse(json: &[u8]) -> Vec<Background> {
    serde_json::from_slice::<Vec<Background>>(json)
        .unwrap_or_default()
        .into_iter()
        .filter(|b| b.kind == "background" && !b.id.is_empty() && !b.session_id.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &[u8] = br#"[
        {"pid": 1, "id": "8e613b8c", "cwd": "C:\\dev\\app", "kind": "background",
         "startedAt": 1, "sessionId": "8e613b8c-7dcf", "name": "fix login",
         "status": "busy", "state": "working"},
        {"pid": 2, "id": "aa", "cwd": "C:\\dev\\app", "kind": "interactive",
         "sessionId": "aa-1", "state": "done"},
        {"pid": 3, "id": "93078e03", "cwd": "C:\\dev\\other", "kind": "background",
         "sessionId": "93078e03-2e39", "name": " ", "state": "done", "future": true}
    ]"#;

    #[test]
    fn only_background_sessions_are_kept() {
        let list = parse(LIST);
        let ids: Vec<&str> = list.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["8e613b8c", "93078e03"]);
        assert_eq!(list[0].session_id, "8e613b8c-7dcf");
        assert_eq!(list[0].cwd, "C:\\dev\\app");
    }

    #[test]
    fn anything_but_a_list_is_nothing() {
        assert!(parse(b"").is_empty());
        assert!(parse(b"error: not logged in").is_empty());
        assert!(parse(br#"{"id": "x"}"#).is_empty());
    }

    #[test]
    fn the_tile_carries_the_ids_and_the_state() {
        let list = parse(LIST);
        let s = list[0].session();
        assert_eq!(s.id, "bg-8e613b8c");
        assert!(is_tile(&s.id));
        assert!(!is_tile("fix-login"));
        assert!(!is_tile("bg-"));
        assert_eq!(s.name, "fix login");
        assert_eq!(s.background.as_deref(), Some("8e613b8c"));
        assert_eq!(s.claude_session_id.as_deref(), Some("8e613b8c-7dcf"));
        assert_eq!(s.phase, Phase::Working);
        // A blank name falls back to the short id.
        assert_eq!(list[1].session().name, "93078e03");
        assert_eq!(list[1].session().phase, Phase::Done);
    }

    #[test]
    fn a_conversation_is_asked_about_once_a_minute() {
        let mut asked = Asked::default();
        let t0 = Instant::now();
        assert!(asked.due("c1", t0));
        assert!(!asked.due("c1", t0 + Duration::from_secs(59)));
        assert!(asked.due("c2", t0 + Duration::from_secs(49)));
        assert!(asked.due("c1", t0 + Asked::AGAIN));
        assert_eq!(asked.at.len(), 2);
    }

    #[test]
    fn new_conversations_flooding_in_are_asked_about_once_a_gap() {
        let mut asked = Asked::default();
        let t0 = Instant::now();
        assert!(asked.due("c0", t0));
        for i in 1..1000 {
            assert!(!asked.due(&format!("c{i}"), t0 + Duration::from_millis(i)));
        }
        // Turned away, not marked: it is asked about once the gap is over.
        assert!(asked.due("c5", t0 + Asked::GAP));
        assert_eq!(asked.at.len(), 2);
    }

    #[test]
    fn held_hooks_come_back_oldest_first_and_once() {
        let mut held = Held::default();
        let t0 = Instant::now();
        held.hold("c1", 1, t0);
        held.hold("c2", 9, t0);
        held.hold("c1", 2, t0 + Duration::from_secs(1));
        assert_eq!(held.take("c1", t0 + Duration::from_secs(2)), vec![1, 2]);
        assert!(held.take("c1", t0 + Duration::from_secs(2)).is_empty());
        assert_eq!(held.take("c2", t0 + Duration::from_secs(2)), vec![9]);
    }

    #[test]
    fn held_hooks_age_out_and_stay_few() {
        let mut held = Held::default();
        let t0 = Instant::now();
        for i in 0..10 {
            held.hold("c1", i, t0);
        }
        assert_eq!(held.take("c1", t0), vec![6, 7, 8, 9]);
        held.hold("c1", 1, t0);
        assert!(held.take("c1", t0 + Held::<i32>::KEEP).is_empty());
        for i in 0..100 {
            held.hold(&format!("x{i}"), i, t0);
        }
        assert_eq!(held.by_id.len(), Held::<i32>::IDS);
        assert!(held.take("x0", t0).is_empty());
        assert_eq!(held.take("x99", t0), vec![99]);
    }

    #[test]
    fn unknown_states_are_idle() {
        let mut b = parse(LIST).remove(0);
        b.state = Some("pondering".into());
        assert_eq!(b.phase(), Phase::Idle);
        b.state = None;
        assert_eq!(b.phase(), Phase::Idle);
    }
}
