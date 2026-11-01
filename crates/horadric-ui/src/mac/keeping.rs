//! Keepers and takeover on the Mac, the part a brother needs of it so far
//! (`horadric-ui/src/keeping.rs` is Windows' whole of it; the bookkeeping
//! is `horadric_core::keeping`, the stores `horadric_core::keeper`):
//!
//! - As a keeper it files the snapshots a brother that names the Mac its
//!   keeper sends (`keep`), so they are there to be taken over from.
//! - Whoever owns a session tells every brother with `takeover`, and the
//!   Mac tells its own owners whenever a brother links. A session of the
//!   Mac another took over meanwhile (a brother's `takeover`, or a roster)
//!   is fenced: its agent stops, its work is kept on
//!   `horadric/orphaned/<session>`, and its tile says where it runs now.
//!
//! Not yet: the Mac naming a keeper of its own and sending it snapshots,
//! and taking a silent brother's session over here.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use horadric_core::brotherhood::{orphan_branch, Owner};
use horadric_core::keeper::{self, Joins};
use horadric_core::keeping::{self, takeover_message, words_of, Book};
use horadric_core::serving::Job;
use serde_json::Value;

use super::app::{self, App};
use super::{brothers, dialog, far, handing};
use crate::project::brother_name;

/// One snapshot at a time goes into a store, so two that came close
/// together never write one repository at once.
static RECEIVING: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct State {
    loaded: bool,
    book: Book,
    joins: Joins,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.loaded {
            s.loaded = true;
            if let Some(dir) = horadric_hooks::state_dir() {
                s.book = Book::load(&dir);
            }
        }
        f(&mut s)
    })
}

fn save(book: &Book) {
    if let Some(dir) = horadric_hooks::state_dir() {
        if let Err(e) = book.save(&dir) {
            eprintln!("horadric: cannot keep the keeping book: {e}");
        }
    }
}

/// The line of a session of the Mac's taken over elsewhere, which never
/// runs here again.
pub(super) fn fenced(id: &str) -> Option<String> {
    with(|s| s.book.fenced(id).map(str::to_string))
}

/// Brother `id` linked: it hears who owns what of the Mac's sessions.
pub(super) fn linked(app: &mut App, id: &str) {
    let mine = handing::sworn().map(|w| w.mine()).unwrap_or_default();
    far::tell(app, id, takeover_message(&mine));
}

/// A brother's `keep` part or `takeover` from `who`, its relay id: a job
/// filing a whole snapshot, or None.
pub(super) fn serve(app: &mut App, who: &str, msg: &Value) -> Option<Job> {
    if msg["t"] == "takeover" {
        let heard = handing::sworn()
            .map(|w| w.told(&words_of(msg)))
            .unwrap_or_default();
        for (id, o, ran, n) in &heard.twice {
            dialog::notify(
                "A session ran twice",
                &format!(
                    "{id} is {}'s, but it also ran on {} (from its number {n}), which stops its copy once it hears.",
                    brother_name(&o.brother),
                    brother_name(ran)
                ),
            );
        }
        moved(app, heard.moved);
        return None;
    }
    let whole = match with(|s| s.joins.add(who, msg, app::unix_now())) {
        Ok(w) => w?,
        Err(e) => {
            eprintln!("horadric: {e}");
            return None;
        }
    };
    if let Some(keepers) = whole.2.as_ref().map(|h| h.keepers.clone()) {
        if !keepers.is_empty() {
            keepers_said(who, keepers);
        }
    }
    let root = keeping::root(&horadric_hooks::state_dir()?);
    let from = who.to_string();
    let from_name = brother_name(who);
    let caps = with(|s| s.book.caps());
    // A keeper's cap spares the sessions of a brother not linked now,
    // which may yet be taken over.
    let awake: HashSet<String> = far::takers(app).into_iter().collect();
    Some(Box::new(move || {
        keeper::file(&root, &from, &from_name, whole, caps, &awake, &RECEIVING)
            .unwrap_or(Value::Null)
    }))
}

/// Brother `from` names `keepers`: when the Mac is no longer its first,
/// what the Mac keeps of it goes.
fn keepers_said(from: &str, keepers: Vec<String>) {
    let Some(me) = brothers::me_id() else {
        return;
    };
    let drop = with(|s| {
        if s.book.keepers_of.get(from) == Some(&keepers) {
            return false;
        }
        let drop = s.book.keepers_said(from, keepers, &me);
        save(&s.book);
        drop
    });
    if let (true, Some(dir)) = (drop, horadric_hooks::state_dir()) {
        keeper::let_go(&keeping::root(&dir), from);
    }
}

/// Sessions of the Mac another owns now, as a roster or a brother's
/// `takeover` said: each is fenced, its agent stopped and its work kept on
/// a branch. One being handed to that very brother is the handoff's own
/// doing, which its `took` finishes.
pub(super) fn moved(app: &mut App, moved: Vec<(String, Owner)>) {
    let moved: Vec<(String, Owner)> = moved
        .into_iter()
        .filter(|(id, o)| handing::going_to(id).as_deref() != Some(o.brother.as_str()))
        .collect();
    if moved.is_empty() {
        return;
    }
    let (Some(dir), Some(me)) = (horadric_hooks::state_dir(), brothers::me_id()) else {
        return;
    };
    let names = horadric_core::rostering::known(&dir, &me, &brothers::mac_name());
    let offset = far::utc_offset(app::unix_now());
    let fenced = with(|s| {
        let fenced = s.book.fence(&moved, &names, offset);
        save(&s.book);
        fenced
    });
    for (id, line) in fenced {
        orphan(app, &id, &line);
    }
}

/// Stops a session taken over elsewhere and keeps its work on its orphan
/// branch, so nothing done here since is lost.
fn orphan(app: &mut App, id: &str, line: &str) {
    if let Some(c) = app.consoles.get(id) {
        if c.exit_code().is_none() {
            c.kill();
        }
    }
    let cwd = app.registry.lock().ok().and_then(|mut r| {
        let s = r.get_mut(id)?;
        s.last_line = line.to_string();
        Some(s.cwd.clone())
    });
    if let Some(p) = app.paused.get_mut(id) {
        p.last_line = line.to_string();
    }
    app.reconcile();
    let Some(cwd) = cwd else {
        return;
    };
    let id = id.to_string();
    std::thread::spawn(move || match keeping::keep_orphan(Path::new(&cwd), &id) {
        Ok(c) => eprintln!("horadric: {} keeps {id}'s work at {c}", orphan_branch(&id)),
        Err(e) => eprintln!("horadric: cannot keep {id}'s orphaned work: {e}"),
    });
}
