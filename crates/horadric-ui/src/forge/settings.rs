//! What the Forge keeps over a restart, in `forge.json` beside the app's
//! state: the projects whose sessions' files open by themselves,
//! and which folder each file tab's host serves, so a tab saved with the
//! browser's pages is a file's tab again after a restart.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
struct Kept {
    /// Projects with the switch on. Off is the default, since a session
    /// writing code would otherwise open a tab for every file it touches.
    /// The old `off` list is ignored on read.
    #[serde(default)]
    on: BTreeSet<String>,
    /// Each host a file tab was served at, and its folder.
    #[serde(default)]
    roots: BTreeMap<String, PathBuf>,
}

thread_local! {
    static KEPT: RefCell<Option<Kept>> = const { RefCell::new(None) };
}

fn file() -> Option<PathBuf> {
    horadric_hooks::state_dir().map(|d| d.join("forge.json"))
}

fn with<T>(f: impl FnOnce(&mut Kept) -> (T, bool)) -> T {
    KEPT.with(|k| {
        let mut k = k.borrow_mut();
        let kept = k.get_or_insert_with(|| {
            file()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        });
        let (out, changed) = f(kept);
        if changed {
            if let (Some(p), Ok(s)) = (file(), serde_json::to_string_pretty(kept)) {
                if let Err(e) = std::fs::write(&p, s) {
                    eprintln!("horadric: cannot save {}: {e}", p.display());
                }
            }
        }
        out
    })
}

/// Whether a file the project `key`'s sessions write opens in the Forge
/// by itself. Off unless turned on.
pub fn auto(key: &str) -> bool {
    with(|k| (k.on.contains(key), false))
}

/// Turns the project's switch on or off.
pub fn set_auto(key: &str, on: bool) {
    with(|k| {
        let changed = if on {
            k.on.insert(key.to_string())
        } else {
            k.on.remove(key)
        };
        ((), changed)
    });
}

/// Keeps which folder `host` serves.
pub fn remember_root(host: &str, root: &Path) {
    with(|k| {
        let host = host.to_ascii_lowercase();
        let changed = k.roots.get(&host).map(PathBuf::as_path) != Some(root);
        if changed {
            k.roots.insert(host, root.to_path_buf());
        }
        ((), changed)
    });
}

/// The folder `host` serves, if one was kept.
pub fn root_of(host: &str) -> Option<PathBuf> {
    with(|k| (k.roots.get(&host.to_ascii_lowercase()).cloned(), false))
}
