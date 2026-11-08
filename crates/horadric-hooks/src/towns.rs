//! Where the towns' own Claude homes are, found on disk so every process
//! agrees without reading the app's state: the installer writing hooks,
//! the transcript lookups and the app starting a town's sessions. See
//! `horadric_core::towns`.

use std::path::{Path, PathBuf};

use horadric_core::towns;

/// The folder this instance keeps its towns under: `~\Horadric`, or
/// `~\Horadric-dev` for a dev instance.
pub fn base() -> Option<PathBuf> {
    Some(Path::new(&crate::home()?).join(crate::state_name()))
}

/// The folders of `base` a town's Claude home can be in.
fn possible(base: &Path) -> Vec<PathBuf> {
    let entries: Vec<String> = std::fs::read_dir(base.join("towns"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    towns::possible_homes(base, &entries)
}

/// Every town home of this instance that is there.
pub fn homes() -> Vec<PathBuf> {
    base()
        .map(|b| possible(&b).into_iter().filter(|h| h.is_dir()).collect())
        .unwrap_or_default()
}

/// The Claude home a session in `cwd` runs on when it is a town's own,
/// None for the machine's.
pub fn home_for(cwd: &str) -> Option<PathBuf> {
    if cwd.is_empty() {
        return None;
    }
    towns::home_for(cwd, &homes()).cloned()
}
