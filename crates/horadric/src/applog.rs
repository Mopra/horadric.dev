//! Points the app's stderr at `horadric.log` in the state folder. Every way
//! the app is started (the launcher, autostart, `reload`) hands it a null
//! stderr, so without this the UI's messages went nowhere. Done in process,
//! not at each spawn, so no launch path can miss it.

use std::fs::{self, File, OpenOptions};
use std::io::{IsTerminal, Write};

use horadric_ui::crashlog;

/// Past this the log is rotated at the next start, keeping one old file.
const CAP: u64 = 4 * 1024 * 1024;

/// Installs the panic hook and, unless stderr is a terminal someone is
/// watching (`horadric app` run by hand, a dev instance), sends stderr to
/// the log.
pub fn start() {
    crashlog::install_panic_hook();
    if std::io::stderr().is_terminal() {
        return;
    }
    let Some(path) = crashlog::log_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if fs::metadata(&path).is_ok_and(|m| m.len() > CAP) {
        let _ = fs::rename(&path, path.with_extension("log.1"));
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(
        file,
        "\n{} horadric {} started, pid {}",
        crashlog::stamp(),
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
    redirect(file);
}

/// Rust's stderr asks Windows for the handle on every write, so swapping
/// the process's handle is enough. The handle is kept for the process's life.
#[cfg(windows)]
fn redirect(file: File) {
    use std::os::windows::io::IntoRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
    // SAFETY: the handle is a file this process owns and never closes.
    unsafe {
        let _ = SetStdHandle(STD_ERROR_HANDLE, HANDLE(file.into_raw_handle()));
    }
}

#[cfg(unix)]
fn redirect(file: File) {
    use std::os::fd::AsRawFd;
    // SAFETY: dup2 onto fd 2 with a valid fd; the copy outlives `file`.
    unsafe {
        libc::dup2(file.as_raw_fd(), 2);
    }
}
