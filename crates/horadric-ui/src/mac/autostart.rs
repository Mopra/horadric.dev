//! Opening at login, through a LaunchAgent in `~/Library/LaunchAgents`.
//! The plist is written, not loaded: launchd reads it at the next login,
//! and loading it now would start a second Horadric beside this one.

use std::fs;
use std::path::{Path, PathBuf};

use crate::plist;

fn agent_path() -> Option<PathBuf> {
    horadric_hooks::home().map(|h| {
        PathBuf::from(h)
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", plist::BUNDLE_ID))
    })
}

/// Opens `exe` at login from now on.
pub fn enable_at(exe: &Path) -> Result<(), String> {
    if horadric_hooks::dev() {
        return Ok(());
    }
    let path = agent_path().ok_or("cannot find your home folder")?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(&path, plist::launch_agent(&exe.to_string_lossy()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Stops opening at login.
pub fn disable() {
    if horadric_hooks::dev() {
        return;
    }
    if let Some(path) = agent_path() {
        let _ = fs::remove_file(path);
    }
}

/// Whether Horadric opens at login.
pub fn enabled() -> bool {
    agent_path().is_some_and(|p| p.is_file())
}
