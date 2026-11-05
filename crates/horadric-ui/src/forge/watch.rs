//! Watches the folders file tabs are served from, so a save shows within
//! a quarter second: one thread per folder sleeping in
//! `ReadDirectoryChangesW`, a short wait for the burst a save makes, then
//! the UI thread woken with what changed.
//!
//! The files tile's watcher waits 300 ms and runs git; this one only says
//! which files changed, so it can afford to answer at once.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::System::Threading::SetEvent;

use crate::watch::{next, DirChanges, Event, Next};

/// How long a folder must be quiet after a change before the page loads
/// it: an editor's save is a truncate and a write or two, a few ms apart.
const SETTLE: Duration = Duration::from_millis(30);
/// The longest a change waits for its burst to end.
const MAX_SETTLE: Duration = Duration::from_millis(120);

/// Folders with changes for the UI thread: each folder, its changed paths
/// relative to it, and whether anything may have changed (the watcher
/// lost count).
type Changes = Vec<(PathBuf, Vec<String>, bool)>;
static CHANGED: Mutex<Changes> = Mutex::new(Vec::new());

/// A folder's thread. Dropping it stops the thread.
struct Follow {
    stop: Arc<Event>,
}

impl Drop for Follow {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.stop.0);
        }
    }
}

thread_local! {
    static FOLLOWS: RefCell<HashMap<String, Follow>> = RefCell::new(HashMap::new());
}

pub fn key_of(root: &Path) -> String {
    root.to_string_lossy().replace('/', "\\").to_lowercase()
}

/// Watches `root` unless it is already watched.
pub fn follow(root: &Path) {
    let key = key_of(root);
    if FOLLOWS.with(|f| f.borrow().contains_key(&key)) {
        return;
    }
    let Some(stop) = Event::new().map(Arc::new) else {
        return;
    };
    let theirs = Arc::clone(&stop);
    let dir = root.to_path_buf();
    let started = thread::Builder::new()
        .name("horadric-forge".into())
        .spawn(move || run(&dir, &theirs));
    if started.is_ok() {
        FOLLOWS.with(|f| f.borrow_mut().insert(key, Follow { stop }));
    }
}

/// Stops watching folders no file tab is served from any more.
pub fn keep_only(roots: &std::collections::HashSet<String>) {
    FOLLOWS.with(|f| f.borrow_mut().retain(|k, _| roots.contains(k)));
}

/// What the threads found since last asked.
pub fn take() -> Changes {
    CHANGED
        .lock()
        .map(|mut c| std::mem::take(&mut *c))
        .unwrap_or_default()
}

fn run(dir: &Path, stop: &Event) {
    let Some(changes) = DirChanges::open(dir, true, "") else {
        return;
    };
    let mut watches = [changes];
    loop {
        let mut paths: Vec<String> = Vec::new();
        let mut all = false;
        match next(&mut watches, stop, None) {
            Next::Stop | Next::Failed => return,
            Next::Quiet => continue,
            Next::Paths(p) => paths.extend(p.into_iter().filter(|p| matters(p))),
            Next::Overflow => all = true,
        }
        if paths.is_empty() && !all {
            continue;
        }
        let settling = Instant::now();
        while settling.elapsed() < MAX_SETTLE {
            match next(&mut watches, stop, Some(SETTLE)) {
                Next::Stop | Next::Failed => return,
                Next::Quiet => break,
                Next::Paths(p) => paths.extend(p.into_iter().filter(|p| matters(p))),
                Next::Overflow => all = true,
            }
        }
        paths.sort();
        paths.dedup();
        if let Ok(mut c) = CHANGED.lock() {
            c.push((dir.to_path_buf(), paths, all));
        }
        super::wake();
    }
}

/// Whether a changed path may be a file a tab shows or a page loads. A
/// build's output and git's own files never are, and churn the most.
fn matters(rel: &str) -> bool {
    let rel = rel.replace('\\', "/");
    let first = rel.split('/').next().unwrap_or_default();
    if matches!(first, ".git" | "target" | "node_modules") {
        return false;
    }
    let path = Path::new(&rel);
    horadric_core::forge::kind_of_bytes(path, &[]).is_some() || super::serve::is_subresource(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_and_git_are_not_watched_for() {
        assert!(matters("docs/deck.html"));
        assert!(matters("docs\\style.css"));
        assert!(!matters("target/debug/build.log"));
        assert!(!matters(".git/index"));
        assert!(!matters("src/main.rs"));
    }
}
