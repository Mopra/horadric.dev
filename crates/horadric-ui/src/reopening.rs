//! Reopening a session the human closed: Ctrl+Shift+R on the stage, or
//! the project menu. The list itself is `horadric_core::reopen`; this is
//! the glue to tiles and the stage.
//!
//! A closed session comes back as a paused tile with its old id, name and
//! title, and is resumed at once as any paused tile is, so it is a Folio
//! when it was one or when new sessions open as chats.

use std::cell::RefCell;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use horadric_core::reopen::{self, Closed};
use horadric_core::saved::SavedSession;

use super::{project_key, unix_now, App, Kind};

/// Key repeat or a double press must not reopen a run of sessions, each a
/// `claude` of its own.
const GUARD: Duration = Duration::from_millis(800);

#[derive(Default)]
struct State {
    closed: Vec<Closed>,
    last: Option<Instant>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// The list as saved, read once at start.
pub(super) fn load(closed: &[Closed]) {
    with(|s| s.closed = closed.to_vec());
}

/// The list to save.
pub(super) fn saved() -> Vec<Closed> {
    with(|s| s.closed.clone())
}

/// The project's closed sessions for its menu, newest first: the
/// conversation each carries on and its line.
pub(super) fn listed(project: &str, n: usize) -> Vec<(String, String)> {
    with(|s| {
        reopen::of(&s.closed, project)
            .take(n)
            .filter_map(|c| {
                let label = c.session.to_session(SystemTime::now()).label().to_string();
                Some((c.conversation()?.to_string(), label))
            })
            .collect()
    })
}

impl App {
    /// Keeps session `id` for reopening, as the human closes it. Called
    /// before it is forgotten, while its tile still says what it was.
    pub(super) fn remember_closed(&mut self, id: &str) {
        let Some(closed) = self.closed_entry(id) else {
            return;
        };
        with(|s| reopen::remember(&mut s.closed, closed));
    }

    fn closed_entry(&self, id: &str) -> Option<Closed> {
        let r = self.shared.registry.lock().ok()?;
        let s = r.get(id)?;
        // Not this PC's to start: a brother's session, or the daemon's.
        if s.brother.is_some() || s.background.is_some() {
            return None;
        }
        let (args, chat) = if let Some(p) = self.paused.get(id) {
            (p.args.clone(), p.chat)
        } else if let Some(c) = self.consoles.get(id) {
            (c.args.clone(), false)
        } else if self.folios.contains_key(id) {
            (self.folio_args.get(id).cloned().unwrap_or_default(), true)
        } else {
            return None;
        };
        Some(Closed {
            session: SavedSession {
                chat,
                ..SavedSession::from_session(s, args, false)
            },
            project: project_key(s),
            at: unix_now(),
        })
    }

    /// Brings back the newest closed session, of the project with key
    /// `project` when one is named, or the one carrying on `conversation`,
    /// on the stage with the keyboard.
    pub(super) fn reopen_closed(&mut self, project: Option<&str>, conversation: Option<&str>) {
        let now = Instant::now();
        if with(|s| s.last.is_some_and(|l| now.duration_since(l) < GUARD)) {
            return;
        }
        let Some(closed) = with(|s| reopen::take(&mut s.closed, project, conversation)) else {
            self.toasts.show(
                Kind::Info,
                "Nothing to reopen",
                "No closed session is kept.",
            );
            return;
        };
        with(|s| s.last = Some(now));
        let mut saved = closed.session;
        // Already back, from the history or a second way in: shown, never
        // started twice.
        let on_tile = self.shared.registry.lock().ok().and_then(|r| {
            r.all()
                .find(|s| s.claude_session_id == saved.claude_session_id)
                .map(|s| s.id.clone())
        });
        if let Some(id) = on_tile {
            self.reveal(&id, false);
            return;
        }
        // Its own worktree goes when it closes, so it comes back in the
        // checkout the worktree was made from.
        if !Path::new(&saved.cwd).is_dir() {
            if let Some(w) = saved.worktree.take() {
                saved.cwd = w.main;
            }
        }
        let taken = self.paused.contains_key(&saved.id)
            || self.consoles.contains_key(&saved.id)
            || self.folios.contains_key(&saved.id)
            || (self.shared.registry.lock()).is_ok_and(|r| r.get(&saved.id).is_some());
        if taken {
            saved.id = self.unique_id(&saved.name);
        }
        let id = saved.id.clone();
        if let Ok(mut r) = self.shared.registry.lock() {
            r.add(saved.to_session(SystemTime::now()));
        }
        self.paused.insert(id.clone(), saved);
        self.reconcile(false);
        // A paused tile asked for resumes, shown and focused.
        self.reveal(&id, false);
        self.save();
    }
}
