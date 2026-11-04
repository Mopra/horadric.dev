//! Has a small model title conversations in the background and keeps the
//! titles: a session's own at the end of its turns, so its tile and pane
//! carry it, and the ones a history list shows that have none yet. A list
//! that is open when one lands is drawn again with it.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use horadric_core::retitle::{self, Titles};
use horadric_hooks::transcript::Past;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::InvalidateRect;

/// Asked at once, so a backlog clears in minutes, not an hour.
const WORKERS: usize = 3;

/// Titled per list opening at most: the newest, which are the ones looked
/// for. Older ones are titled as later lists reach them.
const PER_LIST: usize = 60;

/// A title is a few words; Haiku through `claude` starting takes most of it.
const LONGEST: Duration = Duration::from_secs(45);

struct Queue {
    waiting: VecDeque<(String, PathBuf, u64)>,
    /// Ids waiting or being titled, so a list opened twice asks once.
    busy: Vec<String>,
    workers: usize,
}

fn queue() -> &'static Mutex<Queue> {
    static Q: OnceLock<Mutex<Queue>> = OnceLock::new();
    Q.get_or_init(|| {
        Mutex::new(Queue {
            waiting: VecDeque::new(),
            busy: Vec::new(),
            workers: 0,
        })
    })
}

/// Counts titles made, so an open list knows to read them again.
static MADE: AtomicU64 = AtomicU64::new(0);

/// The window drawing the last list asked for, drawn again when a title
/// lands.
static VIEW: AtomicIsize = AtomicIsize::new(0);

pub fn made() -> u64 {
    MADE.load(Ordering::Relaxed)
}

/// Told of each title made, by conversation id, to put it on the tiles
/// holding that conversation.
type Landed = Box<dyn Fn(&str, &str) + Send + Sync>;
static LANDED: OnceLock<Landed> = OnceLock::new();

pub fn on_landed(f: impl Fn(&str, &str) + Send + Sync + 'static) {
    let _ = LANDED.set(Box::new(f));
}

/// A session's turn ended with its transcript at `path`: its conversation
/// is titled when it has no title of the model's yet or has doubled since.
/// A `/rename` is never replaced.
pub fn turn_ended(path: &str) {
    let path = std::path::Path::new(path);
    let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
        return;
    };
    let Ok(len) = std::fs::metadata(path).map(|m| m.len()) else {
        return;
    };
    if !horadric_hooks::transcript::model_titles().wants(id, len) {
        return;
    }
    if horadric_hooks::transcript::tail_title(path).is_some_and(|t| t.custom) {
        return;
    }
    let Ok(mut q) = queue().lock() else {
        return;
    };
    if !q.busy.iter().any(|b| b == id) {
        q.busy.push(id.to_string());
        // Ahead of a history list's backlog: it is the one on screen.
        q.waiting.push_front((id.to_string(), path.to_path_buf(), len));
    }
    start(&mut q);
}

fn start(q: &mut Queue) {
    while q.workers < WORKERS && !q.waiting.is_empty() {
        q.workers += 1;
        thread::spawn(work);
    }
}

/// Titles the conversations in `past` that want one, newest first, with
/// `view` drawn again as each lands. A `/rename` is never replaced.
pub fn want(past: &[Past], view: isize) {
    if view != 0 {
        VIEW.store(view, Ordering::Relaxed);
    }
    let titles = horadric_hooks::transcript::model_titles();
    let Ok(mut q) = queue().lock() else {
        return;
    };
    let wanted = past
        .iter()
        .filter(|p| !p.title.custom && titles.wants(&p.id, p.len))
        .take(PER_LIST);
    for p in wanted {
        if !q.busy.contains(&p.id) {
            q.busy.push(p.id.clone());
            q.waiting.push_back((p.id.clone(), p.path.clone(), p.len));
        }
    }
    start(&mut q);
}

fn work() {
    loop {
        let next = queue().lock().ok().and_then(|mut q| {
            let n = q.waiting.pop_front();
            if n.is_none() {
                q.workers -= 1;
            }
            n
        });
        let Some((id, path, len)) = next else {
            return;
        };
        if let Some(title) = title_of(&path) {
            keep(&id, title, len);
        }
        if let Ok(mut q) = queue().lock() {
            q.busy.retain(|b| b != &id);
        }
    }
}

fn title_of(path: &std::path::Path) -> Option<String> {
    let prompt = horadric_hooks::transcript::retitle_input(path)?;
    let reply = crate::haiku::ask(retitle::MODEL, retitle::SYSTEM, &prompt, LONGEST)?;
    retitle::titled(&reply)
}

/// Writes the title in, read again under the lock so workers finishing
/// together keep each other's.
fn keep(id: &str, title: String, len: u64) {
    static WRITING: Mutex<()> = Mutex::new(());
    let Some(state) = horadric_hooks::state_dir() else {
        return;
    };
    let Ok(_held) = WRITING.lock() else {
        return;
    };
    let mut titles = Titles::load(&state);
    titles.put(id, title, len);
    if titles.save(&state).is_err() {
        return;
    }
    if let Some(landed) = LANDED.get() {
        landed(id, &titles.made[id].title);
    }
    MADE.fetch_add(1, Ordering::Relaxed);
    let view = VIEW.load(Ordering::Relaxed);
    if view != 0 {
        unsafe {
            let _ = InvalidateRect(Some(HWND(view as *mut _)), None, false);
        }
    }
}
