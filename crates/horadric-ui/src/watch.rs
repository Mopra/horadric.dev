//! Keeps a project's file tree fresh for its files tile.
//!
//! One thread per project runs git, hands the tree to the cluster and then
//! sleeps in `ReadDirectoryChangesW` until a file changes. Nothing polls, so
//! a quiet project costs no CPU. Changes inside ignored folders are dropped,
//! or every `cargo build` would rerun git a hundred times; a burst of
//! changes is waited out and answered with one scan.
//!
//! `git --no-optional-locks` matters: a plain `git status` may rewrite the
//! index, which the watcher would see as a change, and scan again, forever.

use std::collections::HashSet;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
    FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL};

use crate::files::{self, Tree};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// How long the files must be quiet before git runs again.
const SETTLE: Duration = Duration::from_millis(300);
/// The least time between two scans, however busy the folder is.
const MIN_GAP: Duration = Duration::from_secs(1);
/// A folder that never goes quiet, a log written every second, still gets
/// scanned this often.
const MAX_SETTLE: Duration = Duration::from_secs(3);

/// The newest tree, left here by the thread for the cluster to take.
pub type Slot = Arc<Mutex<Option<Tree>>>;

/// The thread watching one project. Dropping it stops the thread.
pub struct Watcher {
    stop: Arc<Event>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.stop.0);
        }
    }
}

/// Starts watching `dir`. Every fresh tree goes into `slot`, then `msg` is
/// posted to `hwnd`. A folder outside git gets no tree and no thread stays.
pub fn start(dir: PathBuf, hwnd: HWND, msg: u32, slot: Slot) -> Option<Watcher> {
    let stop = Arc::new(Event::new()?);
    let theirs = Arc::clone(&stop);
    let hwnd = hwnd.0 as isize;
    thread::Builder::new()
        .name("horadric-files".into())
        .spawn(move || run(&dir, hwnd, msg, &slot, &theirs))
        .ok()?;
    Some(Watcher { stop })
}

fn run(dir: &Path, hwnd: isize, msg: u32, slot: &Slot, stop: &Event) {
    let Some(prefix) = git(dir, &["rev-parse", "--show-prefix"]) else {
        return;
    };
    let prefix = String::from_utf8_lossy(&prefix).trim_end().to_string();
    let mut changes = DirChanges::open(dir);
    loop {
        let started = Instant::now();
        let ignored = match scan(dir, &prefix) {
            Some((tree, ignored)) => {
                if let Ok(mut s) = slot.lock() {
                    *s = Some(tree);
                }
                unsafe {
                    let _ =
                        PostMessageW(Some(HWND(hwnd as *mut c_void)), msg, WPARAM(0), LPARAM(0));
                }
                ignored
            }
            None => HashSet::new(),
        };
        if std::env::var_os("HORADRIC_DEBUG").is_some() {
            eprintln!("files {} scanned in {:?}", dir.display(), started.elapsed());
        }
        let Some(ch) = changes.as_mut() else {
            // No way to hear about changes: the one scan is all there is.
            return;
        };

        // Sleep until something that matters changes.
        loop {
            match ch.next(stop, None) {
                Next::Stop | Next::Failed => return,
                Next::Quiet => {}
                Next::Paths(paths) => {
                    if paths.iter().any(|p| files::relevant(p, &ignored)) {
                        break;
                    }
                }
                Next::Overflow => break,
            }
        }
        // Then let the burst finish.
        let settling = Instant::now();
        while settling.elapsed() < MAX_SETTLE {
            let wait = SETTLE.max(MIN_GAP.saturating_sub(started.elapsed()));
            match ch.next(stop, Some(wait)) {
                Next::Stop | Next::Failed => return,
                Next::Quiet => break,
                Next::Paths(_) | Next::Overflow => {}
            }
        }
    }
}

/// The tree, and what git ignores, which the watcher skips.
fn scan(dir: &Path, prefix: &str) -> Option<(Tree, HashSet<String>)> {
    let listed = git(
        dir,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let status = git(
        dir,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
        ],
    )?;
    let ignored = git(
        dir,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    )
    .unwrap_or_default();
    let tree = Tree::build(
        &files::parse_list(&listed),
        &files::parse_status(&status, prefix),
    );
    Some((tree, files::parse_list(&ignored).into_iter().collect()))
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// Opens a file of the project in VS Code, in the window that has the
/// project open, or starts one with it. Without VS Code, whatever Windows
/// opens the file with.
pub fn open(root: &Path, rel: &str) {
    let file = root.join(rel.replace('/', "\\"));
    if let Some(code) = vs_code() {
        let spawned = Command::new(code)
            .arg(root)
            .arg(&file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(_) => return,
            Err(e) => eprintln!("horadric: cannot start VS Code: {e}"),
        }
    }
    let wide: Vec<u16> = file.as_os_str().encode_wide().chain([0]).collect();
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// Opens the project folder in VS Code, in the window that already has it
/// or a new one. False when there is no VS Code to open it with.
pub fn open_in_code(root: &Path) -> bool {
    let Some(code) = vs_code() else {
        return false;
    };
    let spawned = Command::new(code)
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(e) = &spawned {
        eprintln!("horadric: cannot start VS Code: {e}");
    }
    spawned.is_ok()
}

/// Opens the project folder in Explorer.
pub fn explore(root: &Path) {
    let wide: Vec<u16> = root.as_os_str().encode_wide().chain([0]).collect();
    unsafe {
        ShellExecuteW(
            None,
            w!("explore"),
            PCWSTR(wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// `Code.exe` beside the `bin\code.cmd` on `PATH`. The executable itself,
/// not the script, so no console flashes and no `cmd.exe` quoting applies.
pub fn vs_code() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        if !dir.join("code.cmd").is_file() {
            return None;
        }
        let exe = dir.parent()?.join("Code.exe");
        exe.is_file().then_some(exe)
    })
}

/// A manual reset event, closed when the last side lets go of it.
struct Event(HANDLE);

// A kernel handle, usable from any thread.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    fn new() -> Option<Self> {
        unsafe { CreateEventW(None, true, false, None) }
            .ok()
            .map(Event)
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

enum Next {
    Stop,
    /// The wait ran out with nothing new.
    Quiet,
    /// Changed paths, relative to the watched folder.
    Paths(Vec<String>),
    /// More changed than fit the buffer. Anything may have.
    Overflow,
    Failed,
}

/// `ReadDirectoryChangesW` on a whole folder tree, one read always pending
/// so nothing is missed between two waits.
struct DirChanges {
    dir: HANDLE,
    done: Event,
    // Boxed: the kernel writes to both while a read is pending, so they must
    // not move.
    overlapped: Box<OVERLAPPED>,
    buf: Box<[u32; 16 * 1024]>,
    pending: bool,
}

impl DirChanges {
    fn open(dir: &Path) -> Option<Self> {
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .ok()?;
        let done = match Event::new() {
            Some(e) => e,
            None => {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return None;
            }
        };
        Some(DirChanges {
            dir: handle,
            overlapped: Box::new(OVERLAPPED {
                hEvent: done.0,
                ..Default::default()
            }),
            done,
            buf: Box::new([0; 16 * 1024]),
            pending: false,
        })
    }

    /// Waits for the next batch of changes, `stop`, or `timeout`.
    fn next(&mut self, stop: &Event, timeout: Option<Duration>) -> Next {
        if !self.pending {
            let started = unsafe {
                ReadDirectoryChangesW(
                    self.dir,
                    self.buf.as_mut_ptr() as *mut c_void,
                    std::mem::size_of_val(&*self.buf) as u32,
                    true,
                    FILE_NOTIFY_CHANGE_FILE_NAME
                        | FILE_NOTIFY_CHANGE_DIR_NAME
                        | FILE_NOTIFY_CHANGE_LAST_WRITE,
                    None,
                    Some(&mut *self.overlapped),
                    None,
                )
            };
            if started.is_err() {
                return Next::Failed;
            }
            self.pending = true;
        }
        let ms = timeout.map_or(u32::MAX, |t| t.as_millis() as u32);
        let woke = unsafe { WaitForMultipleObjects(&[self.done.0, stop.0], false, ms) };
        if woke.0 == WAIT_OBJECT_0.0 + 1 {
            return Next::Stop;
        }
        if woke != WAIT_OBJECT_0 {
            return Next::Quiet;
        }
        self.pending = false;
        let mut bytes = 0u32;
        if unsafe { GetOverlappedResult(self.dir, &*self.overlapped, &mut bytes, false) }.is_err() {
            return Next::Failed;
        }
        if bytes == 0 {
            return Next::Overflow;
        }
        Next::Paths(notifications(&self.buf[..], bytes as usize))
    }
}

impl Drop for DirChanges {
    fn drop(&mut self) {
        unsafe {
            if self.pending {
                // The read must be over before its buffer goes.
                let _ = CancelIoEx(self.dir, Some(&*self.overlapped));
                let mut bytes = 0u32;
                let _ = GetOverlappedResult(self.dir, &*self.overlapped, &mut bytes, true);
            }
            let _ = CloseHandle(self.dir);
        }
    }
}

/// The paths in a buffer of `FILE_NOTIFY_INFORMATION` records.
fn notifications(buf: &[u32], len: usize) -> Vec<String> {
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, len) };
    let word = |at: usize| -> usize {
        bytes
            .get(at..at + 4)
            .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let mut out = Vec::new();
    let mut at = 0;
    while at + 12 <= len {
        let next = word(at);
        let name_len = word(at + 8);
        let name = bytes.get(at + 12..at + 12 + name_len).unwrap_or(&[]);
        let units: Vec<u16> = name
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        out.push(String::from_utf16_lossy(&units));
        if next == 0 {
            break;
        }
        at += next;
    }
    out
}
