//! The watcher: the process that follows the window in front and
//! writes what it shows to the trail (`horadric companion`).
//!
//! It hears when the window in front changes and when its title does,
//! writes that at once, and reads the window's text when it has
//! settled. While the window stays in front it reads again only after
//! the human has typed or clicked, so a page that is only looked at is
//! read once.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::core::{w, BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, HANDLE, HWND, WAIT_ABANDONED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, OpenInputDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_SWITCHDESKTOP,
};
use windows::Win32::System::SystemInformation::{GetTickCount, GetTickCount64};
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, OpenProcess, QueryFullProcessImageNameW, SetPriorityClass,
    WaitForSingleObject, BELOW_NORMAL_PRIORITY_CLASS, INFINITE, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, FindWindowExW, GetForegroundWindow, GetMessageW, GetWindowTextW,
    GetWindowThreadProcessId, SetTimer, SystemParametersInfoW, CHILDID_SELF,
    EVENT_OBJECT_NAMECHANGE, EVENT_SYSTEM_FOREGROUND, MSG, OBJID_WINDOW,
    SPI_GETSCREENSAVERRUNNING, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_TIMER,
};

use super::{never, read, trail};

const USAGE: &str = "usage: horadric companion [--parent PID] | horadric companion peek";

/// How long a window or its title must be unchanged before it is read.
const SETTLE_MS: u64 = 1_500;

/// A window whose title never rests (a spinner, a clock) is read this
/// long after it came in front all the same.
const UNSETTLED_MOST_MS: u64 = 10_000;

/// While a window stays in front and the human works in it, it is read
/// again this often at most.
const AGAIN_MS: u64 = 20_000;

/// A reading waits for the human to stop this long, so it is not done
/// in the middle of a word.
const QUIET_MS: u64 = 2_000;

/// The least time between two readings of the window in front, however
/// its title changes.
const GAP_MS: u64 = 3_000;

/// How often the watcher looks at its clocks. The hooks tell it of
/// changes at once; this is only for what has settled since.
const TICK_MS: u32 = 500;

/// How often a second watcher asks whether the first has gone.
const ONLY_ONE_POLL_MS: u32 = 3_000;

/// Windows whose lines are remembered, so coming back to one does not
/// write it all again.
const WINDOWS_KEPT: usize = 6;

const PEEK_WAIT: Duration = Duration::from_secs(3);
const PEEK_CHARS: usize = 2_000;

/// Programs that are in front for a moment on the way to another: the
/// Start menu, search, the task switcher. They are no work.
const PASSING: [&str; 4] = [
    "startmenuexperiencehost.exe",
    "searchhost.exe",
    "searchapp.exe",
    "shellexperiencehost.exe",
];
const PASSING_TITLES: [&str; 2] = ["Task Switching", "Task View"];

/// Shows the window of a packaged app for the process that draws it.
const FRAME_HOST: &str = "applicationframehost.exe";

/// `horadric companion [--parent PID]` watches until it is stopped,
/// `horadric companion peek` prints once what it would read.
pub fn run(args: &[String]) -> Result<(), String> {
    // The agent's desk is never watched: what happens there is the
    // agent's doing, not the human's.
    if crate::desk::hand::in_desk() {
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "peek") {
        return peek();
    }
    if args.first().is_some_and(|a| a == "show") {
        return show(args.get(1));
    }
    let parent = parent_of(args)?;
    // SAFETY: the handle is this process's own and needs no closing.
    if let Err(e) = unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) } {
        eprintln!("horadric: the companion keeps its normal priority: {e}");
    }
    if let Some(pid) = parent {
        if !leave_with(pid)? {
            return Ok(());
        }
    }
    only_one()?;
    watch()
}

/// The app's pid from `--parent PID`, None when run by hand.
fn parent_of(args: &[String]) -> Result<Option<u32>, String> {
    match args {
        [] => Ok(None),
        [flag, pid] if flag == "--parent" => pid
            .parse::<u32>()
            .map(Some)
            .map_err(|_| format!("`{pid}` is no process id\n{USAGE}")),
        _ => Err(USAGE.into()),
    }
}

/// Ends this process when the app that started it ends, however that
/// happens. False when it has ended already.
fn leave_with(pid: u32) -> Result<bool, String> {
    // SAFETY: plain call; the handle is kept for the life of the thread
    // that waits on it.
    let parent = match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(h) => h.0 as isize,
        Err(e) if e.code() == ERROR_INVALID_PARAMETER.to_hresult() => return Ok(false),
        Err(e) => return Err(format!("cannot wait on Horadric, process {pid}: {e}")),
    };
    thread::Builder::new()
        .name("companion-parent".into())
        .spawn(move || {
            // SAFETY: the handle was opened above for this wait alone.
            unsafe { WaitForSingleObject(HANDLE(parent as *mut c_void), INFINITE) };
            std::process::exit(0);
        })
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// Waits until this is the only watcher of this Windows session, the
/// installed Horadric's and a dev instance's alike. It waits rather
/// than leaves: the app starts a watcher that left again and again.
fn only_one() -> Result<(), String> {
    // SAFETY: the name is a literal. The mutex is never closed: this
    // thread owns it until the process ends, which is what frees it.
    let mutex = unsafe { CreateMutexW(None, false, w!("Local\\HoradricCompanion")) }
        .map_err(|e| format!("cannot tell whether another companion watches: {e}"))?;
    loop {
        // SAFETY: the handle is the mutex made above.
        match unsafe { WaitForSingleObject(mutex, ONLY_ONE_POLL_MS) } {
            // Abandoned: the watcher that held it was killed.
            WAIT_OBJECT_0 | WAIT_ABANDONED => return Ok(()),
            WAIT_TIMEOUT => {}
            _ => return Err("waiting for the other companion to go failed".into()),
        }
    }
}

/// The window in front and what is known of it.
struct Front {
    hwnd: isize,
    program: String,
    title: String,
    /// Never watched, or nothing to write of it.
    skip: bool,
    /// When it came in front.
    since: u64,
    /// When it or its title last changed.
    changed: u64,
    /// A change has not been read yet.
    unread: bool,
    /// When it was last read, None when never.
    read_at: Option<u64>,
    /// The title the trail last has for it.
    titled: String,
}

#[derive(Default)]
struct State {
    front: Option<Front>,
    /// What was written of the last few windows, the latest last.
    seen: Vec<(isize, trail::Seen)>,
    /// The hook on the front program's names, to take off when another
    /// program comes in front.
    names: isize,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn watch() -> Result<(), String> {
    // SAFETY: the hook and the timer belong to this thread, which pumps
    // messages below for as long as the process lives.
    unsafe {
        // Out of context: the callback runs here between messages, and
        // nothing of Horadric is loaded into other programs.
        let hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        if hook.is_invalid() {
            // The timer still sees a new window, half a second late.
            eprintln!("horadric: the companion cannot hear the window in front change");
        }
        if SetTimer(None, 0, TICK_MS, None) == 0 {
            return Err("the companion could not set its timer".into());
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            if msg.message == WM_TIMER && msg.hwnd.is_invalid() {
                step();
            } else {
                DispatchMessageW(&msg);
            }
        }
    }
    Ok(())
}

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    _hwnd: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    // Names change all over a program: every label and list row. Only
    // the window's own name is its title.
    if event == EVENT_OBJECT_NAMECHANGE && (object != OBJID_WINDOW.0 || child != CHILDID_SELF as i32)
    {
        return;
    }
    step();
}

/// Looks at what is in front now and does what is due. Never panics
/// out: it is called from a callback Windows makes.
fn step() {
    let done = std::panic::catch_unwind(|| {
        STATE.with(|s| {
            // Held while a reading waits: an event that came meanwhile
            // is seen at the next tick instead.
            if let Ok(mut state) = s.try_borrow_mut() {
                state.step();
            }
        });
    });
    if done.is_err() {
        eprintln!("horadric: the companion's step panicked, it goes on");
    }
}

impl State {
    fn step(&mut self) {
        // SAFETY: plain calls that take nothing or a number.
        let (hwnd, now) = unsafe { (GetForegroundWindow(), GetTickCount64()) };
        if hwnd.is_invalid() {
            self.front = None;
            return;
        }
        let id = hwnd.0 as isize;
        let title = title_of(hwnd);
        match self.front.as_mut().filter(|f| f.hwnd == id) {
            Some(front) => {
                // A packaged app whose inner window was not there yet
                // is asked again until it says which app it is. Until
                // then it is skipped: the never list cannot tell it
                // from any other packaged app.
                let resolved = unresolved(&front.program) && {
                    let (_, program) = program_of(hwnd);
                    let found = !unresolved(&program);
                    if found {
                        front.program = program;
                    }
                    found
                };
                if resolved || front.title != title {
                    front.skip = skipped(&front.program, &title);
                    front.title = title;
                    front.changed = now;
                    front.unread = true;
                }
                if resolved && !front.skip {
                    // Late, but as for any window that comes in front.
                    write(&front.program, &front.title, "");
                    front.titled = front.title.clone();
                }
            }
            None => self.came_in_front(hwnd, title, now),
        }
        let Some(front) = self.front.as_ref().filter(|f| !f.skip) else {
            return;
        };
        let clock = Clock {
            now,
            since: front.since,
            changed: front.changed,
            unread: front.unread,
            read_at: front.read_at,
            input_at: last_input(now),
        };
        if due(&clock) && !away() {
            self.read(hwnd, now);
        }
    }

    fn came_in_front(&mut self, hwnd: HWND, title: String, now: u64) {
        let (pid, program) = program_of(hwnd);
        let skip = skipped(&program, &title);
        if !skip {
            write(&program, &title, "");
        }
        self.hear_names(pid);
        self.front = Some(Front {
            hwnd: hwnd.0 as isize,
            program,
            titled: title.clone(),
            title,
            skip,
            since: now,
            changed: now,
            unread: true,
            read_at: None,
        });
    }

    /// Hears the names of `pid` change and no other program's: heard
    /// for every program, each row that scrolls anywhere would wake
    /// this process.
    fn hear_names(&mut self, pid: u32) {
        // SAFETY: the hook taken off is one this thread put on.
        unsafe {
            if self.names != 0 {
                let _ = UnhookWinEvent(HWINEVENTHOOK(self.names as *mut c_void));
                self.names = 0;
            }
            if pid == 0 {
                return;
            }
            let hook = SetWinEventHook(
                EVENT_OBJECT_NAMECHANGE,
                EVENT_OBJECT_NAMECHANGE,
                None,
                Some(on_event),
                pid,
                0,
                WINEVENT_OUTOFCONTEXT,
            );
            if !hook.is_invalid() {
                self.names = hook.0 as isize;
            }
        }
    }

    fn read(&mut self, hwnd: HWND, now: u64) {
        let id = hwnd.0 as isize;
        let text = read::text_of(hwnd).unwrap_or_default();
        let Some(front) = self.front.as_mut() else {
            return;
        };
        // From before the reading: what the human did during it counts
        // as done since. Stamped whatever becomes of the text: a
        // reading that is dropped was still made, and a title that
        // never rests would otherwise have the window read at every
        // tick.
        front.read_at = Some(now);
        // The reading took a while. If another window came in front
        // meanwhile the text may be its, and it may be one that is
        // never watched: dropped, and the next step sees the change.
        // SAFETY: plain call.
        if unsafe { GetForegroundWindow() } != hwnd {
            return;
        }
        let title = title_of(hwnd);
        if title != front.title {
            // The title changed under the reading (a spinner, a
            // counter, a new tab): the text goes under the title the
            // window has now, unless that one is never watched.
            front.skip = skipped(&front.program, &title);
            front.title = title;
            if front.skip {
                return;
            }
        }
        front.unread = false;
        let seen = match self.seen.iter().position(|(h, _)| *h == id) {
            Some(i) => self.seen.remove(i).1,
            None => trail::Seen::default(),
        };
        self.seen.push((id, seen));
        if self.seen.len() > WINDOWS_KEPT {
            self.seen.remove(0);
        }
        let fresh = match self.seen.last_mut() {
            Some((_, seen)) => seen.fresh(&text),
            None => String::new(),
        };
        // A new title with nothing new under it (a tab seen before) is
        // still where the human went.
        if fresh.is_empty() && front.title == front.titled {
            return;
        }
        write(&front.program, &front.title, &fresh);
        front.titled = front.title.clone();
    }
}

/// The times a reading is decided by, in milliseconds since the boot.
struct Clock {
    now: u64,
    /// When the window came in front.
    since: u64,
    /// When it or its title last changed.
    changed: u64,
    /// A change has not been read yet.
    unread: bool,
    read_at: Option<u64>,
    /// When the human last typed or clicked, anywhere.
    input_at: u64,
}

/// Whether the window in front is read now.
///
/// The first time: once it has settled, or after a while when its title
/// never rests. After that only when the human has done something since
/// the last reading, so a window that changes by itself (a video's
/// clock, a spinner in a title) is not read over and over: on a new
/// title that has settled, or now and then while they work, when they
/// pause.
fn due(c: &Clock) -> bool {
    let settled = c.now.saturating_sub(c.changed) >= SETTLE_MS;
    let Some(read_at) = c.read_at else {
        return settled || c.now.saturating_sub(c.since) >= UNSETTLED_MOST_MS;
    };
    if c.input_at <= read_at {
        return false;
    }
    let since_read = c.now.saturating_sub(read_at);
    let new_title = c.unread && settled && since_read >= GAP_MS;
    let worked = since_read >= AGAIN_MS && c.now.saturating_sub(c.input_at) >= QUIET_MS;
    new_title || worked
}

/// Whether nothing of this window is written: it is never watched, it
/// is only passed through, or it cannot be told what it is.
fn skipped(program: &str, title: &str) -> bool {
    program.is_empty()
        || title.trim().is_empty()
        || unresolved(program)
        || passing(program, title)
        || never::never(program, title)
}

/// Whether `program` is only the host of a packaged app's window, the
/// app itself not known yet. Every such app looks the same then, the
/// ones the human never wants watched too.
pub(super) fn unresolved(program: &str) -> bool {
    program.eq_ignore_ascii_case(FRAME_HOST)
}

fn passing(program: &str, title: &str) -> bool {
    PASSING.iter().any(|p| p.eq_ignore_ascii_case(program)) || PASSING_TITLES.contains(&title)
}

fn write(program: &str, title: &str, text: &str) {
    let entry = trail::Entry {
        at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        program: program.to_string(),
        title: title.to_string(),
        text: text.to_string(),
    };
    if let Err(e) = trail::write(&entry) {
        eprintln!("horadric: the trail could not be written: {e}");
    }
}

/// When the human last typed or clicked, on the clock `now` is on.
fn last_input(now: u64) -> u64 {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: `info` is sized as the call asks and outlives it.
    unsafe {
        if !GetLastInputInfo(&mut info).as_bool() {
            // Not known: as if just now, so nothing is read too early.
            return now;
        }
        // Both on the 32 bit clock, which wraps every 49 days.
        now.saturating_sub(u64::from(GetTickCount().wrapping_sub(info.dwTime)))
    }
}

/// Whether the human is not at the screen: it is locked, Windows asks
/// for consent on its own desktop, or the screen saver runs.
fn away() -> bool {
    // SAFETY: the desktop opened is closed again, and `on` outlives the
    // call it is written by.
    unsafe {
        // The lock screen and the consent prompt are desktops this
        // process may not open.
        match OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_SWITCHDESKTOP) {
            Ok(desktop) => {
                let _ = CloseDesktop(desktop);
            }
            Err(_) => return true,
        }
        let mut on = BOOL(0);
        SystemParametersInfoW(
            SPI_GETSCREENSAVERRUNNING,
            0,
            Some(&mut on as *mut BOOL as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
            && on.as_bool()
    }
}

pub(super) fn title_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    // SAFETY: the buffer is this function's own. For another program's
    // window this reads the title Windows keeps and asks the program
    // nothing, so a hung one cannot hold it.
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

/// The process a window belongs to and its program's file name, empty
/// when it cannot be told. A packaged app whose inner window is not
/// there yet comes back as its host, see [`unresolved`].
pub(super) fn program_of(hwnd: HWND) -> (u32, String) {
    let mut pid = 0u32;
    // SAFETY: `pid` outlives the call.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let program = name_of(pid);
    if !unresolved(&program) {
        return (pid, program);
    }
    // A packaged app's window belongs to a host every such app shares.
    // The app itself is the process of the window inside it.
    let mut inner = 0u32;
    // SAFETY: plain calls; `inner` outlives the one that writes it.
    unsafe {
        let class = w!("Windows.UI.Core.CoreWindow");
        if let Ok(core) = FindWindowExW(Some(hwnd), None, class, PCWSTR::null()) {
            GetWindowThreadProcessId(core, Some(&mut inner));
        }
    }
    let app = name_of(inner);
    // The title still changes on the host's window, so its names are
    // the ones to hear.
    (pid, if app.is_empty() { program } else { app })
}

fn name_of(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    // SAFETY: the handle is closed before returning, and the buffer and
    // its length are this function's own.
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let named = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        if named.is_err() {
            return String::new();
        }
        file_name(&String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `horadric companion show [MINUTES]`: what the trail holds of the last
/// minutes, so the human can see what is kept of their work.
fn show(minutes: Option<&String>) -> Result<(), String> {
    let minutes = match minutes {
        Some(m) => m.parse().map_err(|_| format!("{m} is not a number of minutes"))?,
        None => 15,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let seen = super::trail::recent(minutes, now);
    if seen.is_empty() {
        println!("The trail has nothing for the last {minutes} minutes.");
    } else {
        println!("{}", super::trail::render(&seen, now, usize::MAX));
    }
    Ok(())
}

/// `horadric companion peek`: what UI Automation gives for the window
/// in front, and what it costs. Writes nothing to the trail.
fn peek() -> Result<(), String> {
    println!(
        "Bring the window to read in front. Reading in {} seconds.",
        PEEK_WAIT.as_secs()
    );
    thread::sleep(PEEK_WAIT);
    // SAFETY: plain call.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return Err("no window is in front".into());
    }
    let (_, program) = program_of(hwnd);
    let title = title_of(hwnd);
    println!("program: {program}");
    println!("title:   {title}");
    // Horadric's own windows are read here though the watcher never
    // does: how much the stage gives is one of the things to measure.
    if !never::own(&program) && never::never(&program, &title) {
        println!("never watched, so nothing was read");
        return Ok(());
    }
    if never::own(&program) {
        println!("note:    Horadric's own window, which the watcher never reads");
    }
    if away() {
        println!("note:    the screen is locked or saved, the watcher would not read now");
    }
    let start = Instant::now();
    let got = read::read(hwnd);
    let took = start.elapsed().as_millis();
    match got {
        Ok(text) => {
            println!("chars:   {}", text.chars().count());
            println!("took:    {took} ms");
            println!();
            println!("{}", text.chars().take(PEEK_CHARS).collect::<String>());
        }
        Err(miss) => {
            println!("chars:   0 ({miss})");
            println!("took:    {took} ms");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(now: u64) -> Clock {
        Clock {
            now,
            since: 0,
            changed: 0,
            unread: true,
            read_at: None,
            input_at: 0,
        }
    }

    #[test]
    fn a_new_window_is_read_once_it_has_settled() {
        assert!(!due(&clock(SETTLE_MS - 1)));
        assert!(due(&clock(SETTLE_MS)));
        // Its title changed again a moment ago.
        let restless = Clock {
            changed: 4_000,
            ..clock(4_500)
        };
        assert!(!due(&restless));
    }

    #[test]
    fn a_title_that_never_rests_is_read_after_a_while() {
        let spinning = |now| Clock {
            changed: now - 100,
            ..clock(now)
        };
        assert!(!due(&spinning(UNSETTLED_MOST_MS - 1)));
        assert!(due(&spinning(UNSETTLED_MOST_MS)));
    }

    #[test]
    fn a_window_only_looked_at_is_read_once() {
        let looked = Clock {
            unread: false,
            read_at: Some(2_000),
            input_at: 1_000,
            ..clock(10 * AGAIN_MS)
        };
        assert!(!due(&looked));
        // Not even when its title changes by itself, as a video's does.
        assert!(!due(&Clock {
            unread: true,
            changed: 5_000,
            ..looked
        }));
    }

    #[test]
    fn work_is_read_now_and_then_when_the_human_pauses() {
        let read_at = 2_000;
        let working = |now, input_at| Clock {
            unread: false,
            read_at: Some(read_at),
            input_at,
            ..clock(now)
        };
        // Typed since, but the last reading is too fresh.
        assert!(!due(&working(read_at + AGAIN_MS - 1, read_at + 5_000)));
        // Long enough, and quiet for two seconds.
        assert!(due(&working(read_at + AGAIN_MS, read_at + 5_000)));
        // Long enough, but still typing.
        let now = read_at + AGAIN_MS + 5_000;
        assert!(!due(&working(now, now - QUIET_MS + 1)));
        assert!(due(&working(now, now - QUIET_MS)));
    }

    #[test]
    fn a_new_tab_is_read_when_its_title_has_settled() {
        let read_at = 2_000;
        let clicked = read_at + 4_000;
        let tab = |now| Clock {
            changed: clicked + 100,
            unread: true,
            read_at: Some(read_at),
            input_at: clicked,
            ..clock(now)
        };
        assert!(!due(&tab(clicked + 100 + SETTLE_MS - 1)));
        assert!(due(&tab(clicked + 100 + SETTLE_MS)));
        // Tabs flicked through are not each read: one reading, a gap.
        let flicked = Clock {
            changed: read_at + 200,
            unread: true,
            read_at: Some(read_at),
            input_at: read_at + 100,
            ..clock(read_at + 200 + SETTLE_MS)
        };
        assert!(!due(&flicked));
        assert!(due(&Clock {
            now: read_at + GAP_MS,
            ..flicked
        }));
    }

    #[test]
    fn a_reading_that_was_dropped_is_not_made_again_at_the_next_tick() {
        // The title changed under the reading, which stamps it all the
        // same: read at 20 s, a spinner still turning in the title.
        let read_at = 20_000;
        let spinning = |now, input_at| Clock {
            changed: now - 100,
            unread: true,
            read_at: Some(read_at),
            input_at,
            ..clock(now)
        };
        // Nothing done since: not at the next tick, and not ever.
        assert!(!due(&spinning(read_at + u64::from(TICK_MS), read_at - 1_000)));
        assert!(!due(&spinning(read_at + 10 * AGAIN_MS, read_at - 1_000)));
        // The human works on: again only as often as any window.
        let typed = read_at + 200;
        assert!(!due(&spinning(read_at + u64::from(TICK_MS), typed)));
        assert!(!due(&spinning(read_at + AGAIN_MS - 1, typed)));
        assert!(due(&spinning(read_at + AGAIN_MS, typed)));
    }

    #[test]
    fn a_packaged_app_not_yet_known_is_skipped() {
        assert!(unresolved("ApplicationFrameHost.exe"));
        assert!(unresolved("applicationframehost.exe"));
        assert!(!unresolved("olk.exe"));
        assert!(!unresolved(""));
        assert!(skipped("ApplicationFrameHost.exe", "Mail"));
    }

    #[test]
    fn the_parent_is_named_or_absent() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parent_of(&[]), Ok(None));
        assert_eq!(parent_of(&args(&["--parent", "4321"])), Ok(Some(4321)));
        assert!(parent_of(&args(&["--parent", "x"])).is_err());
        assert!(parent_of(&args(&["--parent"])).is_err());
        assert!(parent_of(&args(&["watch"])).is_err());
    }

    #[test]
    fn passing_windows_are_no_work() {
        assert!(passing("SearchHost.exe", "Search"));
        assert!(passing("explorer.exe", "Task Switching"));
        assert!(!passing("explorer.exe", "Documents"));
        assert_eq!(file_name(r"C:\Program Files\App\App.exe"), "App.exe");
        assert_eq!(file_name(""), "");
    }
}
