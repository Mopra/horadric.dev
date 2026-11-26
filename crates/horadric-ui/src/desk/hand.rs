//! The hand: Horadric's helper inside the agent's desk. It serves that
//! session's screen and takes input for it, for the `desk_` tools.
//!
//! It is the screen share's server (`crate::screen_share`) on the desk's
//! own port and token, with two routes more: `GET /apps` and
//! `POST /launch`. At start it stops the autostart programs that came up
//! on the desk with it (`hand_manners.rs`).

#[path = "hand_apps.rs"]
mod apps;
#[path = "hand_manners.rs"]
mod manners;

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use horadric_core::{desk, desk_apps};
use windows::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId};
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

/// Whether this process found itself in the desk when it started to
/// serve. What starts and stops programs asks this, never the override.
static IN_DESK: AtomicBool = AtomicBool::new(false);

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

/// Whether the desk's window named `session` as the desk. A session that
/// is not the console's can be an ordinary Remote Desktop login too.
fn named(session: u32) -> bool {
    horadric_hooks::state_dir()
        .and_then(|dir| std::fs::read_to_string(dir.join(desk::SESSION_FILE)).ok())
        .and_then(|text| desk::read_session(&text))
        == Some(session)
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
    // The desk signs in with the user's environment, which a dev
    // instance's variable is not part of, so its window can say it on
    // the command line. Set before the first thread is started.
    if std::env::args().any(|a| a == "--dev") && !horadric_hooks::dev() {
        std::env::set_var("HORADRIC_DEV", "1");
    }
    let session = wait_for_desk();
    if session.is_none() && !anywhere() {
        return Err(
            "this is not the agent's desk: the hand runs only in the session the desk's window opened"
                .into(),
        );
    }
    IN_DESK.store(session.is_some(), Ordering::Relaxed);
    // As the app does at its start: without it Windows scales what this
    // process is told of the screen, and a click lands beside its pixel.
    // SAFETY: takes a constant; a refusal leaves the default.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if let Some(session) = session {
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

fn open_apps(_: &str) -> Answer {
    json(desk_apps::apps_json(
        &apps::on_the_desk(),
        &apps::at_the_humans(),
    ))
}

fn launch(body: &str) -> Answer {
    // Outside the desk a start would land on the human's own desktop.
    if !IN_DESK.load(Ordering::Relaxed) {
        return text(
            "409 Conflict",
            "this hand is not in the desk, so it starts nothing".into(),
        );
    }
    let what = match desk_apps::parse_launch(body) {
        Ok(what) => what,
        Err(why) => return text("400 Bad Request", why),
    };
    match apps::launch(what) {
        Ok(started) => {
            screen_share::log(LOG_FILE, &format!("started {}", started.app));
            json(desk_apps::started_json(
                &started.app,
                started.instead_of,
                started.why.as_deref(),
            ))
        }
        Err(why) => text("409 Conflict", why),
    }
}
