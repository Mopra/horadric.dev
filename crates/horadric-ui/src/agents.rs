//! Claude Code's background sessions, asked of `claude agents`. Its daemon
//! runs them, so Horadric only lists them, attaches to them and stops them.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Instant;

use horadric_core::background::{self, Asked, Background, Held};
use horadric_core::{HookEvent, Registry};
use horadric_hooks::listener::Tagged;

use crate::console;

/// Every background session running now. None when `claude` is not there
/// or did not answer, which says nothing about whether any run.
pub fn list() -> Option<Vec<Background>> {
    let out = console::claude_command(&["agents", "--json"])?
        .output()
        .ok()?;
    out.status.success().then(|| background::parse(&out.stdout))
}

/// Stops a background session. Not waited for: its `SessionEnd`, or the
/// tile going, says it happened.
pub fn stop(short: &str) {
    if let Some(mut c) = console::claude_command(&["stop", short]) {
        let _ = c.stdout(std::process::Stdio::null()).spawn();
    }
}

/// A tile for each background session no tile holds yet. True when one
/// was added.
pub fn adopt_all(registry: &Arc<Mutex<Registry>>, list: &[Background]) -> bool {
    let Ok(mut r) = registry.lock() else {
        return false;
    };
    let mut added = false;
    for b in list {
        let held = r
            .all()
            .any(|s| s.claude_session_id.as_deref() == Some(&b.session_id));
        if !held && r.get(&background::tile_id(&b.id)).is_none() {
            r.add(b.session());
            added = true;
        }
    }
    added
}

/// What `claude agents` said last, whether it is being asked now, and
/// the hooks that came before it named their conversation.
#[derive(Default)]
struct Seen {
    list: Vec<Background>,
    asking: bool,
    held: Held<HookEvent>,
}

/// Where a held hook goes back in once its conversation has a tile: the
/// feeder's own channel, so it lands as if it had come late.
fn replay() -> &'static OnceLock<Sender<Tagged>> {
    static REPLAY: OnceLock<Sender<Tagged>> = OnceLock::new();
    &REPLAY
}

/// Has the hooks [`adopt`] could not place yet sent to `tx` once a later
/// ask of `claude agents` names their conversation. Without it they are
/// dropped, and a new background session's permission prompt or turn's
/// end shows only at its next hook.
pub fn replay_into(tx: Sender<Tagged>) {
    let _ = replay().set(tx);
}

fn seen() -> &'static Mutex<Seen> {
    static SEEN: OnceLock<Mutex<Seen>> = OnceLock::new();
    SEEN.get_or_init(Mutex::default)
}

/// The tile for the conversation of an event no tile holds, when it is a
/// background session. Answered from what `claude agents` said last; a
/// conversation it did not name has it asked again, off this thread, at
/// most once a minute per conversation and once per [`Asked::GAP`] in all,
/// since every plain `claude` on the machine posts here and so can any
/// web page. The answer gives the tile its own, and the hooks held till
/// then are sent again (see [`replay_into`]).
pub fn adopt(
    registry: &Arc<Mutex<Registry>>,
    event: &HookEvent,
    asked: &mut Asked,
) -> Option<String> {
    let id = event.session_id.as_str();
    let news = [HookEvent::STATUS, HookEvent::SPENT].contains(&event.hook_event_name.as_str());
    if id.is_empty() || news {
        return None;
    }
    let known = {
        let mut s = seen().lock().ok()?;
        let known = s.list.iter().find(|b| b.session_id == id).cloned();
        if known.is_none() && replay().get().is_some() {
            s.held.hold(id, event.clone(), Instant::now());
        }
        known
    };
    let Some(b) = known else {
        if asked.due(id, Instant::now()) {
            refresh(registry);
        }
        return None;
    };
    let tile = background::tile_id(&b.id);
    let mut r = registry.lock().ok()?;
    if r.get(&tile).is_none() {
        r.add(b.session());
    }
    Some(tile)
}

/// Asks `claude agents` on a thread of its own, one at a time, gives each
/// background session it names a tile and sends their held hooks again.
fn refresh(registry: &Arc<Mutex<Registry>>) {
    {
        let Ok(mut s) = seen().lock() else {
            return;
        };
        if s.asking {
            return;
        }
        s.asking = true;
    }
    let registry = Arc::clone(registry);
    thread::spawn(move || {
        let got = list();
        if let Some(list) = &got {
            adopt_all(&registry, list);
        }
        let mut again = Vec::new();
        if let Ok(mut s) = seen().lock() {
            if let Some(list) = got {
                let now = Instant::now();
                for b in &list {
                    again.extend(s.held.take(&b.session_id, now));
                }
                s.list = list;
            }
            s.asking = false;
        }
        // Untagged: the tile just made holds the conversation's id, so the
        // feeder routes them to it.
        if let Some(tx) = replay().get() {
            for event in again {
                let _ = tx.send(Tagged {
                    horadric_id: String::new(),
                    event,
                    limits: None,
                });
            }
        }
    });
}

/// Whether the background session with this short id still runs. None
/// when `claude` could not say.
pub fn running(short: &str) -> Option<bool> {
    Some(list()?.iter().any(|b| b.id == short))
}
