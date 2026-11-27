//! The app's hold on the watcher, the process that writes the trail
//! (`horadric companion --parent <pid>`): started when the companion is
//! turned on, ended when it is turned off or paused, and started again
//! with a growing wait whenever it ends by itself, as a node's link is
//! ([`crate::far_node`]).
//!
//! On is the human's switch in Settings, paused is the pause key's and
//! the tray's. Both are kept in the state file: a reload or a restart
//! that forgot the pause would start watching what it was paused for.

use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horadric_core::nodes::Backoff;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

use crate::menu::Item;

/// So the watcher, a console program, opens no console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Read by the tray, the settings and the calls that ask the trail,
/// which need not be one thread.
static ON: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);
static WATCHER: Mutex<Option<Watcher>> = Mutex::new(None);

/// The tray menu's ids for the companion, between takeover's and the
/// desk's.
const TRAY: usize = 9850;
const TRAY_OFF: usize = TRAY;
const TRAY_WATCHING: usize = TRAY + 1;
const TRAY_PAUSED: usize = TRAY + 2;

/// Whether the human turned the companion on.
pub fn is_on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Whether it is on and paused.
pub fn paused() -> bool {
    is_on() && PAUSED.load(Ordering::Relaxed)
}

/// Whether the watcher is meant to run now.
pub fn watching() -> bool {
    is_on() && !PAUSED.load(Ordering::Relaxed)
}

/// The switch in Settings. Turned on, it watches at once: a pause does
/// not outlive off.
pub fn set_on(on: bool) {
    ON.store(on, Ordering::Relaxed);
    PAUSED.store(false, Ordering::Relaxed);
    sync();
    if !on {
        // Nothing is written from here on, so nothing else would.
        prune_soon();
    }
}

/// What the state file said at the app's start: on or off, and still
/// paused if it was.
pub fn restore(on: bool, paused: bool) {
    ON.store(on, Ordering::Relaxed);
    PAUSED.store(on && paused, Ordering::Relaxed);
    sync();
    // Off or paused, days still grow older than the trail keeps them.
    prune_soon();
}

/// Deletes the days the trail no longer keeps, off the thread that
/// draws: it lists a folder and deletes files.
fn prune_soon() {
    let pruning = std::thread::Builder::new()
        .name("trail-prune".into())
        .spawn(|| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            super::trail::prune(now);
        });
    if pruning.is_err() {
        eprintln!("horadric: no thread to prune the trail on");
    }
}

/// The pause key: stops it watching, or lets it watch again. Says
/// whether it is paused now. Nothing to pause while it is off.
pub fn toggle_pause() -> bool {
    if !is_on() {
        return false;
    }
    let paused = !PAUSED.load(Ordering::Relaxed);
    PAUSED.store(paused, Ordering::Relaxed);
    sync();
    paused
}

/// Ends the watcher as the app ends. It would leave by itself once it
/// saw the app gone; this does not wait for it to look.
pub fn stop() {
    *WATCHER.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// What the tray's tip adds while the companion is on, so a glance says
/// whether it watches.
pub fn tip() -> Option<&'static str> {
    match (is_on(), paused()) {
        (false, _) => None,
        (true, false) => Some("companion watching"),
        (true, true) => Some("companion paused"),
    }
}

/// The tray menu's line for the companion, with what it does now ticked.
pub(crate) fn tray_item() -> Item {
    let line = |id, label: &str, checked| Item::Action {
        id,
        label: label.into(),
        checked,
        bold: false,
    };
    Item::Submenu(
        "Companion".into(),
        vec![
            line(TRAY_OFF, "Off", !is_on()),
            line(TRAY_WATCHING, "Watching", watching()),
            line(TRAY_PAUSED, "Paused", paused()),
        ],
    )
}

/// Whether a pick from the tray menu is one of the companion's lines.
pub(crate) fn tray_owns(id: usize) -> bool {
    matches!(id, TRAY_OFF | TRAY_WATCHING | TRAY_PAUSED)
}

/// Does what a line of [`tray_item`] says. The app saves after it, since
/// on and off are kept.
pub(crate) fn tray_pick(id: usize) {
    match id {
        TRAY_OFF => set_on(false),
        TRAY_WATCHING => set_on(true),
        TRAY_PAUSED => {
            ON.store(true, Ordering::Relaxed);
            PAUSED.store(true, Ordering::Relaxed);
            sync();
        }
        _ => {}
    }
}

/// Starts or ends the watcher so it runs exactly while [`watching`].
fn sync() {
    let mut w = WATCHER.lock().unwrap_or_else(|e| e.into_inner());
    match (watching(), w.is_some()) {
        (true, false) => *w = Watcher::start(),
        (false, true) => *w = None,
        _ => {}
    }
}

type Shared<T> = Arc<Mutex<Option<T>>>;

/// The watcher and the thread that keeps it running. Dropping it ends
/// both.
struct Watcher {
    stop: Arc<AtomicBool>,
    child: Shared<Child>,
}

impl Watcher {
    fn start() -> Option<Watcher> {
        // The watcher leaves at once inside the agent's desk, which is
        // never watched. Started there it would be started again and
        // again for nothing.
        if crate::desk::hand::in_desk() {
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let child: Shared<Child> = Arc::default();
        let (s, c) = (Arc::clone(&stop), Arc::clone(&child));
        std::thread::Builder::new()
            .name("companion-link".into())
            .spawn(move || keep_running(&s, &c))
            .ok()?;
        Some(Watcher { stop, child })
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(c) = self
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            let _ = c.kill();
        }
    }
}

fn keep_running(stop: &AtomicBool, child: &Mutex<Option<Child>>) {
    let debug = std::env::var_os("HORADRIC_DEBUG").is_some();
    let mut backoff = Backoff::default();
    while !stop.load(Ordering::SeqCst) {
        let began = Instant::now();
        // From the hosts' copy, not the app's own binary: a watcher that
        // is slow to leave would hold the file `reload` moves aside and
        // a dev build writes over.
        let spawned = Command::new(crate::console::host_program())
            .args(["companion", "--parent", &std::process::id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
        match spawned {
            Ok(c) => {
                adopt(&c);
                *child.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
                let code = wait(stop, child);
                if debug {
                    eprintln!("horadric: the companion's watcher ended ({code:?})");
                }
            }
            Err(e) => eprintln!("horadric: cannot start the companion's watcher: {e}"),
        }
        // One that leaves at once could not start. The wait grows, so
        // trying again costs next to nothing.
        let until = Instant::now() + backoff.after(began.elapsed());
        while Instant::now() < until {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Waits for the watcher to end, ending it when `stop` is set. The lock
/// is taken only to look, so a drop can always reach the child.
fn wait(stop: &AtomicBool, child: &Mutex<Option<Child>>) -> Option<i32> {
    loop {
        {
            let mut held = child.lock().unwrap_or_else(|e| e.into_inner());
            let c = held.as_mut()?;
            if stop.load(Ordering::SeqCst) {
                let _ = c.kill();
                let code = c.wait().ok().and_then(|s| s.code());
                *held = None;
                return code;
            }
            if let Ok(Some(status)) = c.try_wait() {
                *held = None;
                return status.code();
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Puts the watcher in a job that ends with the app, so one that never
/// looks whether the app is gone, hung in a call into another program,
/// does not outlive a crash.
fn adopt(child: &Child) {
    static JOB: OnceLock<Option<OwnedHandle>> = OnceLock::new();
    let job = JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(None, None).ok()?;
        let job = OwnedHandle::from_raw_handle(job.0);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            HANDLE(job.as_raw_handle()),
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const c_void,
            std::mem::size_of_val(&limits) as u32,
        )
        .ok()?;
        Some(job)
    });
    if let Some(job) = job {
        unsafe {
            let _ = AssignProcessToJobObject(
                HANDLE(job.as_raw_handle()),
                HANDLE(child.as_raw_handle()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_ticks_one_line_and_owns_only_its_own() {
        for id in [TRAY_OFF, TRAY_WATCHING, TRAY_PAUSED] {
            assert!(tray_owns(id));
        }
        // Takeover's lines and the desk's are on either side.
        assert!(!tray_owns(9801));
        assert!(!tray_owns(9900));
        let Item::Submenu(_, lines) = tray_item() else {
            panic!("a submenu");
        };
        let ticked = lines
            .iter()
            .filter(|l| matches!(l, Item::Action { checked: true, .. }))
            .count();
        assert_eq!(ticked, 1);
    }
}
