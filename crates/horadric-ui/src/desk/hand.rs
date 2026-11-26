//! The hand: Horadric's helper inside the agent's desk. It serves that
//! session's screen and takes input for it, for the `desk_` tools.
//!
//! It is the screen share's server (`crate::screen_share`) on the desk's
//! own port and token, with two routes more: `GET /apps` and
//! `POST /launch`. At start it stops the autostart programs that came up
//! on the desk with it (`hand_manners.rs`).
//!
//! The desk's session stays signed in when its window closes, and the
//! hand with it. So the two routes ask at each request whether the
//! window still names this session, and do nothing while it does not.

#[path = "hand_apps.rs"]
mod apps;
#[path = "hand_manners.rs"]
mod manners;

use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horadric_core::{desk, desk_apps};
use windows::Win32::System::Console::FreeConsole;
use windows::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

use crate::screen_share::{self, Answer, Route, Serving};

/// The hand's audit log in the state folder: when it started, when it
/// was watched and driven, what it started and what it stopped.
const LOG_FILE: &str = "desk-hand.log";

/// How long the hand waits at its start for the window to name its
/// session in `desk::SESSION_FILE`. The window writes it a second or two
/// after it connects, and autostart comes later, but not surely.
const WAIT_FOR_DESK: Duration = Duration::from_secs(20);

/// A debug build serves outside the desk with this set to 1, to try the
/// server where there is no desk. Nothing is started or stopped then.
#[cfg(debug_assertions)]
const ANYWHERE_ENV: &str = "HORADRIC_DESK_HAND_ANYWHERE";

/// How much earlier than the boot a session file may seem written and
/// still count: the boot's time is reckoned from two clocks that are not
/// read at the same instant.
const BOOT_SLACK: Duration = Duration::from_secs(10);

/// What every route answers while the desk's window is closed.
const CLOSED: &str = "the agent's desk is closed, so its hand does nothing until the human \
     opens it again";

/// The desk's session, when this process found itself in the desk as it
/// started to serve. What starts and stops programs asks this, never the
/// override.
static DESK: AtomicU32 = AtomicU32::new(NO_DESK);

/// No session has this id, which is what Windows itself answers for none.
const NO_DESK: u32 = u32::MAX;

fn desk_session() -> Option<u32> {
    Some(DESK.load(Ordering::Relaxed)).filter(|s| *s != NO_DESK)
}

/// Whether this process runs inside the agent's desk, so a full Horadric
/// that autostarts there knows to leave.
pub fn in_desk() -> bool {
    // The session ids first: every start of the app asks this, and in
    // the human's own session that is all it costs.
    other_session().is_some_and(named)
}

/// This process's Windows session, when it is not the console's. Inside
/// a child session the console's id is the parent's, so the desk is told
/// from the human's own session by this alone.
fn other_session() -> Option<u32> {
    let mut own = 0u32;
    // SAFETY: `own` outlives the call, and the pid is this process's.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut own) }.ok()?;
    // SAFETY: takes nothing and hands a number back.
    let console = unsafe { WTSGetActiveConsoleSessionId() };
    (own != console).then_some(own)
}

/// Whether the desk's window names `session` as the desk, now. A session
/// that is not the console's can be an ordinary Remote Desktop login too.
///
/// The file must have been written since this boot. A crash leaves it
/// behind, Windows hands the same ids out again after a restart, and a
/// human signing in over Remote Desktop would then be taken for the
/// desk: their Horadric would never start.
fn named(session: u32) -> bool {
    let Some(file) = horadric_hooks::state_dir().map(|dir| dir.join(desk::SESSION_FILE)) else {
        return false;
    };
    let names = std::fs::read_to_string(&file)
        .ok()
        .and_then(|text| desk::read_session(&text));
    if names != Some(session) {
        return false;
    }
    let since_1970 = |time: SystemTime| time.duration_since(UNIX_EPOCH).ok();
    let modified = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(since_1970);
    // SAFETY: takes nothing and hands a number back.
    let up = Duration::from_millis(unsafe { GetTickCount64() });
    match (modified, since_1970(SystemTime::now())) {
        (Some(modified), Some(now)) => written_this_boot(modified, now, up),
        _ => false,
    }
}

/// Whether a file `modified` then was written since Windows started,
/// which was `up` before `now`. Both times count from the same epoch.
fn written_this_boot(modified: Duration, now: Duration, up: Duration) -> bool {
    modified + BOOT_SLACK >= now.saturating_sub(up)
}

/// The desk's session once the window has named it, None when this is
/// the console's session or the name did not come in time.
fn wait_for_desk() -> Option<u32> {
    let session = other_session()?;
    let until = Instant::now() + WAIT_FOR_DESK;
    loop {
        if named(session) {
            return Some(session);
        }
        if Instant::now() >= until {
            return None;
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn anywhere() -> bool {
    #[cfg(debug_assertions)]
    return std::env::var(ANYWHERE_ENV).is_ok_and(|v| v == "1");
    #[cfg(not(debug_assertions))]
    false
}

/// Serves the desk until its session ends. What `horadric desk hand` runs.
pub fn run() -> Result<(), String> {
    let session = wait_for_desk();
    if session.is_none() && !anywhere() {
        return Err(
            "this is not the agent's desk: the hand runs only in the session the desk's window opened"
                .into(),
        );
    }
    DESK.store(session.unwrap_or(NO_DESK), Ordering::Relaxed);
    // As the app does at its start: without it Windows scales what this
    // process is told of the screen, and a click lands beside its pixel.
    // SAFETY: takes a constant; a refusal leaves the default.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if let Some(session) = session {
        // Started by a RunOnce value, the command line program comes up
        // with a terminal window, which was seen on the desk and listed
        // by `/apps`. Let go of, it closes, having no other program.
        // SAFETY: takes nothing; without a console it only fails.
        let _ = unsafe { FreeConsole() };
        manners::start(session, LOG_FILE);
    }
    // No window and no message loop: the serving keeps the process
    // alive, and Windows ends it with the session.
    screen_share::serve_here(Serving {
        port: desk::port(horadric_hooks::dev()),
        token_file: desk::TOKEN_FILE,
        log_file: LOG_FILE,
        // No app runs on the desk to show a toast.
        tells: false,
        // The desk's size is its window's business.
        resizes: false,
        more: ROUTES,
        // A closed desk shows and takes nothing, see [`closed`].
        gate: Some(closed),
    })
}

const ROUTES: &[Route] = &[
    Route {
        method: "GET",
        path: "/apps",
        answer: open_apps,
    },
    Route {
        method: "POST",
        path: "/launch",
        answer: launch,
    },
];

fn text(status: &'static str, body: String) -> Answer {
    Answer {
        status,
        kind: "text/plain; charset=utf-8",
        body,
    }
}

fn json(body: String) -> Answer {
    Answer {
        status: "200 OK",
        kind: "application/json",
        body,
    }
}

/// The refusal while the desk's window is closed, None while it is open.
/// Asked at each request: the window closing leaves the session signed
/// in and this hand serving, and a closed desk is to do nothing.
fn closed() -> Option<Answer> {
    // The debug override serves where there is no desk to close.
    let session = desk_session()?;
    (!named(session)).then(|| text("409 Conflict", CLOSED.into()))
}

fn open_apps(_: &str) -> Answer {
    if let Some(refusal) = closed() {
        return refusal;
    }
    json(desk_apps::apps_json(
        &apps::on_the_desk(),
        &apps::at_the_humans(),
    ))
}

fn launch(body: &str) -> Answer {
    // Outside the desk a start would land on the human's own desktop.
    if desk_session().is_none() {
        return text(
            "409 Conflict",
            "this hand is not in the desk, so it starts nothing".into(),
        );
    }
    if let Some(refusal) = closed() {
        return refusal;
    }
    let what = match desk_apps::parse_launch(body) {
        Ok(what) => what,
        Err(why) => return text("400 Bad Request", why),
    };
    // Before anything starts: from here on a program on the desk may be
    // one somebody asked for.
    manners::used();
    match apps::launch(what) {
        Ok(started) => {
            screen_share::log(LOG_FILE, &format!("started {}", started.app));
            json(desk_apps::started_json(
                &started.app,
                started.instead_of,
                started.why.as_deref(),
                started.note,
            ))
        }
        Err(why) => text("409 Conflict", why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_a_session_file_counts_only_when_written_since_the_boot() {
        let secs = Duration::from_secs;
        let (now, up) = (secs(1_800_000_000), secs(3_600));
        let boot = now - up;
        assert!(written_this_boot(now, now, up));
        assert!(written_this_boot(boot + secs(60), now, up));
        // The two clocks are not read at one instant.
        assert!(written_this_boot(boot - secs(5), now, up));
        // Left behind by a crash before the restart.
        assert!(!written_this_boot(boot - secs(60), now, up));
        assert!(!written_this_boot(boot - secs(86_400), now, up));
        // A clock that says less than the time Windows has been up.
        assert!(written_this_boot(secs(1), secs(10), secs(60)));
    }

    #[test]
    fn hand_the_console_session_is_never_the_desk() {
        // The tests run in a session of the human's, or in none.
        if other_session().is_none() {
            assert!(!in_desk());
        }
        assert_eq!(desk_session(), None);
    }
}
