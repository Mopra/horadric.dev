//! All known sessions, keyed by Glance id.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::event::HookEvent;
use crate::session::{Phase, Session};

/// The set of sessions Glance knows about. Not thread safe on its own; the
/// owner wraps it in a mutex.
#[derive(Debug, Default)]
pub struct Registry {
    sessions: BTreeMap<String, Session>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a session Glance spawned. Called before the CLI starts so the
    /// first hook finds a home.
    pub fn add(&mut self, session: Session) {
        self.sessions.insert(session.id.clone(), session);
    }

    pub fn get(&self, id: &str) -> Option<&Session> {
        self.sessions.get(id)
    }

    pub fn remove(&mut self, id: &str) -> Option<Session> {
        self.sessions.remove(id)
    }

    /// Applies an event to the session with the given Glance id.
    ///
    /// Unknown ids are adopted: a `claude` started by hand inside a Glance
    /// environment is still a session worth showing. The id doubles as the
    /// name, since the project folder is already the cluster's title.
    /// Returns true when the session's phase changed.
    pub fn apply(&mut self, glance_id: &str, event: &HookEvent, now: SystemTime) -> bool {
        let session = self
            .sessions
            .entry(glance_id.to_string())
            .or_insert_with(|| Session::new(glance_id, glance_id, event.cwd.clone()));
        session.apply(event, now)
    }

    /// Every session, in a stable order.
    pub fn all(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values()
    }

    /// The inbox: sessions blocked on the human, oldest wait first.
    pub fn waiting(&self) -> Vec<&Session> {
        let mut v: Vec<&Session> = self
            .sessions
            .values()
            .filter(|s| s.phase.is_waiting())
            .collect();
        v.sort_by_key(|s| s.since);
        v
    }

    /// Forgets sessions that ended more than `linger` ago. Returns how many.
    pub fn prune_ended(&mut self, linger: std::time::Duration, now: SystemTime) -> usize {
        let before = self.sessions.len();
        self.sessions.retain(|_, s| {
            s.phase != Phase::Ended || now.duration_since(s.since).unwrap_or_default() < linger
        });
        before - self.sessions.len()
    }

    pub fn count_in(&self, phase: &Phase) -> usize {
        self.sessions.values().filter(|s| &s.phase == phase).count()
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adopts_unknown_session_named_by_id() {
        let mut r = Registry::new();
        let e = HookEvent::from_json(
            br#"{"session_id":"c","hook_event_name":"UserPromptSubmit","cwd":"C:\\dev\\glance"}"#,
        )
        .unwrap();
        assert!(r.apply("g9", &e, SystemTime::now()));
        assert_eq!(r.get("g9").unwrap().name, "g9");
        assert_eq!(r.get("g9").unwrap().phase, Phase::Working);
    }

    #[test]
    fn inbox_is_oldest_first() {
        let mut r = Registry::new();
        let t0 = SystemTime::UNIX_EPOCH;
        let t1 = t0 + std::time::Duration::from_secs(10);
        let perm = HookEvent::from_json(
            br#"{"session_id":"c","hook_event_name":"PermissionRequest","tool_name":"Bash"}"#,
        )
        .unwrap();
        r.apply("newer", &perm, t1);
        r.apply("older", &perm, t0);
        let ids: Vec<&str> = r.waiting().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["older", "newer"]);
    }
}
