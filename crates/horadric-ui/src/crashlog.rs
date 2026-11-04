//! What is left when the app dies: its stderr goes to `horadric.log` in the
//! state folder (the binary points it there), a panic writes its message,
//! place and backtrace there too, and a `crashed` marker beside it lets the
//! next start say so. A panic in a wndproc aborts the process, so the hook
//! is the only moment anything about it can be written.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// The log's name in the state folder, `horadric.log.1` once it is rotated.
pub const LOG: &str = "horadric.log";
const MARKER: &str = "crashed";

/// `%APPDATA%\Horadric\horadric.log`, or under `Horadric-dev` for a dev
/// instance.
pub fn log_path() -> Option<PathBuf> {
    horadric_hooks::state_dir().map(|d| d.join(LOG))
}

fn marker_path() -> Option<PathBuf> {
    horadric_hooks::state_dir().map(|d| d.join(MARKER))
}

/// Now, as the log writes it.
pub fn stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    horadric_core::tasks::utc(secs)
}

/// Replaces the default hook, which prints no backtrace unless
/// `RUST_BACKTRACE` is set and leaves nothing for the next start to find.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed").to_string();
        let what = info.payload_as_str().unwrap_or("(no message)");
        let at = info
            .location()
            .map_or_else(|| "an unknown place".to_string(), |l| l.to_string());
        let summary = format!("{} panic on thread {name} at {at}: {what}", stamp());
        let trace = std::backtrace::Backtrace::force_capture();
        eprintln!("horadric: {summary}\n{trace}");
        // Only the main thread, which runs the UI, takes the app down. A
        // worker's panic leaves it running, and a logoff after that ends
        // it with no clean quit to clear a marker, so the next start would
        // call a working app a crash.
        if name != "main" {
            return;
        }
        if let Some(p) = marker_path() {
            let _ = std::fs::write(p, &summary);
        }
    }));
}

/// What the last run's panic said, once: the marker is gone after this.
pub fn take_last_crash() -> Option<String> {
    let p = marker_path()?;
    let said = std::fs::read_to_string(&p).ok()?;
    let _ = std::fs::remove_file(&p);
    Some(said.trim().to_string())
}

/// A quit that got this far was no crash, whatever marker a caught panic
/// on the main thread may have left behind.
pub fn clean_exit() {
    if let Some(p) = marker_path() {
        let _ = std::fs::remove_file(p);
    }
}
